//! Debian dependency resolution.
//!
//! [`resolve`] takes every package a manifest asks for and picks one version
//! of each, plus everything they need, such that:
//!
//! - every `Pre-Depends` and `Depends` relation is satisfied by a selected
//!   package, by name or through `Provides`;
//! - each package is selected at most once;
//! - no selected package `Conflicts` with or `Breaks` another.
//!
//! Choices follow apt where it matters: a relation already satisfied by the
//! selection adds nothing, a real package beats a virtual one, and higher
//! versions beat lower ones. Alternatives (`a | b`) are tried in order.
//! Remaining ties are broken by priority, then name, then repository order,
//! so the same indexes always give the same answer.
//!
//! The search is greedy, with one kind of backtracking. When a relation needs
//! a different version of a package than the one already chosen, the
//! resolver remembers every requirement on that package and starts over,
//! choosing it with all of them applied. The result is always consistent,
//! but in contrived cases this can report a conflict that a full search
//! would get around, or settle for a lower version than necessary.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;

use thiserror::Error;

use super::architecture::Architecture;
use super::dependency::{DependencySpec, PackageDependency};
use super::package::DebianPackage;
use super::package_index_set::{PackageIndexSet, SourcedPackage};
use super::version::DebianVersion;
use crate::version::VersionConstraint;

/// A package the manifest asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub name: String,
    pub version: Option<VersionConstraint<DebianVersion>>,
    /// Only take the package from this repository.
    pub repository: Option<String>,
}

impl Request {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: None,
            repository: None,
        }
    }

    pub fn version(mut self, version: VersionConstraint<DebianVersion>) -> Self {
        self.version = Some(version);
        self
    }

    pub fn repository(mut self, repository: impl Into<String>) -> Self {
        self.repository = Some(repository.into());
        self
    }

    fn spec(&self) -> DependencySpec {
        DependencySpec {
            name: self.name.clone(),
            version: self.version.clone(),
        }
    }
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.spec())?;
        if let Some(repository) = &self.repository {
            write!(f, " from repository '{repository}'")?;
        }
        Ok(())
    }
}

/// Who asked for a package: the manifest, or another package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Manifest,
    Package(String),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Manifest => f.write_str("the manifest"),
            Origin::Package(name) => f.write_str(name),
        }
    }
}

/// A requirement on a package's version, and who made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub version: Option<VersionConstraint<DebianVersion>>,
    pub required_by: Origin,
}

impl Requirement {
    fn allows(&self, version: &DebianVersion) -> bool {
        self.version
            .as_ref()
            .is_none_or(|c| c.is_satisfied_by(version))
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(constraint) => write!(f, "({constraint})")?,
            None => f.write_str("any version")?,
        }
        write!(f, " from {}", self.required_by)
    }
}

/// A dependency nothing in the indexes satisfies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmetDependency {
    pub required_by: String,
    pub dependency: PackageDependency,
}

impl fmt::Display for UnmetDependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (required by {})", self.dependency, self.required_by)
    }
}

/// Errors that can occur during dependency resolution.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResolveError {
    #[error("no package matches '{request}' for {arch}")]
    NotFound {
        request: Box<Request>,
        arch: Architecture,
    },

    #[error(
        "'{name}' is a virtual package provided by {}; ask for one of them by name",
        providers.join(", ")
    )]
    AmbiguousVirtual {
        name: String,
        providers: Vec<String>,
    },

    #[error(
        "no version of '{name}' satisfies every requirement: {}",
        requirements.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
    )]
    Conflict {
        name: String,
        requirements: Vec<Requirement>,
    },

    #[error("'{package}' {field} '{relation}', but '{other}' is also needed")]
    Conflicts {
        package: String,
        field: &'static str,
        relation: Box<DependencySpec>,
        other: String,
    },
}

/// A selected package.
#[derive(Debug, Clone, Copy)]
pub struct Resolved<'a> {
    pub package: &'a DebianPackage,
    pub repository: &'a str,
    /// Asked for by the manifest, rather than pulled in by another package.
    pub direct: bool,
}

/// The outcome of [`resolve`].
#[derive(Debug)]
pub struct Resolution<'a> {
    /// Every selected package, sorted by name.
    pub packages: Vec<Resolved<'a>>,
    /// Each selected package's dependencies, as the sorted names of the
    /// selected packages that satisfy them.
    pub graph: BTreeMap<String, Vec<String>>,
    /// Dependencies of selected packages that nothing satisfies.
    pub unmet: Vec<UnmetDependency>,
}

