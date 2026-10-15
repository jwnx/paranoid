//! Minimal dots-style terminal progress.
//!
//! Each phase prints a single line that grows dots as work completes:
//!
//! ```text
//! Downloading packages ....                 curl
//! Downloading packages .................... 20MB
//! Installing packages  .................... 247 files
//! Building image       .................... 9MB
//! ```

use std::io::Write;
use std::sync::Mutex;

use paranoid::report::{Event, Phase, Reporter};

use super::style;

/// Label column width (left-aligned, padded).
const LABEL_WIDTH: usize = 21;

/// Width of the dots area.
const DOTS_WIDTH: usize = 20;

/// How each phase is shown: its label and how to format its summary.
fn presentation(phase: Phase) -> (&'static str, fn(u64) -> String) {
    match phase {
        Phase::Download => ("Downloading packages", style::fmt_size),
        Phase::Assemble => ("Installing packages", |n| format!("{n} files")),
        Phase::Image => ("Building image", style::fmt_size),
    }
}

/// Renders [`Event`]s as one dots line per phase on stderr.
///
/// Failures are always printed, even when quiet.
pub struct DotsReporter {
    quiet: bool,
    /// Units done and total for the phase in progress.
    progress: Mutex<(u64, u64)>,
}

impl DotsReporter {
    pub fn new(quiet: bool) -> Self {
        Self {
            quiet,
            progress: Mutex::new((0, 0)),
        }
    }
}

impl Reporter for DotsReporter {
    fn report(&self, event: Event<'_>) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            Event::Begin { phase, total } => {
                *progress = (0, total);
                if !self.quiet {
                    print_line(presentation(phase).0, 0.0, "");
                }
            }
            Event::Advance { phase, units, item } => {
                progress.0 += units;
                if !self.quiet {
                    let (done, total) = *progress;
                    let frac = if total > 0 {
                        done as f64 / total as f64
                    } else {
                        1.0
                    };
                    print_line(presentation(phase).0, frac, item);
                }
            }
            Event::Failed { item, error, .. } => {
                eprintln!();
                style::error(format!("{item}: {error}"));
            }
            Event::End { phase, summary } => {
                if !self.quiet {
                    let (label, format) = presentation(phase);
                    finish_line(label, &format(summary));
                }
            }
        }
    }
}

/// Rewrite the current line with the label, dots proportional to progress,
/// and an optional message on the right.
fn print_line(label: &str, frac: f64, message: &str) {
    let filled = ((frac * DOTS_WIDTH as f64).round() as usize).min(DOTS_WIDTH);
    eprint!(
        "\r\x1b[2K{:<label_w$} {:<dots_w$} {}",
        label,
        style::dim().apply_to(".".repeat(filled)),
        message,
        label_w = LABEL_WIDTH,
        dots_w = DOTS_WIDTH,
    );
    let _ = std::io::stderr().flush();
}

/// Print the final state of a phase line.
fn finish_line(label: &str, message: &str) {
    eprint!("\r\x1b[2K");
    eprintln!(
        "{:<label_w$} {} {}",
        label,
        style::dim().apply_to(".".repeat(DOTS_WIDTH)),
        message,
        label_w = LABEL_WIDTH,
    );
}
