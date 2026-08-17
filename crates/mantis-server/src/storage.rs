//! Durable JSON persistence shared by the legacy and multi-project stores.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Failure phase for an atomic JSON replacement.
///
/// `Published` means the atomic rename/replacement completed and readers may
/// already observe the candidate bytes, but syncing the parent directory
/// failed. Callers must not keep serving an older in-memory value in that
/// case; they should publish the candidate and enter a fail-stop state.
#[derive(Debug)]
pub enum PersistFailure {
    NotPublished(io::Error),
    Published(io::Error),
}

impl PersistFailure {
    fn into_io_error(self) -> io::Error {
        match self {
            Self::NotPublished(error) | Self::Published(error) => error,
        }
    }
}

impl std::fmt::Display for PersistFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPublished(error) => write!(formatter, "before atomic publish: {error}"),
            Self::Published(error) => {
                write!(
                    formatter,
                    "after atomic publish, durability uncertain: {error}"
                )
            }
        }
    }
}

impl std::error::Error for PersistFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotPublished(error) | Self::Published(error) => Some(error),
        }
    }
}

/// Load and deserialize a JSON document without accepting a missing file.
pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))
}

/// Serialize to a sibling temporary file, fsync it, atomically rename it, and
/// fsync the parent directory where the platform supports directory handles.
///
/// A successful return therefore means the new bytes, the rename, and the
/// directory entry have all reached the durability boundary exposed by the OS.
pub fn persist_json<T: Serialize + ?Sized>(value: &T, path: &Path) -> io::Result<()> {
    persist_json_observed(value, path).map_err(PersistFailure::into_io_error)
}

/// Atomic replacement with parent creation and observable failure phase.
pub fn persist_json_observed<T: Serialize + ?Sized>(
    value: &T,
    path: &Path,
) -> Result<(), PersistFailure> {
    persist_json_inner(value, path, true)
}

/// Atomically replace a JSON document only when its parent directory already
/// exists. Runtime project writes use this variant so deleting a project
/// directory cannot be silently "repaired" into an incomplete project that
/// will fail validation after restart.
pub fn persist_json_existing<T: Serialize + ?Sized>(value: &T, path: &Path) -> io::Result<()> {
    persist_json_existing_observed(value, path).map_err(PersistFailure::into_io_error)
}

/// Runtime replacement that preserves whether the candidate became visible
/// before an error. Multi-project state uses this to avoid disk/memory splits.
pub fn persist_json_existing_observed<T: Serialize + ?Sized>(
    value: &T,
    path: &Path,
) -> Result<(), PersistFailure> {
    persist_json_inner(value, path, false)
}

fn persist_json_inner<T: Serialize + ?Sized>(
    value: &T,
    path: &Path,
    create_parent: bool,
) -> Result<(), PersistFailure> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cannot serialize JSON: {error}"),
            )
        })
        .map_err(PersistFailure::NotPublished)?;
    let parent = path.parent().filter(|path| !path.as_os_str().is_empty());
    if let Some(parent) = parent {
        if create_parent {
            std::fs::create_dir_all(parent).map_err(PersistFailure::NotPublished)?;
        } else if !parent.is_dir() {
            return Err(PersistFailure::NotPublished(io::Error::new(
                io::ErrorKind::NotFound,
                format!("parent directory does not exist: {}", parent.display()),
            )));
        }
    }

    let tmp = temp_path(path);
    let before_publish = (|| -> io::Result<()> {
        let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace_atomic(&tmp, path)
    })();
    if let Err(error) = before_publish {
        let _ = std::fs::remove_file(&tmp);
        return Err(PersistFailure::NotPublished(error));
    }
    sync_parent(path).map_err(PersistFailure::Published)
}

#[cfg(not(target_os = "windows"))]
fn replace_atomic(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)
}

