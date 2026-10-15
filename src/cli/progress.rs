//! Minimal dots-style terminal progress reporters.
//!
//! Each phase prints a single line that grows dots as work completes:
//!
//! ```text
//! Downloading packages ....                 curl
//! Downloading packages .................... 20MB
//! Extracting packages  .................... 247 files
//! Building layer       ..................
//! Done.
//! ```

use std::io::Write;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use paranoid::download::{DownloadError, DownloadProgress, DownloadedPackage};
use paranoid::extract::{ExtractionError, ExtractionProgress};
use paranoid::lock::LockedPackage;
use paranoid::oci::OciProgress;

use super::style;

// ── Constants ────────────────────────────────────────────────────────────────

/// Label column width (left-aligned, padded).
const LABEL_WIDTH: usize = 21;

/// Total line width for the dots area.
const DOTS_WIDTH: usize = 20;

// ── Shared helpers ───────────────────────────────────────────────────────────

/// Rewrite the current line with label, dots proportional to progress, and a right-side message.
fn print_dots_line(label: &str, progress_frac: f64, right_msg: &str) {
    let filled = ((progress_frac * DOTS_WIDTH as f64).round() as usize).min(DOTS_WIDTH);
    let dots = ".".repeat(filled);

    // If there's a right message, pad between dots and message.
    if right_msg.is_empty() {
        eprint!(
            "\r{:<label_w$} {:<dots_w$}",
            label,
            style::dim().apply_to(&dots),
            label_w = LABEL_WIDTH,
            dots_w = DOTS_WIDTH,
        );
    } else {
        eprint!(
            "\r{:<label_w$} {:<dots_w$} {}",
            label,
            style::dim().apply_to(&dots),
            right_msg,
            label_w = LABEL_WIDTH,
            dots_w = DOTS_WIDTH,
        );
    }
    let _ = std::io::stderr().flush();
}

/// Finalize a dots line (print newline, clear any trailing chars).
fn finish_dots_line(label: &str, right_msg: &str) {
    let dots = ".".repeat(DOTS_WIDTH);
    // Clear the full line then print final state.
    eprint!("\r\x1b[2K");
    eprintln!(
        "{:<label_w$} {} {}",
        label,
        style::dim().apply_to(&dots),
        right_msg,
        label_w = LABEL_WIDTH,
    );
}

// ── Download progress ────────────────────────────────────────────────────────

pub struct DotsDownloadProgress {
    quiet: bool,
    total_bytes: AtomicU64,
    done_bytes: AtomicU64,
    cached_count: AtomicUsize,
    fresh_count: AtomicUsize,
    current_pkg: Mutex<String>,
}

const DOWNLOAD_LABEL: &str = "Downloading packages";

impl DotsDownloadProgress {
    pub fn new(quiet: bool) -> Self {
        Self {
            quiet,
            total_bytes: AtomicU64::new(0),
            done_bytes: AtomicU64::new(0),
            cached_count: AtomicUsize::new(0),
            fresh_count: AtomicUsize::new(0),
            current_pkg: Mutex::new(String::new()),
        }
    }

    pub fn cached_count(&self) -> usize {
        self.cached_count.load(Ordering::Relaxed)
    }

    pub fn fresh_count(&self) -> usize {
        self.fresh_count.load(Ordering::Relaxed)
    }

    pub fn finish(&self) {
        if self.quiet {
            return;
        }
        let total = self.total_bytes.load(Ordering::Relaxed);
        finish_dots_line(DOWNLOAD_LABEL, &style::fmt_size(total));
    }
}

impl DownloadProgress for DotsDownloadProgress {
    fn on_start(&self, _total_packages: usize, total_bytes: u64) {
        if self.quiet {
            return;
        }
        self.total_bytes.store(total_bytes, Ordering::Relaxed);
        print_dots_line(DOWNLOAD_LABEL, 0.0, "");
    }

    fn on_package_start(&self, package: &LockedPackage) {
        if self.quiet {
            return;
        }
        if let Ok(mut current) = self.current_pkg.lock() {
            *current = package.name.clone();
        }
        let total = self.total_bytes.load(Ordering::Relaxed);
        let done = self.done_bytes.load(Ordering::Relaxed);
        let frac = if total > 0 {
            done as f64 / total as f64
        } else {
            0.0
        };
        print_dots_line(DOWNLOAD_LABEL, frac, &package.name);
    }

