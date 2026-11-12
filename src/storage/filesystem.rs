//! Filesystem-backed storage with atomic writes and advisory locking.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::utils::{NxError, Result};

pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve(&self, relative: &str) -> Result<PathBuf> {
        let path = Path::new(relative);
        if path.is_absolute() {
            return Err(storage_error("absolute storage paths are not allowed"));
        }
        for component in path.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                _ => return Err(storage_error("storage paths may not contain '..'")),
            }
        }
        Ok(self.root.join(path))
    }

    pub fn ensure_dir(&self, relative: &str) -> Result<PathBuf> {
        let path = self.resolve(relative)?;
        fs::create_dir_all(&path)?;
        Ok(path)
    }

    pub fn write_atomic(&self, relative: &str, data: &[u8]) -> Result<()> {
        let path = self.resolve(relative)?;
        write_atomic(&path, data)
    }

    pub fn read(&self, relative: &str) -> Result<Option<Vec<u8>>> {
        let path = self.resolve(relative)?;
        read_locked(&path)
    }

    pub fn write_json<T: Serialize>(&self, relative: &str, value: &T) -> Result<()> {
        let data = serde_json::to_vec_pretty(value)
            .map_err(|error| storage_error(format!("serialize {relative}: {error}")))?;
        self.write_atomic(relative, &data)
    }

    pub fn read_json<T: DeserializeOwned>(&self, relative: &str) -> Result<Option<T>> {
        let Some(data) = self.read(relative)? else {
            return Ok(None);
        };
        serde_json::from_slice(&data)
            .map(Some)
            .map_err(|error| storage_error(format!("parse {relative}: {error}")))
    }

    pub fn remove_file(&self, relative: &str) -> Result<()> {
        let path = self.resolve(relative)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn exists(&self, relative: &str) -> bool {
        self.resolve(relative)
            .map(|path| path.exists())
            .unwrap_or(false)
    }
}

/// Atomically replace `path` with `data`. A lock file guards concurrent writers.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = acquire_lock(path)?;
    let temp = temp_path(path);
    let result = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)?;
        Ok::<(), NxError>(())
    })();
    let _ = fs::remove_file(&temp);
    let _ = lock.unlock();
    result
}

pub fn read_locked(path: &Path) -> Result<Option<Vec<u8>>> {
    if !path.exists() {
        return Ok(None);
    }
    let lock = acquire_lock(path)?;
    let mut file = File::open(path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    let _ = lock.unlock();
    Ok(Some(data))
}

fn acquire_lock(path: &Path) -> Result<File> {
    let lock_path = lock_path(path);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    file.lock_exclusive()?;
    Ok(file)
}

fn lock_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("storage");
    path.with_file_name(format!(".{name}.lock"))
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("storage");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    path.with_file_name(format!(".{name}.tmp-{}-{nanos}", std::process::id()))
}

fn storage_error(message: impl Into<String>) -> NxError {
    NxError::Storage(message.into())
}
