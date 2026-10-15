//! User-facing CLI errors with clear, actionable messages.

use std::error::Error;
use std::fmt;
use std::path::Path;

use paranoid::lock::LockedUnmet;

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

    /// A lock that leaves dependencies unmet, used without `--allow-unmet`.
    pub fn unmet(unmet: &[LockedUnmet]) -> Self {
        let s = if unmet.len() == 1 { "y" } else { "ies" };
        let mut msg = format!("{} unmet dependenc{s}:", unmet.len());
        for u in unmet {
            msg.push_str(&format!("\n  {u}"));
        }
        msg.push_str(
            "\nnothing in the configured repositories satisfies them, so the image \
             would be missing them; add a repository that has them, or pass \
             --allow-unmet to go ahead without them",
        );
        Self(msg)
    }

    /// A lock that no longer stands for the manifest, used without
    /// `--allow-stale`.
    pub fn stale(path: &Path) -> Self {
        Self(format!(
            "{} is stale: the manifest changed since it was written, or none of the \
             keys that signed it are trusted any more; run `paranoid lock`, or pass \
             --allow-stale to build from it anyway",
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