/// Resolve `requests`, and everything they depend on, for `arch`.
///
/// Fails if a request matches no package, if requirements on a package
/// can't all be met, or if selected packages conflict. A dependency that
/// matches nothing at all is reported in [`Resolution::unmet`].
pub fn resolve<'a>(
    index: &'a PackageIndexSet,
    requests: &[Request],
    arch: Architecture,
) -> Result<Resolution<'a>, ResolveError> {
    let pins: HashMap<&str, &str> = requests
        .iter()
        .filter_map(|r| Some((r.name.as_str(), r.repository.as_deref()?)))
        .collect();
    let mut learned: HashMap<String, Vec<Requirement>> = HashMap::new();

    loop {
        let mut pass = Pass {
            index,
            arch,
            pins: &pins,
            learned: &learned,
            selected: BTreeMap::new(),
            provided: HashMap::new(),
            requirements: HashMap::new(),
            graph: BTreeMap::new(),
            unmet: Vec::new(),
        };
        match pass.run(requests)? {
            Outcome::Done => {
                let resolution = pass.finish();
                check_conflicts(&resolution)?;
                return Ok(resolution);
            }
            Outcome::Retry { name, requirements } => {
                let known = learned.entry(name.clone()).or_default();
                let before = known.len();
                for requirement in requirements {
                    if !known.contains(&requirement) {
                        known.push(requirement);
                    }
                }
                // Each retry must learn something, or we'd loop forever.
                if known.len() == before {
                    return Err(ResolveError::Conflict {
                        name,
                        requirements: known.clone(),
                    });
                }
            }
        }
    }
}

enum Outcome {
    Done,
    /// Start over, choosing `name` with these requirements applied.
    Retry {
        name: String,
        requirements: Vec<Requirement>,
    },
}

/// What [`Pass::choose`] found for one alternative.
enum Choice<'a> {
    Package(SourcedPackage<'a>),
    /// A different version of this package is already selected.
    Conflict(Vec<Requirement>),
    Nothing,
}

/// One greedy pass over the requests and their dependencies.
struct Pass<'a, 'p> {
    index: &'a PackageIndexSet,
    arch: Architecture,
    /// Package name → the only repository it may come from.
    pins: &'p HashMap<&'p str, &'p str>,
    /// Requirements remembered from earlier passes, by package name.
    learned: &'p HashMap<String, Vec<Requirement>>,

    selected: BTreeMap<&'a str, Resolved<'a>>,
    /// Virtual name → selected packages providing it, and what version.
    provided: HashMap<&'a str, Vec<(&'a str, Option<&'a DebianVersion>)>>,
    /// Requirements met so far in this pass, by package name.
    requirements: HashMap<String, Vec<Requirement>>,
    graph: BTreeMap<String, BTreeSet<String>>,
    unmet: Vec<UnmetDependency>,
}

