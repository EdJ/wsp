//! The journal (`docs/agent-manager.md` 2.5, 3.1): every event the manager
//! sends, appended to `<exec_dir>/journal.jsonl` *before* it is sent, one
//! frame per line, its `id` a per-manager sequence from 1. A client that
//! reconnects asks `subscribe {from}` with the last id it heard and is sent
//! every line after it, so nothing is lost across a client's drop or the
//! manager's own restart.
//!
//! Each line is written with one `write` on an `O_APPEND` file, so a manager
//! killed mid-append leaves at most one torn last line; [`Journal::open`] cuts
//! it off before anything is appended after it. Nothing is fsynced: a process
//! that dies keeps what it wrote, and a host that loses power is outside what
//! the journal promises.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;
use wsp_host_api::{EventId, Frame, Message};

pub struct Journal {
    path: PathBuf,
    file: File,
    head: Option<EventId>,
}

impl Journal {
    /// Open (or begin) the journal at `path`, cutting a torn last line.
    pub fn open(path: &Path) -> io::Result<Journal> {
        let text = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        let whole = text.iter().rposition(|b| *b == b'\n').map_or(0, |at| at + 1);
        if whole < text.len() {
            let f = OpenOptions::new().write(true).open(path)?;
            f.set_len(whole as u64)?;
        }
        let head = text[..whole]
            .split(|b| *b == b'\n')
            .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
            .filter_map(|v| v.get("id").and_then(Value::as_u64))
            .max()
            .map(EventId);
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Journal { path: path.to_path_buf(), file, head })
    }

    /// The newest id written, `None` for an empty journal.
    pub fn head(&self) -> Option<EventId> {
        self.head
    }

    /// Write `msg` as the next event, and return the frame to send.
    pub fn append(&mut self, msg: Message) -> io::Result<Frame> {
        let id = EventId(self.head.map_or(1, |EventId(n)| n + 1));
        let frame = Frame { id: Some(id), req: None, msg };
        let mut line = serde_json::to_vec(&frame).map_err(io::Error::other)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.head = Some(id);
        Ok(frame)
    }

    /// Every event after `from` (all of them for `None`), oldest first, and
    /// how many lines were skipped because this build cannot read them.
    pub fn after(&self, from: Option<EventId>) -> io::Result<(Vec<Frame>, usize)> {
        let text = fs::read(&self.path)?;
        let mut frames = Vec::new();
        let mut skipped = 0;
        for line in text.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            match serde_json::from_slice::<Frame>(line) {
                Ok(f) if f.id > from => frames.push(f),
                Ok(_) => {}
                Err(_) => skipped += 1,
            }
        }
        Ok((frames, skipped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_host_api::{At, Beat};

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wsp-manager-journal-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn ev(n: u64) -> Message {
        Message::Beat(Beat { at: At(n), host: None, seats: vec![] })
    }

    #[test]
    fn ids_run_on_from_the_head_across_a_reopen() {
        let p = dir("reopen").join("journal.jsonl");
        let mut j = Journal::open(&p).unwrap();
        assert_eq!(j.head(), None);
        assert_eq!(j.append(ev(1)).unwrap().id, Some(EventId(1)));
        assert_eq!(j.append(ev(2)).unwrap().id, Some(EventId(2)));
        drop(j);
        let mut j = Journal::open(&p).unwrap();
        assert_eq!(j.head(), Some(EventId(2)));
        assert_eq!(j.append(ev(3)).unwrap().id, Some(EventId(3)));
        let (after, skipped) = j.after(Some(EventId(1))).unwrap();
        assert_eq!(after.iter().map(|f| f.id.unwrap().0).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(skipped, 0);
        assert_eq!(j.after(None).unwrap().0.len(), 3);
    }

    #[test]
    fn a_torn_last_line_is_cut_before_the_next_append() {
        let p = dir("torn").join("journal.jsonl");
        let mut j = Journal::open(&p).unwrap();
        j.append(ev(1)).unwrap();
        drop(j);
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"id\":2,\"type\":\"be").unwrap();
        drop(f);
        let mut j = Journal::open(&p).unwrap();
        assert_eq!(j.head(), Some(EventId(1)));
        j.append(ev(2)).unwrap();
        let (all, skipped) = j.after(None).unwrap();
        assert_eq!((all.len(), skipped), (2, 0));
    }

    #[test]
    fn a_line_this_build_cannot_read_is_counted_not_fatal() {
        let p = dir("newer").join("journal.jsonl");
        fs::write(&p, "{\"id\":1,\"type\":\"seat.teleported\"}\n").unwrap();
        let mut j = Journal::open(&p).unwrap();
        assert_eq!(j.head(), Some(EventId(1)), "the head counts a line it cannot decode");
        j.append(ev(2)).unwrap();
        let (all, skipped) = j.after(None).unwrap();
        assert_eq!((all.len(), skipped), (1, 1));
    }
}
