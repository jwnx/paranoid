//! Minimal, paranoid-style terminal output helpers.
//!
//! Design principles:
//! - Amarelo (yellow) as primary accent color
//! - Dots (....) as visual filler / progress language
//! - Footer pattern: path + hash on every command
//! - Minimal noise, maximum signal

use console::Style;
use std::path::Path;

// ── Lazy styles ──────────────────────────────────────────────────────────────

/// Primary accent: yellow (paranoid / caution).
pub fn accent() -> Style {
    Style::new().yellow()
}

/// Bold text (for hashes, important values).
pub fn bold() -> Style {
    Style::new().bold()
}

/// Dimmed text (secondary info, transitive deps).
pub fn dim() -> Style {
    Style::new().dim()
}

/// Error style: red.
fn error_style() -> Style {
    Style::new().red()
}

/// Diff added: green.
pub fn added() -> Style {
    Style::new().green()
}

/// Diff removed: red.
pub fn removed() -> Style {
    Style::new().red()
}

/// Diff changed: yellow.
pub fn changed() -> Style {
    Style::new().yellow()
}

// ── Output helpers ───────────────────────────────────────────────────────────

/// Format the standard footer: path + hash.
///
/// Returns (path_line, hash_line) for the caller to print to the
/// appropriate stream (stderr for actions, stdout for inspect).
pub fn format_footer(path: &Path, hash: &str) -> (String, String) {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    (
        format!("{}", dim().apply_to(abs.display())),
        format!("{}", dim().apply_to(hash)),
    )
}

/// Print the standard footer to stderr (for action commands: lock, build).
pub fn footer(path: &Path, hash: &str) {
    let (path_line, hash_line) = format_footer(path, hash);
    eprintln!("{}", path_line);
    eprintln!("{}", hash_line);
}

/// Print the standard footer to stdout (for inspect commands).
pub fn footer_stdout(path: &Path, hash: &str) {
    let (path_line, hash_line) = format_footer(path, hash);
    println!("{}", path_line);
    println!("{}", hash_line);
}

/// Print a dots-filled key-value line for inspect-style output (stdout).
///
/// ```text
/// Name         ................. node:latest
/// ```
pub fn dots_row(label: &str, value: &str, label_width: usize, total_width: usize) {
    let dots_count = total_width
        .saturating_sub(label_width)
        .saturating_sub(value.len())
        .saturating_sub(2); // spaces around dots
    let dots = ".".repeat(dots_count.max(3));
    println!(
        "{:<width$} {} {}",
        label,
        dim().apply_to(&dots),
        value,
        width = label_width,
    );
}

/// Print an error line.
pub fn error(msg: impl std::fmt::Display) {
    eprintln!("{} {}", error_style().apply_to("error:"), msg);
}

/// Print a warning line.
pub fn warning(msg: impl std::fmt::Display) {
    eprintln!("{} {}", accent().apply_to("warning:"), msg);
}

/// Format a byte count as a human-readable size string.
pub fn fmt_size(bytes: u64) -> String {
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

/// Truncate a string to at most `max` chars, adding `..` if truncated.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}..", &s[..max.saturating_sub(2)])
    }
}
