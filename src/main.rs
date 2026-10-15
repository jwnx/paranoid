//! paranoid — deterministic, minimal OCI images from Debian packages.
//!
//! Subcommands:
//!   lock     Resolve dependencies and write paranoid.lock
//!   build    Download packages and build an OCI image from them
//!   inspect  Show the contents of a lock file or built image

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod cli {
    pub mod error;
    pub mod progress;
    pub mod style;
}

use cli::error::CliError;
use cli::progress::DotsReporter;
use cli::style as st;

use paranoid::build::{BuildOptions, build};
use paranoid::config::Manifest;
use paranoid::download::DownloadConfig;
use paranoid::lock::LockFile;
use paranoid::oci::{ImageOutput, Timestamp};
use paranoid::resolve::ResolveService;

// ── Top-level CLI ─────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "paranoid",
    version,
    about = "Build deterministic, minimal OCI images from Debian packages",
    long_about = None,
)]
struct Cli {
    /// Suppress all output except errors.
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Resolve dependencies and write paranoid.lock.
    Lock(LockArgs),

    /// Download packages, extract them, and build an OCI image.
    Build(BuildArgs),

    /// Show the contents of a lock file or image.
    Inspect(InspectArgs),
}

// ── lock ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
struct LockArgs {
    /// Path to the manifest file or directory containing one.
    #[arg(short = 'f', long, value_name = "FILE|DIR", default_value = ".")]
    manifest: PathBuf,

    /// Where to write the lock file.
    ///
    /// Defaults to `paranoid.lock` next to the manifest when -f points to a
    /// directory, or in the current directory otherwise.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Re-resolve even if the lock file is already up to date.
    #[arg(long)]
    force: bool,
}

// ── build ─────────────────────────────────────────────────────────────────────

#[derive(Parser)]
struct BuildArgs {
    /// Path to the manifest file or directory containing one.
    #[arg(short = 'f', long, value_name = "FILE|DIR", default_value = ".")]
    manifest: PathBuf,

    /// Lock file to use (must exist; run `paranoid lock` first).
    ///
    /// Defaults to `paranoid.lock` next to the manifest when -f points to a
    /// directory, or in the current directory otherwise.
    #[arg(long, value_name = "FILE")]
    lock: Option<PathBuf>,

    /// Output path for the OCI image.
    ///
    /// Ends with `.tar` → writes a tarball loadable by `podman load` / `docker load`.
    /// Otherwise → writes an OCI image layout directory.
    #[arg(short, long, value_name = "PATH", default_value = "image.tar")]
    output: PathBuf,

    /// Directory to hold downloaded packages (defaults to system cache).
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,

    /// Re-download packages even if they are cached.
    #[arg(long)]
    force_download: bool,

    /// Maximum number of parallel downloads.
    #[arg(long, value_name = "N", default_value = "4")]
    jobs: NonZeroUsize,
}

// ── inspect ───────────────────────────────────────────────────────────────────

#[derive(Parser)]
struct InspectArgs {
    /// What to inspect.
    #[command(subcommand)]
    target: InspectTarget,
}

#[derive(Subcommand)]
enum InspectTarget {
    /// Show packages in a lock file.
    Lock {
        /// Lock file to inspect.
        #[arg(value_name = "FILE", default_value = "paranoid.lock")]
        path: PathBuf,

        /// Show dependency tree instead of flat table.
        #[arg(long)]
        tree: bool,
    },

    /// Show the contents of a built OCI image (tar or directory layout).
    Image {
        /// Path to the OCI image (`.tar` tarball or OCI layout directory).
        #[arg(value_name = "PATH", default_value = "image.tar")]
        path: PathBuf,
    },
}

// ── entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Lock(args) => run_lock(args, cli.quiet).await,
        Commands::Build(args) => run_build(args, cli.quiet).await,
        Commands::Inspect(args) => run_inspect(args).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            st::error(&e);
            ExitCode::FAILURE
        }
    }
}

