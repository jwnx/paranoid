//! paranoid — deterministic, minimal OCI images from Debian packages.
//!
//! Subcommands:
//!   lock     Resolve dependencies and write paranoid.lock
//!   build    Download, extract, and build an OCI image
//!   inspect  Show the contents of a lock file or built image

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod cli {
    pub mod error;
    pub mod progress;
    pub mod style;
}

use cli::error::CliError;
use cli::progress::{IndicatifDownloadProgress, IndicatifExtractionProgress, IndicatifOciProgress};
use cli::style as st;

use paranoid::config::Manifest;
use paranoid::download::{DownloadConfig, DownloadService};
use paranoid::extract::{ExtractionConfig, ExtractionMode, ExtractionService};
use paranoid::lock::LockFile;
use paranoid::oci::{OciConfig, OciImageService};
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

    /// Print additional diagnostic information.
    #[arg(short, long, global = true)]
    verbose: bool,

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
    #[arg(long, value_name = "N", default_value_t = 4)]
    jobs: usize,
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
        Commands::Lock(args) => run_lock(args, cli.quiet, cli.verbose).await,
        Commands::Build(args) => run_build(args, cli.quiet, cli.verbose).await,
        Commands::Inspect(args) => run_inspect(args).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::FAILURE
        }
    }
}

// ── lock implementation ───────────────────────────────────────────────────────

async fn run_lock(args: LockArgs, quiet: bool, _verbose: bool) -> Result<(), CliError> {
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
    let existing_hash: Option<String> = existing_lock
        .as_ref()
        .and_then(|l| l.content_hash().ok());

    // Check if lock is already up to date.
    if !args.force
        && let Some(ref existing) = existing_lock
        && existing.matches_config(&manifest)
    {
        if !quiet {
            let hash = existing_hash.as_deref().unwrap_or("unknown");
            st::note("Lock file", hash);
            st::status("Up to date", "use --force to re-resolve");
        }
        return Ok(());
    }

    if !quiet {
        if let Some(ref hash) = existing_hash {
            st::note("Current", hash);
        }
        st::status_info("Resolving", "dependencies...");
    }

    let service = ResolveService::new().map_err(CliError::resolve)?;
    let resolved = service
        .resolve(&manifest)
        .await
        .map_err(CliError::resolve)?;

    if !resolved.unmet.is_empty() {
        for dep in &resolved.unmet {
            st::warning(format!("unmet dependency: {}", dep));
        }
    }

    let lock = LockFile::from_resolved(&manifest, resolved);
    lock.write(&output).map_err(CliError::lock)?;

    if !quiet {
        let new_hash = lock.content_hash().map_err(CliError::lock)?;
        st::note("New", &new_hash);

        let pkg_count = lock.packages.len();
        let msg = format!(
            "{} · {} package{}",
            output.display(),
            pkg_count,
            if pkg_count == 1 { "" } else { "s" }
        );

        if let Some(ref old) = existing_hash {
            if *old == new_hash {
                st::status("Wrote", format!("{} (no changes)", msg));
            } else if let Some(ref old_lock) = existing_lock {
                // Compute counts first, print summary, then details.
                let (added, changed, removed) = lock_diff_counts(old_lock, &lock);
                st::status(
                    "Updated",
                    format!("{} (+{} ~{} -{})", msg, added, changed, removed),
                );
                print_lock_diff(old_lock, &lock);
            } else {
                st::status("Updated", &msg);
            }
        } else {
            st::status("Wrote", &msg);
        }
    }

    Ok(())
}

// ── build implementation ──────────────────────────────────────────────────────

