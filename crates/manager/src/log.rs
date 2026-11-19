//! The manager's own log, `<exec_dir>/log/manager.log` (2.4): one dated line
//! per thing it did or refused, rotated once to `manager.1.log` past [`CAP`].

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Rotate past this many bytes.
pub const CAP: u64 = 10 * 1024 * 1024;
pub const FILE: &str = "manager.log";
pub const OLD: &str = "manager.1.log";

pub struct Log {
    dir: PathBuf,
    file: Mutex<Option<File>>,
}

impl Log {
    /// Log under `dir`, made if missing.
    pub fn open(dir: &Path) -> std::io::Result<Log> {
        fs::create_dir_all(dir)?;
        let file = OpenOptions::new().create(true).append(true).open(dir.join(FILE))?;
        Ok(Log { dir: dir.to_path_buf(), file: Mutex::new(Some(file)) })
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    /// Write one line. A log that cannot be written loses the line, never the
    /// manager.
    pub fn line(&self, what: &str) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let big = file.as_ref().and_then(|f| f.metadata().ok()).is_some_and(|m| m.len() > CAP);
        if big {
            let _ = fs::rename(self.dir.join(FILE), self.dir.join(OLD));
            *file = OpenOptions::new().create(true).append(true).open(self.dir.join(FILE)).ok();
        }
        if let Some(f) = file.as_mut() {
            let _ = f.write_all(format!("{} {what}\n", wsp_base::util::now_iso()).as_bytes());
        }
    }
}
