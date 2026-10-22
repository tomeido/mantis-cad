//! Cross-process locks for the complete read/validate/replace transaction.
//!
//! Sidecars are intentionally retained: removing a locked inode would let a
//! second process create and lock a different inode for the same document.

use crate::CliError;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

pub(crate) struct DocumentLocks {
    _files: Vec<File>,
}

impl DocumentLocks {
    pub(crate) fn acquire(paths: &[&Path]) -> Result<Self, CliError> {
        let mut canonical = paths
            .iter()
            .map(|path| canonical_document_path(path))
            .collect::<Result<Vec<_>, _>>()?;
        canonical.sort();
        canonical.dedup();
        let mut files = Vec::new();
        for path in canonical {
            let mut name = std::ffi::OsString::from(".");
            name.push(
                path.file_name()
                    .ok_or_else(|| CliError::usage("document path needs a file name"))?,
            );
            name.push(".mantis-lock");
            let lock_path = path.with_file_name(name);
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(&lock_path).map_err(|error| {
                CliError::coded(
                    "lock_failed",
                    format!("cannot open document lock: {error}"),
                    serde_json::json!({"path": path}),
                )
            })?;
            file.try_lock().map_err(|error| {
                let busy = matches!(error, std::fs::TryLockError::WouldBlock);
                CliError::coded(
                    if busy { "document_busy" } else { "lock_failed" },
                    format!("cannot lock {}: {error}", path.display()),
                    serde_json::json!({"path": path, "retryable": busy}),
                )
            })?;
            files.push(file);
        }
        Ok(Self { _files: files })
    }
}

fn canonical_document_path(path: &Path) -> Result<PathBuf, CliError> {
    if path.exists() {
        return path.canonicalize().map_err(|error| {
            CliError::coded(
                "lock_failed",
                format!("cannot resolve document path: {error}"),
                serde_json::json!({"path": path}),
            )
        });
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize().map_err(|error| {
        CliError::coded(
            "lock_failed",
            format!("cannot resolve document directory: {error}"),
            serde_json::json!({"path": path}),
        )
    })?;
    let name = path
        .file_name()
        .ok_or_else(|| CliError::usage("document path needs a file name"))?;
    Ok(parent.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_share_lock_and_released_lock_is_reusable() {
        let directory =
            std::env::temp_dir().join(format!("mantis-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("model.json");
        std::fs::write(&path, "fixture").unwrap();
        let first = DocumentLocks::acquire(&[&path]).unwrap();
        let alias = directory.join(".").join("model.json");
        assert!(
            matches!(DocumentLocks::acquire(&[&alias]), Err(CliError::Detailed { code, .. }) if code == "document_busy")
        );
        drop(first);
        let second = DocumentLocks::acquire(&[&path, &alias]).unwrap();
        drop(second);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