// ── lock implementation ───────────────────────────────────────────────────────

async fn run_lock(args: LockArgs, quiet: bool) -> Result<(), CliError> {
    let manifest = load_manifest(&args.manifest)?;
    let output = args
        .output
        .unwrap_or_else(|| default_lock_path(&args.manifest));

    // Read existing lock (for hash comparison, up-to-date check, and diff).
    let existing_lock: Option<LockFile> = if output.exists() {
        LockFile::from_file(&output).ok()
    } else {
        None
    };
    let existing_hash: Option<String> = existing_lock.as_ref().and_then(|l| l.content_hash().ok());

    // Check if lock is already up to date.
    if !args.force
        && let Some(ref existing) = existing_lock
        && existing.matches_config(&manifest)
    {
        if !quiet {
            let hash = existing_hash.as_deref().unwrap_or("unknown");
            eprintln!("Lockfile up to date.");
            st::footer(&output, hash);
        }
        return Ok(());
    }

    if !quiet {
        eprint!("Resolving dependencies...");
    }

    let service = ResolveService::new().map_err(CliError::resolve)?;
    let resolved = service
        .resolve(&manifest)
        .await
        .map_err(CliError::resolve)?;

    if !quiet {
        eprint!(" Done ");
    }

    if !resolved.unmet.is_empty() {
        for dep in &resolved.unmet {
            st::warning(format!("unmet dependency: {}", dep));
        }
    }

    let lock = LockFile::from_resolved(&manifest, resolved);
    lock.write(&output).map_err(CliError::lock)?;

    if !quiet {
        let new_hash = lock.content_hash().map_err(CliError::lock)?;
        let pkg_count = lock.packages.len();

        match (existing_lock.as_ref(), existing_hash.as_deref()) {
            (Some(_), Some(old_hash)) if old_hash == new_hash => {
                eprintln!("({} packages, no changes)", pkg_count);
            }
            (Some(old_lock), Some(_)) => {
                let (added, changed, removed) = lock_diff_counts(old_lock, &lock);
                eprintln!("(+{} ~{} -{})", added, changed, removed);
                print_lock_diff(old_lock, &lock);
            }
            _ => {
                eprintln!("({} packages)", pkg_count);
            }
        }

        st::footer(&output, &new_hash);
    }

    Ok(())
}

// ── build implementation ──────────────────────────────────────────────────────

async fn run_build(args: BuildArgs, quiet: bool) -> Result<(), CliError> {
    let manifest = load_manifest(&args.manifest)?;
    let lock_path = args
        .lock
        .unwrap_or_else(|| default_lock_path(&args.manifest));

    // Load lock file.
    if !lock_path.exists() {
        return Err(CliError::missing_lock(&lock_path));
    }
    let lock = LockFile::from_file(&lock_path).map_err(CliError::lock)?;

    if !lock.matches_config(&manifest) {
        st::warning(format!(
            "{} may be stale — run `paranoid lock` first",
            lock_path.display()
        ));
    }

    let options = BuildOptions {
        download: DownloadConfig {
            jobs: args.jobs,
            force: args.force_download,
        },
        cache_dir: args.cache_dir,
        output: ImageOutput::from_path(&args.output),
        created: source_date_epoch()?,
    };
    // Failures of individual packages are reported as they happen; the
    // returned error summarizes them.
    let report = build(&manifest, &lock, &options, &DotsReporter::new(quiet))
        .await
        .map_err(CliError::build)?;

    if !quiet {
        eprintln!("Done.");
        eprintln!();

        let output = options.output.path();
        let name = match manifest.image.as_ref().and_then(|i| i.name.as_ref()) {
            Some(_) => report.image.reference.clone(),
            None => output.display().to_string(),
        };
        eprintln!(
            "Built {} with {} packages ({} fresh, {} cached) sized {}.",
            name,
            lock.packages.len(),
            report.fresh,
            report.cached,
            st::fmt_size(report.image.total_size),
        );
        st::footer(output, report.image.manifest_digest.as_str());
    }

    Ok(())
}

