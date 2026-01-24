use std::path::{Path, PathBuf};

use crate::extract::ExtractionError;

/// Normalize a path by collapsing `.` and `..` components.
///
/// Unlike `canonicalize()`, this does not access the filesystem, so it works
/// with paths that don't exist yet. This is essential for validating symlink
/// targets before they're created.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();

    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                match components.last() {
                    Some(std::path::Component::Normal(_)) => {
                        components.pop();
                    }

                    // Already have "..", add another
                    Some(std::path::Component::ParentDir) => {
                        components.push(component);
                    }

                    // Empty. This is a relative path starting with ".."
                    None => {
                        components.push(component);
                    }

                    _ => {}
                }
            }
            std::path::Component::CurDir => {}
            _ => {
                components.push(component);
            }
        }
    }

    components.iter().collect()
}

/// Validate that a path doesn't escape the destination directory.
///
/// Archive paths starting with `/` are treated as relative to the destination,
/// not as absolute filesystem paths.
pub fn validate_path(entry_path: &Path, dest: &Path) -> Result<PathBuf, ExtractionError> {
    // Strip leading `/` - archive absolute paths are relative to dest
    let stripped = entry_path.strip_prefix("/").unwrap_or(entry_path);

    let normalized = normalize_path(stripped);

    // Reject paths that escape via ..
    if normalized.starts_with("..") {
        return Err(ExtractionError::PathTraversal {
            path: entry_path.to_path_buf(),
            reason: "path escapes destination".to_string(),
        });
    }

    let full_path = normalize_path(&dest.join(&normalized));

    if !full_path.starts_with(dest) {
        return Err(ExtractionError::PathTraversal {
            path: entry_path.to_path_buf(),
            reason: "path escapes destination".to_string(),
        });
    }

    Ok(full_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_path_simple() {
        let dest = Path::new("/tmp/extract");
        let result = validate_path(Path::new("foo/bar.txt"), dest).unwrap();
        assert_eq!(result, PathBuf::from("/tmp/extract/foo/bar.txt"));
    }

    #[test]
    fn test_validate_path_leading_slash() {
        let dest = Path::new("/tmp/extract");
        let result = validate_path(Path::new("/foo/bar.txt"), dest).unwrap();
        assert_eq!(result, PathBuf::from("/tmp/extract/foo/bar.txt"));
    }

    #[test]
    fn test_validate_path_dot_slash() {
        let dest = Path::new("/tmp/extract");
        let result = validate_path(Path::new("./foo/bar.txt"), dest).unwrap();
        assert_eq!(result, PathBuf::from("/tmp/extract/foo/bar.txt"));
    }

    #[test]
    fn test_validate_path_traversal_blocked() {
        let dest = Path::new("/tmp/extract");
        let result = validate_path(Path::new("../etc/passwd"), dest);
        assert!(result.is_err());
        assert!(matches!(result, Err(ExtractionError::PathTraversal { .. })));
    }

    #[test]
    fn test_validate_path_nested_traversal_blocked() {
        let dest = Path::new("/tmp/extract");
        let result = validate_path(Path::new("foo/../../etc/passwd"), dest);
        assert!(result.is_err());
    }

    #[test]
    fn test_normalize_path_simple() {
        assert_eq!(
            normalize_path(Path::new("foo/bar")),
            PathBuf::from("foo/bar")
        );
    }

    #[test]
    fn test_normalize_path_with_dotdot() {
        assert_eq!(
            normalize_path(Path::new("foo/bar/../baz")),
            PathBuf::from("foo/baz")
        );
    }

    #[test]
    fn test_normalize_path_collapses_multiple_dotdot() {
        assert_eq!(
            normalize_path(Path::new("foo/bar/baz/../../qux")),
            PathBuf::from("foo/qux")
        );
    }

    #[test]
    fn test_normalize_path_absolute_with_dotdot() {
        assert_eq!(
            normalize_path(Path::new("/tmp/extract/subdir/../../other")),
            PathBuf::from("/tmp/other")
        );
    }

    #[test]
    fn test_normalize_path_dotdot_at_root_ignored() {
        // Can't go above root
        assert_eq!(
            normalize_path(Path::new("/tmp/../../../etc")),
            PathBuf::from("/etc")
        );
    }

    #[test]
    fn test_normalize_path_removes_dot() {
        assert_eq!(
            normalize_path(Path::new("foo/./bar/./baz")),
            PathBuf::from("foo/bar/baz")
        );
    }

    #[test]
    fn test_normalize_path_relative_escape() {
        // Relative path that goes up - preserves the ..
        assert_eq!(
            normalize_path(Path::new("../etc/passwd")),
            PathBuf::from("../etc/passwd")
        );
    }
}
