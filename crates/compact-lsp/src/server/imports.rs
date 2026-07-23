//! File URI conversion, import resolution, and cross-file symbol lookup.

use std::path::{Path, PathBuf};

use url::Url;

/// Convert a percent-encoded file URI into a platform-native path.
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    Url::parse(uri).ok()?.to_file_path().ok()
}

/// Convert a platform-native path into a correctly encoded file URI.
pub fn path_to_file_uri(path: &Path) -> Option<String> {
    Url::from_file_path(path).ok().map(Url::into)
}

/// Resolve an import path relative to the current file.
///
/// Converts relative import paths like "../utils/Utils" to absolute file URIs.
pub fn resolve_import_path(current_uri: &str, import_path: &str) -> Option<String> {
    // Get the directory of the current file
    let current_path = file_uri_to_path(current_uri)?;
    let current_dir = current_path.parent()?;

    // Resolve the relative import path
    let import_with_ext = if import_path.ends_with(".compact") {
        import_path.to_string()
    } else {
        format!("{}.compact", import_path)
    };

    let resolved = current_dir.join(&import_with_ext);
    let normalized = normalize_path(&resolved)?;

    path_to_file_uri(&normalized)
}

/// Normalize a path by resolving .. and . components.
pub fn normalize_path(path: &Path) -> Option<PathBuf> {
    // Use canonicalize if the file exists, otherwise do manual normalization
    if path.exists() {
        path.canonicalize().ok()
    } else {
        // Manual normalization for non-existent paths
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                std::path::Component::CurDir => {}
                _ => normalized.push(component.as_os_str()),
            }
        }
        Some(normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_round_trip_preserves_spaces_and_unicode() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("Compact Project/π.compact");
        let uri = path_to_file_uri(&path).unwrap();

        assert!(uri.contains("Compact%20Project"));
        assert!(uri.contains("%CF%80.compact"));
        assert_eq!(file_uri_to_path(&uri).unwrap(), path);
    }

    #[test]
    fn relative_imports_are_normalized_and_encoded() {
        let temporary = tempfile::tempdir().unwrap();
        let current_path = temporary
            .path()
            .join("Compact Project/contracts/Main.compact");
        let uri = path_to_file_uri(&current_path).unwrap();
        let resolved = resolve_import_path(&uri, "../shared/Utility").unwrap();

        assert_eq!(
            file_uri_to_path(&resolved).unwrap(),
            temporary
                .path()
                .join("Compact Project/shared/Utility.compact")
        );
        assert!(resolved.contains("Compact%20Project"));
    }

    #[test]
    fn rejects_non_file_uris() {
        assert_eq!(file_uri_to_path("https://example.com/Main.compact"), None);
        assert_eq!(
            resolve_import_path("untitled:Main.compact", "./Utility"),
            None
        );
    }
}