    fn on_package_complete(&self, package: &LockedPackage, from_cache: bool) {
        if from_cache {
            self.cached_count.fetch_add(1, Ordering::Relaxed);
        } else {
            self.fresh_count.fetch_add(1, Ordering::Relaxed);
        }

        let done = self.done_bytes.fetch_add(package.size, Ordering::Relaxed) + package.size;

        if self.quiet {
            return;
        }

        let total = self.total_bytes.load(Ordering::Relaxed);
        let frac = if total > 0 {
            done as f64 / total as f64
        } else {
            1.0
        };
        let current = self
            .current_pkg
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        print_dots_line(DOWNLOAD_LABEL, frac, &current);
    }

    fn on_package_error(&self, package: &LockedPackage, error: &DownloadError) {
        // Always print errors.
        eprintln!();
        style::error(format!("{} {}: {}", package.name, package.version, error));
    }

    fn on_complete(&self, _succeeded: usize, _failed: usize) {
        // finish() handles final line.
    }
}

// ── Extraction progress ──────────────────────────────────────────────────────

pub struct DotsExtractionProgress {
    quiet: bool,
    total: AtomicUsize,
    done: AtomicUsize,
    total_files: AtomicUsize,
}

const EXTRACT_LABEL: &str = "Extracting packages";

impl DotsExtractionProgress {
    pub fn new(quiet: bool) -> Self {
        Self {
            quiet,
            total: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
            total_files: AtomicUsize::new(0),
        }
    }

    pub fn finish(&self) {
        if self.quiet {
            return;
        }
        let files = self.total_files.load(Ordering::Relaxed);
        finish_dots_line(EXTRACT_LABEL, &format!("{} files", files));
    }
}

impl ExtractionProgress for DotsExtractionProgress {
    fn on_start(&self, total_packages: usize) {
        if self.quiet {
            return;
        }
        self.total.store(total_packages, Ordering::Relaxed);
        print_dots_line(EXTRACT_LABEL, 0.0, "");
    }

    fn on_package_start(&self, _package: &DownloadedPackage) {}

    fn on_package_complete(&self, _package: &DownloadedPackage, files_extracted: usize) {
        self.total_files
            .fetch_add(files_extracted, Ordering::Relaxed);
        let done = self.done.fetch_add(1, Ordering::Relaxed) + 1;

        if self.quiet {
            return;
        }

        let total = self.total.load(Ordering::Relaxed);
        let frac = if total > 0 {
            done as f64 / total as f64
        } else {
            1.0
        };
        print_dots_line(EXTRACT_LABEL, frac, "");
    }

    fn on_package_error(&self, package: &DownloadedPackage, error: &ExtractionError) {
        eprintln!();
        style::error(format!("{} {}: {}", package.name, package.version, error));
    }

    fn on_complete(&self, _succeeded: usize, _failed: usize) {
        // finish() handles final line.
    }
}

// ── OCI progress ─────────────────────────────────────────────────────────────

pub struct DotsOciProgress {
    quiet: bool,
    step: AtomicUsize,
}

const BUILD_LABEL: &str = "Building layer";
/// Total number of OCI generation steps (layer, config, manifest, blobs, tarball).
const BUILD_STEPS: usize = 5;

impl DotsOciProgress {
    pub fn new(quiet: bool) -> Self {
        Self {
            quiet,
            step: AtomicUsize::new(0),
        }
    }

    pub fn finish(&self) {
        if self.quiet {
            return;
        }
        finish_dots_line(BUILD_LABEL, "");
    }

    fn advance(&self) {
        let step = self.step.fetch_add(1, Ordering::Relaxed) + 1;
        if self.quiet {
            return;
        }
        let frac = (step as f64 / BUILD_STEPS as f64).min(1.0);
        print_dots_line(BUILD_LABEL, frac, "");
    }
}

impl OciProgress for DotsOciProgress {
    fn on_start(&self) {
        if self.quiet {
            return;
        }
        print_dots_line(BUILD_LABEL, 0.0, "");
    }

    fn on_layer_start(&self) {}

    fn on_layer_complete(&self, _size: u64) {
        self.advance();
    }

    fn on_config_start(&self) {}

    fn on_config_complete(&self) {
        self.advance();
    }

    fn on_manifest_start(&self) {}

    fn on_manifest_complete(&self) {
        self.advance();
    }

    fn on_writing_blobs(&self) {
        self.advance();
    }

    fn on_creating_tarball(&self) {
        self.advance();
    }

    fn on_complete(&self, _digest: &str, _total_size: u64) {
        // finish() handles final line.
    }

    fn on_error(&self, error: &str) {
        eprintln!();
        style::error(error);
    }
}