/// `std::fs::rename` cannot replace an existing destination on Windows.
/// `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)` preserves the same atomic
/// replacement contract used on Unix for every push after the first.
#[cfg(target_os = "windows")]
fn replace_atomic(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both pointers reference live, NUL-terminated UTF-16 buffers and
    // the flags are valid MoveFileExW options.
    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    name.push(format!(".tmp-{}-{sequence}", std::process::id()));
    PathBuf::from(name)
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    maybe_fail_sync(path, SyncKind::Parent)?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    File::open(parent)?.sync_all()
}

#[cfg(unix)]
pub fn sync_directory(path: &Path) -> io::Result<()> {
    maybe_fail_sync(path, SyncKind::Directory)?;
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    maybe_fail_sync(_path, SyncKind::Parent)?;
    // Windows does not allow opening a directory through std::fs::File. The
    // file itself is still synced before the atomic replacement.
    Ok(())
}

#[cfg(not(unix))]
pub fn sync_directory(_path: &Path) -> io::Result<()> {
    maybe_fail_sync(_path, SyncKind::Directory)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SyncKind {
    Parent,
    Directory,
}

#[cfg(not(test))]
fn maybe_fail_sync(_path: &Path, _kind: SyncKind) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
fn maybe_fail_sync(path: &Path, kind: SyncKind) -> io::Result<()> {
    let mut failures = sync_failure_registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let key = (path.to_path_buf(), kind);
    let Some(remaining) = failures.get_mut(&key) else {
        return Ok(());
    };
    *remaining = remaining.saturating_sub(1);
    if *remaining == 0 {
        failures.remove(&key);
    }
    Err(io::Error::other("injected directory sync failure"))
}

#[cfg(test)]
fn inject_sync_failure(path: &Path, kind: SyncKind, count: usize) {
    sync_failure_registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert((path.to_path_buf(), kind), count);
}

#[cfg(test)]
fn sync_failure_registry(
) -> &'static std::sync::Mutex<std::collections::BTreeMap<(PathBuf, SyncKind), usize>> {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};

    static FAILURES: OnceLock<Mutex<BTreeMap<(PathBuf, SyncKind), usize>>> = OnceLock::new();
    FAILURES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(test)]
pub(crate) fn fail_parent_sync_for_test(path: &Path) {
    inject_sync_failure(path, SyncKind::Parent, 1);
}

#[cfg(test)]
pub(crate) fn fail_directory_sync_for_test(path: &Path, count: usize) {
    inject_sync_failure(path, SyncKind::Directory, count);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Value {
        revision: u64,
        label: String,
    }

    fn path() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "mantis-storage-test-{}-{n}/nested/value.json",
            std::process::id()
        ))
    }

    #[test]
    fn durable_replace_round_trips_and_removes_temp_file() {
        let path = path();
        let first = Value {
            revision: 1,
            label: "first".into(),
        };
        let second = Value {
            revision: 2,
            label: "second".into(),
        };

        persist_json(&first, &path).unwrap();
        assert_eq!(load_json::<Value>(&path).unwrap(), first);
        persist_json(&second, &path).unwrap();
        assert_eq!(load_json::<Value>(&path).unwrap(), second);
        let file_name = path.file_name().unwrap().to_string_lossy();
        assert!(std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{file_name}.tmp-"))));

        let root = path.parent().unwrap().parent().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn existing_parent_variant_never_recreates_a_deleted_project_directory() {
        let path = path();
        let value = Value {
            revision: 1,
            label: "must fail".into(),
        };
        let error = persist_json_existing(&value, &path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn reports_post_replace_sync_failure_as_published() {
        let path = path();
        let first = Value {
            revision: 1,
            label: "first".into(),
        };
        let second = Value {
            revision: 2,
            label: "second".into(),
        };
        persist_json(&first, &path).unwrap();
        fail_parent_sync_for_test(&path);
        let error = persist_json_existing_observed(&second, &path).unwrap_err();
        assert!(matches!(error, PersistFailure::Published(_)));
        assert_eq!(load_json::<Value>(&path).unwrap(), second);

        let root = path.parent().unwrap().parent().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