// ── inspect implementation ────────────────────────────────────────────────────

async fn run_inspect(args: InspectArgs) -> Result<(), CliError> {
    match args.target {
        InspectTarget::Lock { path, tree } => {
            if tree {
                inspect_lock_tree(&path)
            } else {
                inspect_lock(&path)
            }
        }
        InspectTarget::Image { path } => inspect_image(&path),
    }
}

fn inspect_lock(path: &Path) -> Result<(), CliError> {
    let lock = LockFile::from_file(path).map_err(CliError::lock)?;

    let total_bytes: u64 = lock.packages.iter().map(|p| p.size).sum();
    let direct_count = lock.packages.iter().filter(|p| p.direct).count();
    let transitive_count = lock.packages.len() - direct_count;
    let pkg_count = lock.packages.len();

    // ── Header ────────────────────────────────────────────────────────────────
    println!("Inspect Lockfile");
    if transitive_count > 0 {
        println!(
            "{} packages, {} direct + {} transitive ({})",
            pkg_count,
            direct_count,
            transitive_count,
            st::fmt_size(total_bytes),
        );
    } else {
        println!("{} packages ({})", pkg_count, st::fmt_size(total_bytes),);
    }
    println!();

    // ── Table ─────────────────────────────────────────────────────────────────
    const W_NAME: usize = 28;
    const W_VER: usize = 20;
    const W_ARCH: usize = 8;
    const W_SIZE: usize = 8;

    // Header row
    println!(
        "{:<w_name$} {:<w_ver$} {:<w_arch$} {:>w_size$}",
        st::dim().apply_to("Package"),
        st::dim().apply_to("Version"),
        st::dim().apply_to("Arch"),
        st::dim().apply_to("Size"),
        w_name = W_NAME,
        w_ver = W_VER,
        w_arch = W_ARCH,
        w_size = W_SIZE,
    );

    // Rows: direct first, then transitive (dimmed).
    let mut sorted = lock.packages.clone();
    sorted.sort_by(|a, b| b.direct.cmp(&a.direct).then(a.name.cmp(&b.name)));

    for pkg in &sorted {
        let row = format!(
            "{:<w_name$} {:<w_ver$} {:<w_arch$} {:>w_size$}",
            st::truncate(&pkg.name, W_NAME),
            st::truncate(&pkg.version, W_VER),
            pkg.architecture.to_string(),
            st::fmt_size(pkg.size),
            w_name = W_NAME,
            w_ver = W_VER,
            w_arch = W_ARCH,
            w_size = W_SIZE,
        );
        if pkg.direct {
            println!("{}", st::bold().apply_to(&row));
        } else {
            println!("{}", st::dim().apply_to(&row));
        }
    }

    // ── Footer ────────────────────────────────────────────────────────────────
    println!();
    let hash = lock.content_hash().map_err(CliError::lock)?;
    st::footer_stdout(path, &hash);

    Ok(())
}