impl<'a> Pass<'a, '_> {
    fn run(&mut self, requests: &[Request]) -> Result<Outcome, ResolveError> {
        // Breadth-first, so everything the manifest asks for is chosen
        // before any dependency gets a say.
        let mut queue: VecDeque<(Origin, PackageDependency)> = requests
            .iter()
            .map(|r| (Origin::Manifest, PackageDependency::Required(r.spec())))
            .collect();

        while let Some((origin, dependency)) = queue.pop_front() {
            let alternatives = match &dependency {
                PackageDependency::Required(spec) => std::slice::from_ref(spec),
                PackageDependency::Alternate(specs) => specs.as_slice(),
            };

            if let Some((spec, name)) = alternatives
                .iter()
                .find_map(|spec| Some((spec, self.satisfied_by(spec)?)))
            {
                self.note(spec, name, &origin);
                continue;
            }

            let mut conflict = None;
            let mut chosen = None;
            for spec in alternatives {
                match self.choose(spec, &origin)? {
                    Choice::Package(package) => {
                        chosen = Some((spec, package));
                        break;
                    }
                    Choice::Conflict(requirements) => {
                        conflict.get_or_insert((spec.name.clone(), requirements));
                    }
                    Choice::Nothing => {}
                }
            }

            match (chosen, conflict) {
                (Some((spec, package)), _) => {
                    self.select(package, spec, &origin);
                    let by = Origin::Package(package.package.name.clone());
                    queue.extend(
                        package
                            .package
                            .dependencies()
                            .map(|d| (by.clone(), d.clone())),
                    );
                }
                (None, Some((name, requirements))) => return self.retry(name, requirements),
                (None, None) => match origin {
                    Origin::Manifest => {
                        let request = requests
                            .iter()
                            .find(|r| r.spec() == alternatives[0])
                            .expect("manifest dependencies come from requests");
                        return Err(ResolveError::NotFound {
                            request: Box::new(request.clone()),
                            arch: self.arch,
                        });
                    }
                    Origin::Package(required_by) => self.unmet.push(UnmetDependency {
                        required_by,
                        dependency,
                    }),
                },
            }
        }

        Ok(Outcome::Done)
    }

    /// The name of a selected package that satisfies `spec`, if any.
    fn satisfied_by(&self, spec: &DependencySpec) -> Option<&'a str> {
        if let Some(selected) = self.selected.get(spec.name.as_str())
            && satisfies(spec, &selected.package.version)
        {
            return Some(selected.package.name.as_str());
        }
        self.provided
            .get(spec.name.as_str())?
            .iter()
            .find(|(_, version)| provides(spec, *version))
            .map(|(provider, _)| *provider)
    }

    /// Pick a package to satisfy `spec`, given that nothing selected does.
    fn choose(&self, spec: &DependencySpec, origin: &Origin) -> Result<Choice<'a>, ResolveError> {
        // A package by this name is selected, but the wrong version.
        if self.selected.contains_key(spec.name.as_str()) {
            let mut requirements = self.requirements_on(&spec.name);
            requirements.push(requirement(spec, origin));
            return Ok(Choice::Conflict(requirements));
        }

        if let Some(package) = self.best_package(&spec.name, &[requirement(spec, origin)]) {
            return Ok(Choice::Package(package));
        }

        // Providers, best first. One already selected would have satisfied
        // `spec`, so a selected provider name here is a different version.
        let mut providers: Vec<SourcedPackage<'a>> = self
            .index
            .providers(&spec.name, self.arch)
            .filter(|p| provides(spec, p.provided_version))
            .map(|p| p.provider)
            .filter(|p| !self.selected.contains_key(p.package.name.as_str()))
            .filter(|p| self.allowed(&spec.name, p) && self.allowed(&p.package.name, p))
            .collect();
        providers.sort_by(|a, b| {
            let (a, b) = (a.package, b.package);
            (b.priority.cmp(&a.priority))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| b.version.cmp(&a.version))
        });

        // apt refuses to guess which provider the user meant.
        if *origin == Origin::Manifest {
            let names: BTreeSet<&str> = providers.iter().map(|p| p.package.name.as_str()).collect();
            let has_real = self.index.packages(&spec.name, self.arch).next().is_some();
            if names.len() > 1 && !has_real {
                return Err(ResolveError::AmbiguousVirtual {
                    name: spec.name.clone(),
                    providers: names.into_iter().map(String::from).collect(),
                });
            }
        }

        Ok(providers
            .into_iter()
            .next()
            .map_or(Choice::Nothing, Choice::Package))
    }

    /// The best version of `name` that meets `requirements`, everything
    /// learned about `name`, and its pin: like apt, from the repository with
    /// the highest pin priority, then the highest version. Ties go to the
    /// earliest repository.
    fn best_package(&self, name: &str, requirements: &[Requirement]) -> Option<SourcedPackage<'a>> {
        let learned = self.learned.get(name).map_or(&[][..], Vec::as_slice);
        let mut best: Option<SourcedPackage<'a>> = None;
        for candidate in self.index.packages(name, self.arch) {
            let version = &candidate.package.version;
            if requirements
                .iter()
                .chain(learned)
                .all(|r| r.allows(version))
                && self.allowed(name, &candidate)
                && best.is_none_or(|b| {
                    (candidate.pin_priority, version) > (b.pin_priority, &b.package.version)
                })
            {
                best = Some(candidate);
            }
        }
        best
    }

    /// Whether `name`'s pin, if any, lets `candidate` be used for it.
    fn allowed(&self, name: &str, candidate: &SourcedPackage<'_>) -> bool {
        self.pins
            .get(name)
            .is_none_or(|repository| *repository == candidate.repository)
    }

    fn select(&mut self, package: SourcedPackage<'a>, spec: &DependencySpec, origin: &Origin) {
        let name = package.package.name.as_str();
        self.selected.insert(
            name,
            Resolved {
                package: package.package,
                repository: package.repository,
                direct: false,
            },
        );
        for provide in &package.package.provides {
            self.provided
                .entry(provide.name.as_str())
                .or_default()
                .push((name, provide.version.as_ref()));
        }
        self.note(spec, name, origin);
    }

    /// Record that `name`, already selected, satisfies `spec` for `origin`.
    fn note(&mut self, spec: &DependencySpec, name: &str, origin: &Origin) {
        if spec.name == name {
            self.requirements
                .entry(spec.name.clone())
                .or_default()
                .push(requirement(spec, origin));
        }
        match origin {
            Origin::Manifest => {
                if let Some(selected) = self.selected.get_mut(name) {
                    selected.direct = true;
                }
            }
            Origin::Package(parent) if parent != name => {
                self.graph
                    .entry(parent.clone())
                    .or_default()
                    .insert(name.to_string());
            }
            Origin::Package(_) => {}
        }
    }

    fn requirements_on(&self, name: &str) -> Vec<Requirement> {
        let learned = self.learned.get(name).into_iter().flatten();
        let seen = self.requirements.get(name).into_iter().flatten();
        let mut all: Vec<Requirement> = Vec::new();
        for requirement in learned.chain(seen) {
            if !all.contains(requirement) {
                all.push(requirement.clone());
            }
        }
        all
    }

    /// Start over with `requirements` on `name`, if some version meets them
    /// all; otherwise this is a real conflict.
    fn retry(&self, name: String, requirements: Vec<Requirement>) -> Result<Outcome, ResolveError> {
        if self.best_package(&name, &requirements).is_some() {
            Ok(Outcome::Retry { name, requirements })
        } else {
            Err(ResolveError::Conflict { name, requirements })
        }
    }

    fn finish(self) -> Resolution<'a> {
        Resolution {
            packages: self.selected.into_values().collect(),
            graph: self
                .graph
                .into_iter()
                .map(|(parent, children)| (parent, children.into_iter().collect()))
                .collect(),
            unmet: self.unmet,
        }
    }
}

