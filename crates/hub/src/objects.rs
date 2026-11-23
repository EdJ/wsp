//! The object directory: each object's bytes in a file named by its hash,
//! `objects/ab/cdef…` (the first two hex digits are the directory, the other
//! 62 the file), as git keeps loose objects.
//!
//! A write goes to a temp file under `objects/tmp/`, is synced, and is renamed
//! into place, so a reader sees a whole object or none. An id already present
//! is not written again: its bytes are the ones that hash to it, so there is
//! nothing to rewrite. Bytes are held to their id ([`wsp_objects::verify`])
//! before they reach here; this module keeps what it is given, byte for byte,
//! and never re-encodes it.

use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use wsp_objects::ObjectId;
use wsp_store_contract::{Error, Result};

/// A directory of objects keyed by hash.
#[derive(Debug)]
pub struct ObjectDir {
    root: PathBuf,
    /// One writer at a time in this process, so two puts of one id here do not
    /// both rename it into place.
    write: Mutex<()>,
    /// Names this handle's temp files apart.
    seq: AtomicU64,
}

fn io(what: &str, path: &Path, e: std::io::Error) -> Error {
    Error::Backend {
        reason: format!("{what} {}: {e}", path.display()),
    }
}

impl ObjectDir {
    /// Opens (creating, if absent) the object directory at `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<ObjectDir> {
        let root = root.as_ref().to_path_buf();
        let tmp = root.join("tmp");
        fs::create_dir_all(&tmp).map_err(|e| io("creating", &tmp, e))?;
        Ok(ObjectDir {
            root,
            write: Mutex::new(()),
            seq: AtomicU64::new(0),
        })
    }

    /// The directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where `id`'s bytes are kept: `<root>/ab/cdef…`.
    pub fn path_of(&self, id: &ObjectId) -> PathBuf {
        let hex = id.hex();
        self.root.join(&hex[..2]).join(&hex[2..])
    }

    /// Whether `id` is held.
    pub fn has(&self, id: &ObjectId) -> bool {
        self.path_of(id).is_file()
    }

    /// `id`'s bytes, as they were written; `None` for an id not held.
    pub fn get(&self, id: &ObjectId) -> Result<Option<Vec<u8>>> {
        let path = self.path_of(id);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io("reading", &path, e)),
        }
    }

    /// Keeps `bytes` as `id`: `true` when written, `false` when `id` was held
    /// already (and left as it was). The caller has held them to `id`.
    pub fn put(&self, id: &ObjectId, bytes: &[u8]) -> Result<bool> {
        let _one = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.path_of(id);
        if path.is_file() {
            return Ok(false);
        }
        let dir = path
            .parent()
            .expect("an object's path has its fan-out directory");
        fs::create_dir_all(dir).map_err(|e| io("creating", dir, e))?;
        let tmp = self.root.join("tmp").join(format!(
            "{}.{}.{}",
            id.hex(),
            std::process::id(),
            self.seq.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut f = File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(io("writing", &tmp, e));
        }
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            return Err(io("renaming into", &path, e));
        }
        // The rename is durable once its directory is synced.
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(true)
    }
}