fn inspect_lock_tree(path: &Path) -> Result<(), CliError> {
    use std::collections::{HashMap, HashSet};

    let lock = LockFile::from_file(path).map_err(CliError::lock)?;

    // Build lookup: name → (version, size)
    let pkg_info: HashMap<&str, (&str, u64)> = lock
        .packages
        .iter()
        .map(|p| (p.name.as_str(), (p.version.as_str(), p.size)))
        .collect();

    // Build graph from lock file dep edges
    let graph: HashMap<&str, Vec<&str>> = lock
        .deps
        .iter()
        .map(|e| {
            (
                e.parent.as_str(),
                e.children.iter().map(|c| c.as_str()).collect(),
            )
        })
        .collect();

    // Roots: packages marked as direct
    let mut roots: Vec<&str> = lock
        .packages
        .iter()
        .filter(|p| p.direct)
        .map(|p| p.name.as_str())
        .collect();
    roots.sort();

    // ── Header ────────────────────────────────────────────────────────────────
    let total_bytes: u64 = lock.packages.iter().map(|p| p.size).sum();
    let direct_count = roots.len();
    let transitive_count = lock.packages.len() - direct_count;
    let pkg_count = lock.packages.len();

    println!("Inspect Lockfile");
    if transitive_count > 0 {
        println!(
            "{} packages, {} direct + {} transitive ({})",
            pkg_count,
            direct_count,
            transitive_count,
            st::fmt_size(total_bytes),
        );
    } else {
        println!("{} packages ({})", pkg_count, st::fmt_size(total_bytes),);
    }
    println!();

    // ── Tree ──────────────────────────────────────────────────────────────────
    let mut globally_seen: HashSet<&str> = HashSet::new();

    for (i, root) in roots.iter().enumerate() {
        let is_last_root = i == roots.len() - 1;

        let (ver, size) = pkg_info.get(root).copied().unwrap_or(("?", 0));
        println!(
            "{} {}",
            st::bold().apply_to(root),
            st::dim().apply_to(format!("{} ({})", ver, st::fmt_size(size))),
        );

        globally_seen.insert(root);

        if let Some(children) = graph.get(root) {
            let mut children = children.clone();
            children.sort();
            print_tree_children(&children, &graph, &pkg_info, &mut globally_seen, "");
        }

        if !is_last_root {
            println!();
        }
    }

    // ── Footer ────────────────────────────────────────────────────────────────
    println!();
    let hash = lock.content_hash().map_err(CliError::lock)?;
    st::footer_stdout(path, &hash);

    Ok(())
}

// ── inspect image implementation ──────────────────────────────────────────────

