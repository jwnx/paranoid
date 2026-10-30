//! User-facing CLI errors with clear, actionable messages.

use std::error::Error;
use std::fmt;
use std::path::Path;

use paranoid::report::render_chain;

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

    /// A file that couldn't be fetched, or doesn't match its pinned hash.
    pub fn files(e: impl Error) -> Self {
        Self::wrap("could not lock files", e)
    }

    pub fn lock(e: impl Error) -> Self {
        Self::wrap("lock file error", e)
    }

    /// A lock file named on the command line that can't be read.
    pub fn read_lock(path: &Path, e: impl Error) -> Self {
        Self::wrap(&format!("could not read {}", path.display()), e)
    }

    pub fn build(e: impl Error) -> Self {
        Self::wrap("build failed", e)
    }

    pub fn sbom(e: impl Error) -> Self {
        Self::wrap("could not describe the image", e)
    }

    pub fn write(path: &Path, e: impl Error) -> Self {
        Self::wrap(&format!("could not write {}", path.display()), e)
    }

    pub fn push(e: impl Error) -> Self {
        Self::wrap("push failed", e)
    }

    /// A push to a URL without credentials, which a bucket likely refused.
    pub fn push_unsigned(e: impl Error) -> Self {
        let Self(message) = Self::push(e);
        Self(format!(
            "{message}\nrequests weren't signed; for an S3 bucket, set AWS_ACCESS_KEY_ID \
             and AWS_SECRET_ACCESS_KEY"
        ))
    }

    pub fn missing_lock(path: &Path) -> Self {
        Self(format!(
            "lock file '{}' not found — run `paranoid lock` first",
            path.display()
        ))
    }

    /// A lock that leaves dependencies unmet, used without `--allow-unmet`.
    pub fn unmet(unmet: &[String]) -> Self {
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

    /// A `-o` that would write every image of a set to the same path.
    pub fn output_template(path: &Path) -> Self {
        Self(format!(
            "building several images to '{}' would overwrite each with the next; \
             put {{image}} in the path, as in -o '{{image}}.tar', or pick one with --image",
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
