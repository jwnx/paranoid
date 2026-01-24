//! Terminal progress reporters using indicatif.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use paranoid::download::DownloadedPackage;
use paranoid::download::{DownloadError, DownloadProgress};
use paranoid::extract::{ExtractionError, ExtractionProgress};
use paranoid::lock::LockedPackage;
use paranoid::oci::OciProgress;

// ── shared helpers ────────────────────────────────────────────────────────────

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
        .unwrap()
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
}

fn bytes_bar_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{spinner:.cyan} [{bar:30.cyan/blue}] {bytes}/{total_bytes} {wide_msg}",
    )
    .unwrap()
    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
    .progress_chars("█▉▊▋▌▍▎▏  ")
}

fn count_bar_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.cyan} [{bar:30.cyan/blue}] {pos}/{len} {wide_msg}")
        .unwrap()
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
        .progress_chars("█▉▊▋▌▍▎▏  ")
}

// ── Download progress ─────────────────────────────────────────────────────────

pub struct IndicatifDownloadProgress {
    quiet: bool,
    multi: MultiProgress,
    overall: ProgressBar,
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
}

impl IndicatifDownloadProgress {
    pub fn new(quiet: bool) -> Self {
        let multi = MultiProgress::new();
        let overall = multi.add(ProgressBar::new(0));
        overall.set_style(bytes_bar_style());
        Self {
            quiet,
            multi,
            overall,
            bytes_done: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
        }
    }

    pub fn finish(&self) {
        self.overall.finish_and_clear();
    }
}

impl DownloadProgress for IndicatifDownloadProgress {
    fn on_start(&self, total_packages: usize, total_bytes: u64) {
        if self.quiet {
            return;
        }
        self.bytes_total.store(total_bytes, Ordering::Relaxed);
        self.overall.set_length(total_bytes);
        self.overall.set_message(format!(
            "Downloading {} package{}",
            total_packages,
            if total_packages == 1 { "" } else { "s" }
        ));
        self.overall
            .enable_steady_tick(std::time::Duration::from_millis(80));
    }

    fn on_package_start(&self, package: &LockedPackage) {
        if self.quiet {
            return;
        }
        let pb = self.multi.add(ProgressBar::new_spinner());
        pb.set_style(spinner_style());
        pb.set_message(format!("  {} {}", package.name, package.version));
        pb.enable_steady_tick(std::time::Duration::from_millis(80));
        // Immediately finish the per-package spinner — progress is tracked on overall bar.
        pb.finish_and_clear();
    }

    fn on_package_complete(&self, package: &LockedPackage, from_cache: bool) {
        if self.quiet {
            return;
        }
        let done = self.bytes_done.fetch_add(package.size, Ordering::Relaxed) + package.size;
        self.overall.set_position(done);
        let suffix = if from_cache { " (cached)" } else { "" };
        self.overall
            .set_message(format!("{} {}{}", package.name, package.version, suffix));
    }

    fn on_package_error(&self, package: &LockedPackage, error: &DownloadError) {
        // Always print errors regardless of quiet mode.
        self.multi
            .println(format!(
                "  error: {} {}: {}",
                package.name, package.version, error
            ))
            .ok();
    }

    fn on_complete(&self, succeeded: usize, failed: usize) {
        if self.quiet {
            return;
        }
        if failed == 0 {
            self.overall.set_message(format!(
                "Downloaded {} package{}",
                succeeded,
                if succeeded == 1 { "" } else { "s" }
            ));
        } else {
            self.overall
                .set_message(format!("{} downloaded, {} failed", succeeded, failed));
        }
    }
}

// ── Extraction progress ───────────────────────────────────────────────────────

pub struct IndicatifExtractionProgress {
    quiet: bool,
    pb: ProgressBar,
}

impl IndicatifExtractionProgress {
    pub fn new(quiet: bool) -> Self {
        let pb = ProgressBar::new(0);
        pb.set_style(count_bar_style());
        Self { quiet, pb }
    }

    pub fn finish(&self) {
        self.pb.finish_and_clear();
    }
}

impl ExtractionProgress for IndicatifExtractionProgress {
    fn on_start(&self, total_packages: usize) {
        if self.quiet {
            return;
        }
        self.pb.set_length(total_packages as u64);
        self.pb.set_message(format!(
            "Extracting {} package{}",
            total_packages,
            if total_packages == 1 { "" } else { "s" }
        ));
        self.pb
            .enable_steady_tick(std::time::Duration::from_millis(80));
    }

    fn on_package_start(&self, package: &DownloadedPackage) {
        if self.quiet {
            return;
        }
        self.pb
            .set_message(format!("  {} {}", package.name, package.version));
    }

    fn on_package_complete(&self, _package: &DownloadedPackage, _files_extracted: usize) {
        if self.quiet {
            return;
        }
        self.pb.inc(1);
    }

    fn on_package_error(&self, package: &DownloadedPackage, error: &ExtractionError) {
        self.pb.println(format!(
            "  error: {} {}: {}",
            package.name, package.version, error
        ));
    }

    fn on_complete(&self, succeeded: usize, failed: usize) {
        if self.quiet {
            return;
        }
        if failed == 0 {
            self.pb.set_message(format!(
                "Extracted {} package{}",
                succeeded,
                if succeeded == 1 { "" } else { "s" }
            ));
        } else {
            self.pb
                .set_message(format!("{} extracted, {} failed", succeeded, failed));
        }
    }
}

// ── OCI progress ──────────────────────────────────────────────────────────────

pub struct IndicatifOciProgress {
    quiet: bool,
    pb: Mutex<ProgressBar>,
}

impl IndicatifOciProgress {
    pub fn new(quiet: bool) -> Self {
        let pb = ProgressBar::new_spinner();
        pb.set_style(spinner_style());
        Self {
            quiet,
            pb: Mutex::new(pb),
        }
    }

    pub fn finish(&self) {
        if let Ok(pb) = self.pb.lock() {
            pb.finish_and_clear();
        }
    }

    fn set_msg(&self, msg: &str) {
        if let Ok(pb) = self.pb.lock() {
            pb.set_message(msg.to_string());
        }
    }
}

impl OciProgress for IndicatifOciProgress {
    fn on_start(&self) {
        if self.quiet {
            return;
        }
        if let Ok(pb) = self.pb.lock() {
            pb.enable_steady_tick(std::time::Duration::from_millis(80));
        }
        self.set_msg("Building OCI image...");
    }

    fn on_layer_start(&self) {
        if !self.quiet {
            self.set_msg("Creating layer...");
        }
    }

    fn on_layer_complete(&self, size: u64) {
        if !self.quiet {
            let mb = size as f64 / 1_048_576.0;
            self.set_msg(&format!("Layer created ({:.2} MB)", mb));
        }
    }

    fn on_config_start(&self) {
        if !self.quiet {
            self.set_msg("Writing image config...");
        }
    }

    fn on_config_complete(&self) {}

    fn on_manifest_start(&self) {
        if !self.quiet {
            self.set_msg("Writing manifest...");
        }
    }

    fn on_manifest_complete(&self) {}

    fn on_writing_blobs(&self) {
        if !self.quiet {
            self.set_msg("Writing blobs...");
        }
    }

    fn on_creating_tarball(&self) {
        if !self.quiet {
            self.set_msg("Creating tarball...");
        }
    }

    fn on_complete(&self, _digest: &str, _total_size: u64) {
        if !self.quiet {
            self.set_msg("OCI image complete");
        }
    }

    fn on_error(&self, error: &str) {
        if let Ok(pb) = self.pb.lock() {
            pb.println(format!("error: {}", error));
        }
    }
}