fn inspect_image(path: &Path) -> Result<(), CliError> {
    use paranoid::oci::{ImageConfig, ImageIndex, ImageManifest};
    use std::io::Read;

    let err = |msg: String| CliError::inspect_image(msg);

    // ── Step 1: load index.json and a blob reader from tar or directory ───────

    fn blob_path(digest: &str) -> Result<String, String> {
        let (algo, hex) = digest
            .split_once(':')
            .ok_or_else(|| format!("malformed digest: {}", digest))?;
        Ok(format!("blobs/{}/{}", algo, hex))
    }

    let mut files: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();

    if path.extension().is_some_and(|e| e == "tar") {
        let f = std::fs::File::open(path)
            .map_err(|e| err(format!("cannot open {}: {}", path.display(), e)))?;
        let mut archive = tar::Archive::new(f);
        for entry in archive
            .entries()
            .map_err(|e| err(format!("cannot read tar: {}", e)))?
        {
            let mut entry = entry.map_err(|e| err(format!("corrupt tar entry: {}", e)))?;
            let entry_path = entry
                .path()
                .map_err(|e| err(format!("bad tar path: {}", e)))?
                .to_string_lossy()
                .into_owned();

            if entry_path == "index.json" || entry_path.starts_with("blobs/") {
                let mut data = Vec::new();
                entry
                    .read_to_end(&mut data)
                    .map_err(|e| err(format!("error reading {}: {}", entry_path, e)))?;
                files.insert(entry_path, data);
            }
        }
    } else {
        let index_path = path.join("index.json");
        let data = std::fs::read(&index_path)
            .map_err(|e| err(format!("cannot read index.json: {}", e)))?;
        files.insert("index.json".to_owned(), data);
    }

    let read_blob = |digest: &str| -> Result<Vec<u8>, CliError> {
        let rel = blob_path(digest).map_err(&err)?;
        if let Some(data) = files.get(&rel) {
            return Ok(data.clone());
        }
        let full = path.join(&rel);
        std::fs::read(&full).map_err(|e| err(format!("cannot read blob {}: {}", rel, e)))
    };

    // ── Step 2: parse index.json ──────────────────────────────────────────────

    let index_bytes = files
        .get("index.json")
        .ok_or_else(|| err("index.json not found in image".to_owned()))?;

    let index: ImageIndex = serde_json::from_slice(index_bytes)
        .map_err(|e| err(format!("cannot parse index.json: {}", e)))?;

    if index.manifests.is_empty() {
        return Err(err("image index contains no manifests".to_owned()));
    }

    // ── Step 3: parse manifest ────────────────────────────────────────────────

    let manifest_desc = &index.manifests[0];
    let manifest_bytes = read_blob(manifest_desc.digest.as_str())?;
    let manifest: ImageManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| err(format!("cannot parse image manifest: {}", e)))?;

    // ── Step 4: parse image config ────────────────────────────────────────────

    let config_bytes = read_blob(manifest.config.digest.as_str())?;
    let image_config: ImageConfig = serde_json::from_slice(&config_bytes)
        .map_err(|e| err(format!("cannot parse image config: {}", e)))?;

    // ── Step 5: display ───────────────────────────────────────────────────────

    // Image ref from index annotations, if present.
    let image_ref = manifest_desc
        .annotations
        .as_ref()
        .and_then(|a| a.get("org.opencontainers.image.ref.name"))
        .map(|s| s.as_str());

    // Labels from config.
    let labels = image_config.config.as_ref().and_then(|c| c.labels.as_ref());

    let label = |key: &str| -> Option<&str> { labels.and_then(|m| m.get(key)).map(|s| s.as_str()) };

    // Image size from layers.
    let layer_bytes: u64 = manifest
        .layers
        .iter()
        .map(|l| u64::try_from(l.size).unwrap_or(0))
        .sum();

    // ── Dots key-value display ────────────────────────────────────────────────

    const LABEL_W: usize = 13;
    const TOTAL_W: usize = 60;

    println!("Inspect Image");
    println!();

    if let Some(r) = image_ref {
        st::dots_row("Name", r, LABEL_W, TOTAL_W);
    }
    st::dots_row(
        "Architecture",
        image_config.architecture.as_str(),
        LABEL_W,
        TOTAL_W,
    );

    // Created timestamp
    if let Some(ref ts) = image_config.created {
        st::dots_row("Created At", &ts.to_string(), LABEL_W, TOTAL_W);
    } else if let Some(v) = label("org.opencontainers.image.created") {
        st::dots_row("Created At", v, LABEL_W, TOTAL_W);
    }

    // Revision
    if let Some(v) = label("org.opencontainers.image.revision") {
        st::dots_row("Revision", v, LABEL_W, TOTAL_W);
    }

    // Entrypoint
    if let Some(ref cfg) = image_config.config {
        if let Some(ref ep) = cfg.entrypoint {
            st::dots_row("Entrypoint", &ep.join(" "), LABEL_W, TOTAL_W);
        }
        if let Some(ref cmd) = cfg.cmd {
            st::dots_row("Cmd", &cmd.join(" "), LABEL_W, TOTAL_W);
        }
        if let Some(ref workdir) = cfg.working_dir {
            st::dots_row("WorkingDir", workdir.as_str(), LABEL_W, TOTAL_W);
        }
    }

    // Size
    st::dots_row("Size", &st::fmt_size(layer_bytes), LABEL_W, TOTAL_W);

    // ── Footer ────────────────────────────────────────────────────────────────
    println!();
    st::footer_stdout(path, manifest_desc.digest.as_str());

    Ok(())
}

// ── tree helpers ──────────────────────────────────────────────────────────────