async fn run_build(args: BuildArgs, quiet: bool, verbose: bool) -> Result<(), CliError> {
    let manifest = load_manifest(&args.manifest)?;
    let lock_path = args
        .lock
        .unwrap_or_else(|| default_lock_path(&args.manifest));

    // Load lock file.
    if !lock_path.exists() {
        return Err(CliError::missing_lock(&lock_path));
    }
    let lock = LockFile::from_file(&lock_path).map_err(CliError::lock)?;

    if verbose {
        let total_bytes: u64 = lock.packages.iter().map(|p| p.size).sum();
        let total_mb = total_bytes as f64 / 1_048_576.0;
        st::note(
            "Lock",
            format!(
                "{} · {} packages · {:.2} MB",
                lock_path.display(),
                lock.packages.len(),
                total_mb,
            ),
        );
    }

    if !lock.matches_config(&manifest) {
        st::warning(format!(
            "{} may be stale — run `paranoid lock` first",
            lock_path.display()
        ));
    }

    // ── Download ──────────────────────────────────────────────────────────────
    let download_config = DownloadConfig::new()
        .with_max_concurrent(args.jobs)
        .with_force_download(args.force_download);

    let download_service = DownloadService::with_config(download_config)
        .map_err(CliError::download)?;

    let download_progress = IndicatifDownloadProgress::new(quiet);
    let download_result = download_service
        .download_all(&lock, &manifest.repositories, &download_progress)
        .await;

    download_progress.finish();

    if verbose {
        let cached = download_result.succeeded.iter().filter(|p| p.from_cache).count();
        let fresh = download_result.succeeded.len() - cached;
        st::status(
            "Downloaded",
            format!(
                "{} packages ({} fresh, {} cached)",
                download_result.succeeded.len(),
                fresh,
                cached,
            ),
        );
    }

    if !download_result.failed.is_empty() {
        for failure in &download_result.failed {
            st::error(format!("download failed for {}: {}", failure.name, failure.error));
        }
        return Err(CliError::download_failures(download_result.failed.len()));
    }

    // ── Extract ───────────────────────────────────────────────────────────────
    let extract_dir = tempfile::tempdir().map_err(CliError::io)?;
    let extract_config = ExtractionConfig::new().with_mode(ExtractionMode::Merged);
    let extract_service = ExtractionService::with_config(extract_config);

    let extract_progress = IndicatifExtractionProgress::new(quiet);
    let extract_result = extract_service
        .extract_all(&download_result.succeeded, extract_dir.path(), &extract_progress)
        .await;

    extract_progress.finish();

    if verbose {
        let total_files: usize = extract_result.succeeded.iter().map(|p| p.files.len()).sum();
        st::status(
            "Extracted",
            format!(
                "{} packages ({} files)",
                extract_result.succeeded.len(),
                total_files,
            ),
        );
    }

    if !extract_result.failed.is_empty() {
        for failure in &extract_result.failed {
            st::error(format!("extraction failed for {}: {}", failure.name, failure.error));
        }
        return Err(CliError::extract_failures(extract_result.failed.len()));
    }

    // ── OCI ───────────────────────────────────────────────────────────────────
    let oci_config = build_oci_config(&manifest)?;

    // Capture name+tag before moving config into the service.
    let image_ref: Option<String> = oci_config
        .get_image_name()
        .map(|name| format!("{}:{}", name.as_str(), oci_config.get_image_tag().as_str()));

    let oci_service = OciImageService::with_config(oci_config);
    let oci_progress = IndicatifOciProgress::new(quiet);

    let source_dir = extract_dir.path();
    let output = &args.output;
    let result = if output.extension().is_some_and(|e| e == "tar") {
        oci_service.generate_tarball(source_dir, output, &oci_progress)
    } else {
        oci_service.generate(source_dir, output, &oci_progress)
    }
    .map_err(CliError::oci)?;

    oci_progress.finish();

    if !quiet {
        st::status(
            "Built",
            format!("{} ({})", output.display(), fmt_size(result.total_size)),
        );
        st::note("Digest", result.manifest_digest.as_str());
        if let Some(ref image_ref) = image_ref {
            st::note("Image", image_ref);
            // Hint to load the image, using the right tool if detectable.
            let load_cmd = if output.extension().is_some_and(|e| e == "tar") {
                let loader = detect_container_tool();
                format!("{} load -i {}", loader, output.display())
            } else {
                format!("podman load -i {}", output.display())
            };
            st::note("Hint", format!("{} && {} run --rm {}", load_cmd, detect_container_tool(), image_ref));
        }
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
    use console::Style;

    let lock = LockFile::from_file(path).map_err(CliError::lock)?;

    let total_bytes: u64 = lock.packages.iter().map(|p| p.size).sum();
    let direct_count = lock.packages.iter().filter(|p| p.direct).count();
    let transitive_count = lock.packages.len() - direct_count;

    // ── Header (stdout, styled) ───────────────────────────────────────────────
    let dim = Style::new().dim();
    let bold = Style::new().bold();
    let label_style = Style::new().dim();

    let summary = if transitive_count > 0 {
        format!(
            "{} direct + {} transitive  ·  {} total download",
            direct_count,
            transitive_count,
            fmt_size(total_bytes),
        )
    } else {
        format!(
            "{} packages  ·  {} total download",
            direct_count,
            fmt_size(total_bytes),
        )
    };

    println!(
        "{:>12} {}",
        label_style.apply_to("Lock file"),
        path.display()
    );
    println!("{:>12} {}", label_style.apply_to("Packages"), summary);
    println!();

    // ── Column widths ─────────────────────────────────────────────────────────
    const W_NAME: usize = 36;
    const W_VER: usize = 24;
    const W_ARCH: usize = 8;
    const W_SIZE: usize = 8;

    let header = format!(
        "  {:<w_name$} {:<w_ver$} {:<w_arch$} {:>w_size$}",
        "Package", "Version", "Arch", "Size",
        w_name = W_NAME, w_ver = W_VER, w_arch = W_ARCH, w_size = W_SIZE,
    );
    println!("{}", dim.apply_to(&header));
    println!(
        "{}",
        dim.apply_to(format!("  {}", "─".repeat(W_NAME + W_VER + W_ARCH + W_SIZE + 3)))
    );

    // ── Rows: direct first (bold), then transitive (dim) ─────────────────────
    let mut sorted = lock.packages.clone();
    sorted.sort_by(|a, b| b.direct.cmp(&a.direct).then(a.name.cmp(&b.name)));

    let mut last_was_direct = true;
    for pkg in &sorted {
        if last_was_direct && !pkg.direct {
            println!();
        }
        last_was_direct = pkg.direct;

        let row = format!(
            "  {:<w_name$} {:<w_ver$} {:<w_arch$} {:>w_size$}",
            truncate(&pkg.name, W_NAME),
            truncate(&pkg.version, W_VER),
            pkg.architecture.to_string(),
            fmt_size(pkg.size),
            w_name = W_NAME, w_ver = W_VER, w_arch = W_ARCH, w_size = W_SIZE,
        );
        if pkg.direct {
            println!("{}", bold.apply_to(&row));
        } else {
            println!("{}", dim.apply_to(&row));
        }
    }

    Ok(())
}

fn inspect_lock_tree(path: &Path) -> Result<(), CliError> {
    use console::Style;
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
        .map(|e| (e.parent.as_str(), e.children.iter().map(|c| c.as_str()).collect()))
        .collect();

    // Roots: packages marked as direct
    let mut roots: Vec<&str> = lock
        .packages
        .iter()
        .filter(|p| p.direct)
        .map(|p| p.name.as_str())
        .collect();
    roots.sort();

    let dim = Style::new().dim();
    let bold = Style::new().bold();
    let styles = TreeStyles {
        dim: Style::new().dim(),
        green: Style::new().green(),
        yellow: Style::new().yellow(),
    };

    // Header
    let total_bytes: u64 = lock.packages.iter().map(|p| p.size).sum();
    let direct_count = roots.len();
    let transitive_count = lock.packages.len() - direct_count;
    println!(
        "{:>12} {}",
        dim.apply_to("Lock file"),
        path.display()
    );
    if transitive_count > 0 {
        println!(
            "{:>12} {} direct + {} transitive  ·  {} total",
            dim.apply_to("Packages"),
            direct_count,
            transitive_count,
            fmt_size(total_bytes),
        );
    } else {
        println!(
            "{:>12} {} packages  ·  {} total",
            dim.apply_to("Packages"),
            direct_count,
            fmt_size(total_bytes),
        );
    }
    println!();

    let mut globally_seen: HashSet<&str> = HashSet::new();

    for (i, root) in roots.iter().enumerate() {
        let is_last_root = i == roots.len() - 1;

        // Print root line
        let (ver, size) = pkg_info.get(root).copied().unwrap_or(("?", 0));
        println!(
            "{} {}",
            bold.apply_to(root),
            dim.apply_to(format!("{} ({})", ver, fmt_size(size))),
        );

        globally_seen.insert(root);

        if let Some(children) = graph.get(root) {
            let mut children = children.clone();
            children.sort();
            print_tree_children(
                &children,
                &graph,
                &pkg_info,
                &mut globally_seen,
                "",
                &styles,
            );
        }

        if !is_last_root {
            println!();
        }
    }

    Ok(())
}

// ── inspect image implementation ──────────────────────────────────────────────

fn inspect_image(path: &Path) -> Result<(), CliError> {
    use console::Style;
    use paranoid::oci::{ImageConfig, ImageIndex, ImageManifest};
    use std::io::Read;

    let err = |msg: String| CliError::inspect_image(msg);

    // ── Step 1: load index.json and a blob reader from tar or directory ───────

    /// Reads the raw bytes of a blob given its digest string (e.g. "sha256:abc…").
    /// `read_blob` is a closure that maps a relative blob path to bytes.
    fn blob_path(digest: &str) -> Result<String, String> {
        let (algo, hex) = digest
            .split_once(':')
            .ok_or_else(|| format!("malformed digest: {}", digest))?;
        Ok(format!("blobs/{}/{}", algo, hex))
    }

    // Bytes of every file we care about, keyed by relative path inside the OCI layout.
    let mut files: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();

    if path.extension().is_some_and(|e| e == "tar") {
        // OCI tarball — read relevant entries into memory.
        let f = std::fs::File::open(path)
            .map_err(|e| err(format!("cannot open {}: {}", path.display(), e)))?;
        let mut archive = tar::Archive::new(f);
        for entry in archive
            .entries()
            .map_err(|e| err(format!("cannot read tar: {}", e)))?
        {
            let mut entry =
                entry.map_err(|e| err(format!("corrupt tar entry: {}", e)))?;
            let entry_path = entry
                .path()
                .map_err(|e| err(format!("bad tar path: {}", e)))?
                .to_string_lossy()
                .into_owned();

            // We only need index.json and blobs.
            if entry_path == "index.json" || entry_path.starts_with("blobs/") {
                let mut data = Vec::new();
                entry
                    .read_to_end(&mut data)
                    .map_err(|e| err(format!("error reading {}: {}", entry_path, e)))?;
                files.insert(entry_path, data);
            }
        }
    } else {
        // OCI directory layout — read files lazily as needed.
        // We pre-load index.json; blobs are read on demand below.
        let index_path = path.join("index.json");
        let data = std::fs::read(&index_path)
            .map_err(|e| err(format!("cannot read index.json: {}", e)))?;
        files.insert("index.json".to_owned(), data);
    }

    // Helper: read blob bytes either from the pre-loaded map or directly from disk.
    let read_blob = |digest: &str| -> Result<Vec<u8>, CliError> {
        let rel = blob_path(digest).map_err(&err)?;
        if let Some(data) = files.get(&rel) {
            return Ok(data.clone());
        }
        // Directory layout: read from disk.
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

    let dim = Style::new().dim();
    let bold = Style::new().bold();

    // Image ref from index annotations, if present.
    let image_ref = manifest_desc
        .annotations
        .as_ref()
        .and_then(|a| a.get("org.opencontainers.image.ref.name"))
        .map(|s| s.as_str());

    // Labels from config.
    let labels = image_config
        .config
        .as_ref()
        .and_then(|c| c.labels.as_ref());

    // Helper: look up a label by OCI key.
    let label = |key: &str| -> Option<&str> {
        labels.and_then(|m| m.get(key)).map(|s| s.as_str())
    };

    // Row printer — left-aligned label, right-aligned value.
    let row = |name: &str, value: &str| {
        println!("{:>16}  {}", dim.apply_to(name), value);
    };

    println!();

    // Image identity
    if let Some(r) = image_ref {
        row("Image", &bold.apply_to(r).to_string());
    }
    row("Digest", manifest_desc.digest.as_str());
    row("Architecture", image_config.architecture.oci_name());

    // Timestamps
    if let Some(ref ts) = image_config.created {
        row("Created", &ts.to_string());
    }
    if let Some(v) = label("org.opencontainers.image.created")
        && image_config.created.is_none()
    {
        row("Created", v);
    }

    // Metadata labels
    if let Some(v) = label("org.opencontainers.image.title") {
        row("Title", v);
    }
    if let Some(v) = label("org.opencontainers.image.description") {
        row("Description", v);
    }
    if let Some(v) = label("org.opencontainers.image.authors") {
        row("Authors", v);
    }
    if let Some(v) = label("org.opencontainers.image.source") {
        row("Source", v);
    }
    if let Some(v) = label("org.opencontainers.image.revision") {
        row("Revision", v);
    }

    // Runtime config
    if let Some(ref cfg) = image_config.config {
        if let Some(ref ep) = cfg.entrypoint {
            row("Entrypoint", &ep.join(" "));
        }
        if let Some(ref cmd) = cfg.cmd {
            row("Cmd", &cmd.join(" "));
        }
        if let Some(ref workdir) = cfg.working_dir {
            row("WorkingDir", workdir.as_str());
        }
        if let Some(ref env) = cfg.env {
            for (i, var) in env.iter().enumerate() {
                let label_str = if i == 0 { "Env" } else { "" };
                row(label_str, var.as_str());
            }
        }
    }

    // Layers
    let layer_count = manifest.layers.len();
    let layer_bytes: u64 = manifest
        .layers
        .iter()
        .map(|l| u64::try_from(l.size).unwrap_or(0))
        .sum();
    row(
        "Layers",
        &format!(
            "{}  {}",
            layer_count,
            dim.apply_to(format!("({})", fmt_size(layer_bytes)))
        ),
    );

    // Print each layer digest indented.
    for layer in &manifest.layers {
        println!(
            "{:>16}  {}",
            "",
            dim.apply_to(layer.digest.as_str())
        );
    }

    println!();

    Ok(())
}

struct TreeStyles {
    dim: console::Style,
    green: console::Style,
    yellow: console::Style,
}

fn print_tree_children<'a>(
    children: &[&'a str],
    graph: &std::collections::HashMap<&'a str, Vec<&'a str>>,
    pkg_info: &std::collections::HashMap<&'a str, (&'a str, u64)>,
    globally_seen: &mut std::collections::HashSet<&'a str>,
    prefix: &str,
    styles: &TreeStyles,
) {
    for (i, &child) in children.iter().enumerate() {
        let is_last = i == children.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });

        let already_seen = globally_seen.contains(child);
        let (ver, size) = pkg_info.get(child).copied().unwrap_or(("?", 0));

        let name_part = if already_seen {
            format!("{} {}", styles.yellow.apply_to(child), styles.dim.apply_to("(*)"))
        } else {
            format!("{}", styles.green.apply_to(child))
        };
        let meta_part = styles.dim.apply_to(format!("{} ({})", ver, fmt_size(size)));

        println!(
            "{}{}{} {}",
            prefix,
            styles.dim.apply_to(connector),
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
                    styles,
                );
            }
        }
    }
}

