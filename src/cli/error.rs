//! User-facing CLI errors with clear, actionable messages.

use std::error::Error;
use std::fmt;
use std::path::Path;

/// A top-level CLI error shown to the user before the process exits.
pub struct CliError(String);

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl Error for CliError {}

/// Render an error and its causes as `outer: cause: root cause`.
///
/// Some library errors already include their source in their own message;
/// a cause whose text is already shown is skipped instead of repeated.
fn render_chain(error: &dyn Error) -> String {
    let mut out = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    out
}

impl CliError {
    /// An error with `context` in front of the full cause chain.
    fn wrap(context: &str, error: impl Error) -> Self {
        Self(format!("{context}: {}", render_chain(&error)))
    }

    pub fn manifest(e: impl Error) -> Self {
        Self::wrap("could not load manifest", e)
    }

    pub fn resolve(e: impl Error) -> Self {
        Self::wrap("dependency resolution failed", e)
    }

    pub fn lock(e: impl Error) -> Self {
        Self::wrap("lock file error", e)
    }

    pub fn build(e: impl Error) -> Self {
        Self::wrap("build failed", e)
    }

    pub fn missing_lock(path: &Path) -> Self {
        Self(format!(
            "lock file '{}' not found — run `paranoid lock` first",
            path.display()
        ))
    }

    pub fn config(msg: impl fmt::Display) -> Self {
        Self(format!("configuration error: {msg}"))
    }

    pub fn inspect_image(msg: impl fmt::Display) -> Self {
        Self(format!("could not inspect image: {msg}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("failed to install bash")]
    struct Outer(#[source] Inner);

    #[derive(Debug, thiserror::Error)]
    #[error("disk full")]
    struct Inner;

    #[derive(Debug, thiserror::Error)]
    #[error("HTTP error: {0}")]
    struct Embeds(#[source] Inner);

    #[test]
    fn renders_the_whole_chain() {
        assert_eq!(
            CliError::build(Outer(Inner)).to_string(),
            "build failed: failed to install bash: disk full"
        );
    }

    #[test]
    fn skips_causes_already_in_the_message() {
        assert_eq!(render_chain(&Embeds(Inner)), "HTTP error: disk full");
    }
}