fn print_tree_children<'a>(
    children: &[&'a str],
    graph: &std::collections::HashMap<&'a str, Vec<&'a str>>,
    pkg_info: &std::collections::HashMap<&'a str, (&'a str, u64)>,
    globally_seen: &mut std::collections::HashSet<&'a str>,
    prefix: &str,
) {
    for (i, &child) in children.iter().enumerate() {
        let is_last = i == children.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });

        let already_seen = globally_seen.contains(child);
        let (ver, size) = pkg_info.get(child).copied().unwrap_or(("?", 0));

        let name_part = if already_seen {
            format!(
                "{} {}",
                st::changed().apply_to(child),
                st::dim().apply_to("(*)")
            )
        } else {
            format!("{}", st::added().apply_to(child))
        };
        let meta_part = st::dim().apply_to(format!("{} ({})", ver, st::fmt_size(size)));

        println!(
            "{}{}{} {}",
            prefix,
            st::dim().apply_to(connector),
            name_part,
            meta_part,
        );

        if !already_seen {
            globally_seen.insert(child);
            if let Some(grandchildren) = graph.get(child) {
                let mut grandchildren: Vec<&str> = grandchildren.clone();
                grandchildren.sort();
                print_tree_children(
                    &grandchildren,
                    graph,
                    pkg_info,
                    globally_seen,
                    &child_prefix,
                );
            }
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Resolve the default path for `paranoid.lock`.
fn default_lock_path(manifest: &Path) -> PathBuf {
    if manifest.is_dir() {
        manifest.join("paranoid.lock")
    } else {
        manifest
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("paranoid.lock")
    }
}

fn load_manifest(path: &Path) -> Result<Manifest, CliError> {
    if path.is_dir() {
        Manifest::from_dir(path).map_err(CliError::manifest)
    } else {
        Manifest::from_file(path).map_err(CliError::manifest)
    }
}

/// The image creation time: `SOURCE_DATE_EPOCH` when set, the Unix epoch
/// otherwise, so that building the same lock twice yields the same digest.
///
/// See <https://reproducible-builds.org/docs/source-date-epoch/>.
fn source_date_epoch() -> Result<Timestamp, CliError> {
    let Ok(raw) = std::env::var("SOURCE_DATE_EPOCH") else {
        return Ok(Timestamp::epoch());
    };
    raw.trim()
        .parse()
        .ok()
        .and_then(Timestamp::from_unix)
        .ok_or_else(|| CliError::config(format!("invalid SOURCE_DATE_EPOCH '{raw}'")))
}

/// Compute package diff counts between two lock files.
fn lock_diff_counts(old: &LockFile, new: &LockFile) -> (usize, usize, usize) {
    use std::collections::HashMap;

    let old_map: HashMap<&str, &str> = old
        .packages
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();
    let new_map: HashMap<&str, &str> = new
        .packages
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();

    let added = new_map.keys().filter(|n| !old_map.contains_key(*n)).count();
    let changed = new_map
        .iter()
        .filter(|(n, v)| old_map.get(*n).is_some_and(|ov| ov != *v))
        .count();
    let removed = old_map.keys().filter(|n| !new_map.contains_key(*n)).count();

    (added, changed, removed)
}

/// Print a diff of package changes between two lock files.
fn print_lock_diff(old: &LockFile, new: &LockFile) {
    use std::collections::HashMap;

    let old_map: HashMap<&str, &str> = old
        .packages
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();

    let new_map: HashMap<&str, &str> = new
        .packages
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();

    let mut entries: Vec<(&str, char, &str, String)> = Vec::new();

    for (name, new_ver) in &new_map {
        match old_map.get(name) {
            None => entries.push((name, '+', name, new_ver.to_string())),
            Some(old_ver) if old_ver != new_ver => {
                entries.push((name, '~', name, format!("{} → {}", old_ver, new_ver)));
            }
            _ => {}
        }
    }

    for (name, old_ver) in &old_map {
        if !new_map.contains_key(name) {
            entries.push((name, '-', name, old_ver.to_string()));
        }
    }

    if entries.is_empty() {
        return;
    }

    entries.sort_by_key(|(k, _, _, _)| *k);
    for (_, sigil, name, detail) in entries {
        st::diff_line(sigil, name, &detail);
    }
}