/// Format a byte count as a human-readable size string.
fn fmt_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.0} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Truncate a string to at most `max` chars, adding `…` if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Resolve the default path for `paranoid.lock`.
///
/// If the manifest path is a directory, place the lock file inside it.
/// Otherwise fall back to `./paranoid.lock`.
fn default_lock_path(manifest: &Path) -> PathBuf {
    if manifest.is_dir() {
        manifest.join("paranoid.lock")
    } else {
        // -f points to a file: place the lock next to it
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

fn build_oci_config(manifest: &Manifest) -> Result<OciConfig, CliError> {
    let mut config = OciConfig::new();

    if let Some(image) = &manifest.image {
        // Architecture
        let arch: paranoid::arch::Architecture = image
            .architecture
            .parse()
            .map_err(|_| CliError::invalid_arch(&image.architecture))?;
        config = config.architecture(arch);

        // Image name
        if let Some(ref name) = image.name {
            config = config
                .try_image_name(name.as_str())
                .map_err(|e| CliError::config(format!("invalid image name '{}': {}", name, e)))?;
        }

        // Image tag
        if let Some(ref tag) = image.tag {
            config = config
                .try_image_tag(tag.as_str())
                .map_err(|e| CliError::config(format!("invalid image tag '{}': {}", tag, e)))?;
        }

        // OCI standard labels
        // org.opencontainers.image.created — RFC 3339 current time
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        config = config.label("org.opencontainers.image.created", now);

        // .title from name
        if let Some(ref name) = image.name {
            config = config.label("org.opencontainers.image.title", name);
        }

        // .description, .authors, .source from manifest metadata
        if let Some(ref desc) = image.description {
            config = config.label("org.opencontainers.image.description", desc);
        }
        if let Some(ref authors) = image.authors {
            config = config.label("org.opencontainers.image.authors", authors);
        }
        if let Some(ref source) = image.source {
            config = config.label("org.opencontainers.image.source", source);
        }

        // .revision — try to read from git HEAD
        if let Some(rev) = git_revision() {
            config = config.label("org.opencontainers.image.revision", rev);
        }
    }

    // Entrypoint (top-level in manifest)
    if let Some(ref ep) = manifest.entrypoint {
        config = config.entrypoint(ep.cmd.iter().cloned());
    }

    Ok(config)
}

/// Compute package diff counts between two lock files.
///
/// Returns `(added, changed, removed)`.
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

    let added = new_map
        .keys()
        .filter(|n| !old_map.contains_key(*n))
        .count();
    let changed = new_map
        .iter()
        .filter(|(n, v)| old_map.get(*n).is_some_and(|ov| ov != *v))
        .count();
    let removed = old_map
        .keys()
        .filter(|n| !new_map.contains_key(*n))
        .count();

    (added, changed, removed)
}

/// Print a diff of package changes between two lock files.
///
/// Shows `+` for added, `-` for removed, `~` for version changes.
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

    // Collect (sort_key, sigil, name, detail) tuples, then sort by name.
    let mut entries: Vec<(&str, char, &str, String)> = Vec::new();

    for (name, new_ver) in &new_map {
        match old_map.get(name) {
            None => entries.push((name, '+', name, new_ver.to_string())),
            Some(old_ver) if old_ver != new_ver => {
                entries.push((name, '~', name, format!("{} → {}", old_ver, new_ver)));
            }
            _ => {} // unchanged
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

/// Detect whether `podman` or `docker` is available, defaulting to `podman`.
fn detect_container_tool() -> &'static str {
    if std::process::Command::new("podman")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        "podman"
    } else if std::process::Command::new("docker")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        "docker"
    } else {
        "podman"
    }
}

/// Attempt to read the current git HEAD commit SHA.
///
/// Returns `None` if git is not available or we're not in a git repo.
fn git_revision() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
