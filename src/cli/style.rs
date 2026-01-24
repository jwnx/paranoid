//! Cargo-style terminal output helpers.
//!
//! Each status line is printed as:
//!
//! ```text
//!   <bold-colored verb right-padded to 12 chars>  message
//! ```
//!
//! This mirrors the output style of `cargo build`, `cargo lock`, etc.

use console::Style;

// Column width for the verb, matching Cargo's 12-char right-justified label.
const VERB_WIDTH: usize = 12;

/// Print a status line: bold-green verb + message.
///
/// Used for positive events (resolved, wrote, built, …).
pub fn status(verb: &str, msg: impl std::fmt::Display) {
    let s = Style::new().green().bold();
    eprintln!("{:>width$} {}", s.apply_to(verb), msg, width = VERB_WIDTH);
}

/// Print a status line in bold-cyan (informational / in-progress).
pub fn status_info(verb: &str, msg: impl std::fmt::Display) {
    let s = Style::new().cyan().bold();
    eprintln!("{:>width$} {}", s.apply_to(verb), msg, width = VERB_WIDTH);
}

/// Print a dim note line (hash display, secondary info).
pub fn note(label: &str, msg: impl std::fmt::Display) {
    let label_s = Style::new().dim();
    eprintln!(
        "{:>width$} {}",
        label_s.apply_to(label),
        msg,
        width = VERB_WIDTH
    );
}

/// Print a warning line: bold-yellow "warning" verb.
pub fn warning(msg: impl std::fmt::Display) {
    let s = Style::new().yellow().bold();
    eprintln!(
        "{:>width$} {}",
        s.apply_to("warning"),
        msg,
        width = VERB_WIDTH
    );
}

/// Print an error line: bold-red "error" verb.
pub fn error(msg: impl std::fmt::Display) {
    let s = Style::new().red().bold();
    eprintln!(
        "{:>width$} {}",
        s.apply_to("error"),
        msg,
        width = VERB_WIDTH
    );
}

/// Format a diff line for package changes.
///
/// `+` added   → green
/// `~` changed → yellow  
/// `-` removed → red
pub fn diff_line(sigil: char, name: &str, detail: impl std::fmt::Display) {
    match sigil {
        '+' => eprintln!(
            "               {} {} {}",
            Style::new().green().bold().apply_to("+"),
            Style::new().green().apply_to(name),
            detail,
        ),
        '-' => eprintln!(
            "               {} {} {}",
            Style::new().red().bold().apply_to("-"),
            Style::new().red().apply_to(name),
            detail,
        ),
        '~' => eprintln!(
            "               {} {} {}",
            Style::new().yellow().bold().apply_to("~"),
            Style::new().yellow().apply_to(name),
            detail,
        ),
        _ => eprintln!("               {} {} {}", sigil, name, detail),
    }
}
