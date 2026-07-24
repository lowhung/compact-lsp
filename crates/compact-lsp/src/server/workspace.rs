//! Workspace scanning and file discovery.

use std::io;
use std::path::{Path, PathBuf};

/// Recursively find all .compact files in a directory.
pub fn find_compact_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    find_compact_files_recursive(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn find_compact_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = std::fs::read_dir(dir)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_symlink() {
            continue;
        }

        if file_type.is_dir() {
            // Skip hidden directories and common non-source directories
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') && name != "node_modules" && name != "target" {
                find_compact_files_recursive(&path, files)?;
            }
        } else if file_type.is_file() && path.extension().map(|e| e == "compact").unwrap_or(false) {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_only_workspace_compact_sources() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        std::fs::create_dir_all(root.join("contracts/nested")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join("target/generated")).unwrap();

        std::fs::write(root.join("contracts/Main.compact"), "").unwrap();
        std::fs::write(root.join("contracts/nested/Utility.compact"), "").unwrap();
        std::fs::write(root.join("contracts/readme.md"), "").unwrap();
        std::fs::write(root.join(".hidden/Hidden.compact"), "").unwrap();
        std::fs::write(root.join("node_modules/pkg/Dependency.compact"), "").unwrap();
        std::fs::write(root.join("target/generated/Build.compact"), "").unwrap();

        let files = find_compact_files(root).unwrap();
        assert_eq!(
            files,
            vec![
                root.join("contracts/Main.compact"),
                root.join("contracts/nested/Utility.compact")
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::write(external.path().join("Outside.compact"), "").unwrap();
        symlink(external.path(), workspace.path().join("linked")).unwrap();

        assert!(find_compact_files(workspace.path()).unwrap().is_empty());
    }
}