fn requirement(spec: &DependencySpec, origin: &Origin) -> Requirement {
    Requirement {
        version: spec.version.clone(),
        required_by: origin.clone(),
    }
}

/// Whether a real package at `version` satisfies `spec`.
fn satisfies(spec: &DependencySpec, version: &DebianVersion) -> bool {
    spec.version
        .as_ref()
        .is_none_or(|c| c.is_satisfied_by(version))
}

/// Whether providing `spec.name` at `provided` satisfies `spec`. Policy
/// says an unversioned `Provides` never satisfies a versioned relation.
fn provides(spec: &DependencySpec, provided: Option<&DebianVersion>) -> bool {
    match (&spec.version, provided) {
        (None, _) => true,
        (Some(constraint), Some(version)) => constraint.is_satisfied_by(version),
        (Some(_), None) => false,
    }
}

/// Fail if a selected package `Conflicts` with or `Breaks` another one,
/// either by name or through what it provides. A package may conflict with
/// something it provides itself.
fn check_conflicts(resolution: &Resolution<'_>) -> Result<(), ResolveError> {
    for this in &resolution.packages {
        for (field, relation) in this.package.conflicts() {
            let other = resolution.packages.iter().find(|other| {
                let other = other.package;
                other.name != this.package.name
                    && ((other.name == relation.name && satisfies(relation, &other.version))
                        || other.provides.iter().any(|p| {
                            p.name == relation.name && provides(relation, p.version.as_ref())
                        }))
            });
            if let Some(other) = other {
                return Err(ResolveError::Conflicts {
                    package: this.package.name.clone(),
                    field,
                    relation: Box::new(relation.clone()),
                    other: other.package.name.clone(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::super::package_index::PackageIndex;
    use super::*;

    /// An index from `Packages`-style stanzas. `Architecture` defaults to
    /// amd64 and `Filename` is filled in.
    fn index(stanzas: &str) -> PackageIndex {
        let mut text = String::new();
        for stanza in stanzas
            .split("\n\n")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let stanza: String = stanza.lines().map(|l| format!("{}\n", l.trim())).collect();
            text.push_str(&stanza);
            if !stanza.contains("Architecture:") {
                text.push_str("Architecture: amd64\n");
            }
            text.push_str("Filename: pool/x.deb\n\n");
        }
        PackageIndex::from_str(&text).unwrap()
    }

    fn repos(repos: &[(&str, &str)]) -> PackageIndexSet {
        let mut set = PackageIndexSet::new();
        for (name, stanzas) in repos {
            set.add_index(name.to_string(), index(stanzas));
        }
        set
    }

    fn requests(names: &[&str]) -> Vec<Request> {
        names
            .iter()
            .map(|n| match n.split_once(' ') {
                Some((name, constraint)) => {
                    let constraint = constraint.trim_matches(['(', ')']);
                    Request::new(name).version(constraint.parse().unwrap())
                }
                None => Request::new(*n),
            })
            .collect()
    }

    fn run<'a>(
        set: &'a PackageIndexSet,
        requests: &[Request],
    ) -> Result<Resolution<'a>, ResolveError> {
        resolve(set, requests, Architecture::Amd64)
    }

    /// `name=version` for every selected package, sorted.
    fn selected(resolution: &Resolution<'_>) -> Vec<String> {
        resolution
            .packages
            .iter()
            .map(|r| format!("{}={}", r.package.name, r.package.version))
            .collect()
    }

    fn resolves_to(set: &PackageIndexSet, asked: &[&str]) -> Vec<String> {
        selected(&run(set, &requests(asked)).unwrap())
    }

    // ── basics ──────────────────────────────────────────────────────────

    #[test]
    fn resolves_transitive_dependencies() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libfoo\n\n\
             Package: libfoo\nVersion: 1\nPre-Depends: libbar\n\n\
             Package: libbar\nVersion: 1\n\n\
             Package: unrelated\nVersion: 1",
        )]);
        let resolution = run(&set, &requests(&["app"])).unwrap();
        assert_eq!(selected(&resolution), ["app=1", "libbar=1", "libfoo=1"]);
        assert!(resolution.unmet.is_empty());

        let direct: Vec<_> = resolution.packages.iter().map(|r| r.direct).collect();
        assert_eq!(direct, [true, false, false]);
        assert_eq!(resolution.packages[0].repository, "main");
    }

    #[test]
    fn records_the_graph_including_cycles() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libfoo\n\n\
             Package: libfoo\nVersion: 1\nDepends: libbar\n\n\
             Package: libbar\nVersion: 1\nDepends: libfoo",
        )]);
        let resolution = run(&set, &requests(&["app"])).unwrap();
        assert_eq!(selected(&resolution), ["app=1", "libbar=1", "libfoo=1"]);
        let graph: Vec<_> = resolution.graph.iter().collect();
        assert_eq!(
            format!("{graph:?}"),
            r#"[("app", ["libfoo"]), ("libbar", ["libfoo"]), ("libfoo", ["libbar"])]"#
        );
    }

    #[test]
    fn picks_the_highest_version_that_satisfies() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libfoo (<< 3)\n\n\
             Package: libfoo\nVersion: 1\n\n\
             Package: libfoo\nVersion: 2\n\n\
             Package: libfoo\nVersion: 3",
        )]);
        assert_eq!(resolves_to(&set, &["app"]), ["app=1", "libfoo=2"]);
        assert_eq!(resolves_to(&set, &["libfoo"]), ["libfoo=3"]);
    }

    #[test]
    fn falls_back_to_arch_all() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: data\n\n\
             Package: data\nVersion: 1\nArchitecture: all",
        )]);
        assert_eq!(resolves_to(&set, &["app"]), ["app=1", "data=1"]);
    }

    #[test]
    fn reports_unmet_dependencies() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libmissing (>= 1), libfoo (>= 2)\n\n\
             Package: libfoo\nVersion: 1",
        )]);
        let resolution = run(&set, &requests(&["app"])).unwrap();
        assert_eq!(selected(&resolution), ["app=1"]);
        let unmet: Vec<_> = resolution.unmet.iter().map(ToString::to_string).collect();
        assert_eq!(
            unmet,
            [
                "libmissing (>= 1) (required by app)",
                "libfoo (>= 2) (required by app)"
            ]
        );
    }

    #[test]
    fn fails_when_a_request_matches_nothing() {
        let set = repos(&[("main", "Package: app\nVersion: 1")]);
        let err = run(&set, &requests(&["nope"])).unwrap_err();
        assert_eq!(err.to_string(), "no package matches 'nope' for amd64");
        let err = run(&set, &requests(&["app (>= 2)"])).unwrap_err();
        assert_eq!(err.to_string(), "no package matches 'app (>= 2)' for amd64");
    }

    // ── alternatives ────────────────────────────────────────────────────

    #[test]
    fn alternatives_take_the_first_available() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libmissing | libbar | libfoo\n\n\
             Package: libfoo\nVersion: 1\n\n\
             Package: libbar\nVersion: 1",
        )]);
        assert_eq!(resolves_to(&set, &["app"]), ["app=1", "libbar=1"]);
    }

    #[test]
    fn alternatives_prefer_what_is_already_selected() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libfoo | libbar\n\n\
             Package: libfoo\nVersion: 1\n\n\
             Package: libbar\nVersion: 1",
        )]);
        assert_eq!(resolves_to(&set, &["libbar", "app"]), ["app=1", "libbar=1"]);
    }

    #[test]
    fn alternatives_skip_a_version_that_does_not_fit() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libfoo (>= 2) | libbar\n\n\
             Package: libfoo\nVersion: 1\n\n\
             Package: libbar\nVersion: 1",
        )]);
        assert_eq!(resolves_to(&set, &["app"]), ["app=1", "libbar=1"]);
    }

    // ── virtual packages ────────────────────────────────────────────────

    #[test]
    fn a_real_package_beats_a_virtual_one() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: awk\n\n\
             Package: mawk\nVersion: 1\nProvides: awk\nPriority: required\n\n\
             Package: awk\nVersion: 1",
        )]);
        assert_eq!(resolves_to(&set, &["app"]), ["app=1", "awk=1"]);
    }

    #[test]
    fn an_unversioned_provides_does_not_satisfy_a_versioned_dependency() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libssl (>= 3)\n\n\
             Package: fakessl\nVersion: 9\nProvides: libssl",
        )]);
        let resolution = run(&set, &requests(&["app"])).unwrap();
        assert_eq!(selected(&resolution), ["app=1"]);
        assert_eq!(resolution.unmet.len(), 1);
    }

    #[test]
    fn a_versioned_provides_satisfies_by_its_provided_version() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libssl (>= 3)\n\n\
             Package: old\nVersion: 9\nProvides: libssl (= 2)\n\n\
             Package: new\nVersion: 1\nProvides: libssl (= 3.1)",
        )]);
        assert_eq!(resolves_to(&set, &["app"]), ["app=1", "new=1"]);
    }

    #[test]
    fn providers_are_ranked_by_priority_with_none_last_then_name() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: awk\n\n\
             Package: aawk\nVersion: 1\nProvides: awk\n\n\
             Package: zawk\nVersion: 1\nProvides: awk\nPriority: optional\n\n\
             Package: mawk\nVersion: 1\nProvides: awk\nPriority: required\n\n\
             Package: gawk\nVersion: 1\nProvides: awk\nPriority: optional",
        )]);
        assert_eq!(resolves_to(&set, &["app"]), ["app=1", "mawk=1"]);

        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: awk\n\n\
             Package: aawk\nVersion: 1\nProvides: awk\n\n\
             Package: zawk\nVersion: 1\nProvides: awk\nPriority: optional\n\n\
             Package: gawk\nVersion: 1\nProvides: awk\nPriority: optional",
        )]);
        assert_eq!(resolves_to(&set, &["app"]), ["app=1", "gawk=1"]);
    }

    #[test]
    fn finds_arch_all_providers() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: debconf-2.0\n\n\
             Package: debconf\nVersion: 1\nArchitecture: all\nProvides: debconf-2.0",
        )]);
        assert_eq!(resolves_to(&set, &["app"]), ["app=1", "debconf=1"]);
    }

    #[test]
    fn a_selected_provider_satisfies_later_dependencies() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: awk\n\n\
             Package: mawk\nVersion: 1\nProvides: awk\nPriority: required\n\n\
             Package: gawk\nVersion: 1\nProvides: awk\nPriority: optional",
        )]);
        assert_eq!(resolves_to(&set, &["gawk", "app"]), ["app=1", "gawk=1"]);
    }

    #[test]
    fn a_request_for_an_ambiguous_virtual_package_fails() {
        let set = repos(&[(
            "main",
            "Package: mawk\nVersion: 1\nProvides: awk\n\n\
             Package: gawk\nVersion: 1\nProvides: awk",
        )]);
        assert_eq!(
            run(&set, &requests(&["awk"])).unwrap_err(),
            ResolveError::AmbiguousVirtual {
                name: "awk".into(),
                providers: vec!["gawk".into(), "mawk".into()],
            }
        );

        let set = repos(&[("main", "Package: mawk\nVersion: 1\nProvides: awk")]);
        let resolution = run(&set, &requests(&["awk"])).unwrap();
        assert_eq!(selected(&resolution), ["mawk=1"]);
        assert!(resolution.packages[0].direct);
    }

    // ── consistency ─────────────────────────────────────────────────────

    /// Each request used to be resolved alone, so `app` got libfoo 2 and
    /// `legacy`'s `<< 2` was silently lost when the results were merged.
    #[test]
    fn requirements_from_different_requests_are_combined() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libfoo\n\n\
             Package: legacy\nVersion: 1\nDepends: libfoo (<< 2)\n\n\
             Package: libfoo\nVersion: 1\n\n\
             Package: libfoo\nVersion: 2",
        )]);
        assert_eq!(
            resolves_to(&set, &["app", "legacy"]),
            ["app=1", "legacy=1", "libfoo=1"]
        );
    }

    #[test]
    fn requirements_deep_in_the_tree_are_combined() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libfoo, mid\n\n\
             Package: mid\nVersion: 1\nDepends: libfoo (<< 2)\n\n\
             Package: libfoo\nVersion: 1\nDepends: libone\n\n\
             Package: libfoo\nVersion: 2\nDepends: libtwo\n\n\
             Package: libone\nVersion: 1\n\n\
             Package: libtwo\nVersion: 1",
        )]);
        // libfoo 2's dependency is gone once libfoo 1 is chosen.
        assert_eq!(
            resolves_to(&set, &["app"]),
            ["app=1", "libfoo=1", "libone=1", "mid=1"]
        );
    }

    #[test]
    fn unsatisfiable_requirements_are_a_conflict() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libfoo (>= 2)\n\n\
             Package: legacy\nVersion: 1\nDepends: libfoo (<< 2)\n\n\
             Package: libfoo\nVersion: 1\n\n\
             Package: libfoo\nVersion: 2",
        )]);
        let err = run(&set, &requests(&["app", "legacy"])).unwrap_err();
        assert_eq!(
            err.to_string(),
            "no version of 'libfoo' satisfies every requirement: \
             (>= 2) from app, (<< 2) from legacy"
        );
    }

    #[test]
    fn a_requested_version_wins_or_conflicts() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libfoo (>= 1)\n\n\
             Package: libfoo\nVersion: 1\n\n\
             Package: libfoo\nVersion: 2",
        )]);
        assert_eq!(
            resolves_to(&set, &["app", "libfoo (= 1)"]),
            ["app=1", "libfoo=1"]
        );

        let err = run(&set, &requests(&["app", "libfoo (<< 1)"])).unwrap_err();
        assert!(matches!(
            err,
            ResolveError::Conflict { .. } | ResolveError::NotFound { .. }
        ));
    }

    // ── Conflicts and Breaks ────────────────────────────────────────────

    #[test]
    fn conflicting_packages_fail() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: libfoo, libbar\n\n\
             Package: libfoo\nVersion: 1\nConflicts: libbar\n\n\
             Package: libbar\nVersion: 1",
        )]);
        assert_eq!(
            run(&set, &requests(&["app"])).unwrap_err().to_string(),
            "'libfoo' Conflicts 'libbar', but 'libbar' is also needed"
        );
    }

    #[test]
    fn breaks_respect_versions_and_provides() {
        let stanzas = "Package: app\nVersion: 1\nDepends: libfoo, libbar\n\n\
                       Package: libbar\nVersion: 2\nProvides: libbaz (= 2)\n\n\
                       Package: libfoo\nVersion: 1\nBreaks: ";
        let breaks = |relation: &str| {
            let set = repos(&[("main", &format!("{stanzas}{relation}"))]);
            run(&set, &requests(&["app"])).map(|r| selected(&r))
        };
        assert!(breaks("libbar (<< 2)").is_ok());
        assert!(breaks("libbar (<< 3)").is_err());
        assert!(breaks("libbaz (<< 2)").is_ok());
        assert!(breaks("libbaz (<< 3)").is_err());
        assert!(breaks("libbaz").is_err());
    }

    #[test]
    fn a_package_may_conflict_with_what_it_provides() {
        let set = repos(&[(
            "main",
            "Package: mawk\nVersion: 1\nProvides: awk\nConflicts: awk",
        )]);
        assert_eq!(resolves_to(&set, &["mawk"]), ["mawk=1"]);
    }

    // ── repositories ────────────────────────────────────────────────────

    #[test]
    fn ties_go_to_the_earlier_repository() {
        let set = repos(&[
            ("first", "Package: bash\nVersion: 5"),
            ("second", "Package: bash\nVersion: 5"),
            ("third", "Package: bash\nVersion: 6"),
        ]);
        let resolution = run(&set, &requests(&["bash"])).unwrap();
        assert_eq!(resolution.packages[0].repository, "third");

        let set = repos(&[
            ("first", "Package: bash\nVersion: 5"),
            ("second", "Package: bash\nVersion: 5"),
        ]);
        let resolution = run(&set, &requests(&["bash"])).unwrap();
        assert_eq!(resolution.packages[0].repository, "first");
    }

    fn prioritized(repos: &[(&str, i32, &str)]) -> PackageIndexSet {
        let mut set = PackageIndexSet::new();
        for (name, priority, stanzas) in repos {
            set.add_index_with_priority(name.to_string(), *priority, index(stanzas));
        }
        set
    }

    /// PGDG's apt-archive carries every version PGDG ever published, betas
    /// included. At a low priority it only serves what PGDG no longer has.
    #[test]
    fn a_higher_priority_repository_beats_a_higher_version() {
        let set = prioritized(&[
            (
                "pgdg-archive",
                100,
                "Package: libpq5\nVersion: 19~beta4-1\n\n\
                 Package: libpq5\nVersion: 18.6-1\n\n\
                 Package: pgbackrest\nVersion: 2.58.0-1\nDepends: libpq5\n\n\
                 Package: pgbackrest\nVersion: 2.59.0-1\nDepends: libpq5",
            ),
            (
                "pgdg",
                500,
                "Package: libpq5\nVersion: 18.6-1\n\n\
                 Package: pgbackrest\nVersion: 2.59.0-1\nDepends: libpq5",
            ),
        ]);

        // Only the archive has 2.58.0, so it comes from there; libpq5 comes
        // from PGDG even though the archive has a higher version.
        let resolution = run(&set, &requests(&["pgbackrest (= 2.58.0-1)"])).unwrap();
        assert_eq!(
            selected(&resolution),
            ["libpq5=18.6-1", "pgbackrest=2.58.0-1"]
        );
        assert_eq!(resolution.packages[0].repository, "pgdg");
        assert_eq!(resolution.packages[1].repository, "pgdg-archive");

        // Same version in both: the higher priority wins, not the order.
        let resolution = run(&set, &requests(&["pgbackrest"])).unwrap();
        assert_eq!(
            selected(&resolution),
            ["libpq5=18.6-1", "pgbackrest=2.59.0-1"]
        );
        assert_eq!(resolution.packages[1].repository, "pgdg");
    }

    #[test]
    fn a_negative_priority_repository_is_never_used() {
        let set = prioritized(&[
            ("debian", 500, "Package: bash\nVersion: 5.2"),
            (
                "never",
                -1,
                "Package: bash\nVersion: 9.9\n\nPackage: zsh\nVersion: 5.9",
            ),
        ]);
        assert_eq!(resolves_to(&set, &["bash"]), ["bash=5.2"]);
        assert!(matches!(
            run(&set, &requests(&["zsh"])),
            Err(ResolveError::NotFound { .. })
        ));
    }

    /// A pin used to only relabel the package: it was still resolved from
    /// every repository, so its download URL could point at the wrong one.
    #[test]
    fn a_pinned_package_comes_from_its_repository() {
        let set = repos(&[
            (
                "debian",
                "Package: app\nVersion: 1\nDepends: libfoo\n\nPackage: libfoo\nVersion: 1",
            ),
            ("backports", "Package: libfoo\nVersion: 2"),
        ]);
        let pinned = [
            Request::new("app"),
            Request::new("libfoo").repository("debian"),
        ];
        let resolution = run(&set, &pinned).unwrap();
        assert_eq!(selected(&resolution), ["app=1", "libfoo=1"]);
        assert_eq!(resolution.packages[1].repository, "debian");

        let missing = [Request::new("app").repository("backports")];
        assert_eq!(
            run(&set, &missing).unwrap_err().to_string(),
            "no package matches 'app from repository 'backports'' for amd64"
        );
    }

    #[test]
    fn is_deterministic() {
        let set = repos(&[(
            "main",
            "Package: app\nVersion: 1\nDepends: a | b, c, awk\n\n\
             Package: a\nVersion: 1\nDepends: c (>= 1)\n\n\
             Package: c\nVersion: 1\n\n\
             Package: c\nVersion: 2\n\n\
             Package: x\nVersion: 1\nProvides: awk\n\n\
             Package: y\nVersion: 1\nProvides: awk",
        )]);
        let first = resolves_to(&set, &["app"]);
        for _ in 0..20 {
            assert_eq!(resolves_to(&set, &["app"]), first);
        }
        assert_eq!(first, ["a=1", "app=1", "c=2", "x=1"]);
    }
}
