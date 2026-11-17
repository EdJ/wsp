//! `wsp surface` — the panel drawn by somebody else.
//!
//! No host drives it any more, so what is left is the record a surface used to
//! leave: the last frame on disk, which `wsp peek panel` reads back.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::store::Store;


/// Where that file sits: beside the bindings and the daemon marker, because it
/// is machine state of exactly their kind — true only while a process is up,
/// and keyed on the state directory, so a sandbox's surface and the live one
/// do not paint over each other's picture.
///
/// Nothing tidies it away when a surface exits. A frame from a surface that
/// has gone is not misleading on its own; what would be is reading one as
/// live, and that is [`crate::daemon::surface_drawing`]'s question rather than
/// this file's.
pub(crate) fn frame_path(state: &Path) -> PathBuf {
    state.join("surface-frame.json")
}

/// The last frame a surface drew, as something to print.
pub(crate) struct Frame {
    /// The cells it was built for — the sidebar's rect, as the host reported
    /// it. Worth printing beside the frame: half of what this is used to find
    /// is a row costing more of them than it should.
    pub cols: usize,
    pub rows: usize,
    /// When it was drawn, ISO. The one field that says whether the picture is
    /// live: a surface writes only a frame that *changed*, so an old stamp
    /// means the sidebar has not moved — which is either nothing happening or
    /// the bug being looked for.
    pub at: String,
    /// One string per row, right-trimmed. The spans on disk carry colour, bold
    /// and the selection bar; nothing reads them yet, and a reader that wants
    /// to paint them has them without a surface change.
    pub lines: Vec<String>,
}

/// Read it back. `None` for a state directory no surface has ever drawn in, and
/// for a record this build cannot parse — a frame that cannot be read is not a
/// frame, and the caller has something to say either way.
pub(crate) fn last_frame(state: &Path) -> Option<Frame> {
    let raw = std::fs::read_to_string(frame_path(state)).ok()?;
    let record: Value = serde_json::from_str(&raw).ok()?;
    let lines = record
        .get("lines")?
        .as_array()?
        .iter()
        .map(|row| {
            let text: String = row
                .as_array()
                .map(|spans| {
                    spans.iter().filter_map(|s| s.get("s").and_then(Value::as_str)).collect()
                })
                .unwrap_or_default();
            // The padding is the wire's, so a selection bar reaches the edge of
            // a host's cells. On a page it is trailing whitespace.
            text.trim_end().to_string()
        })
        .collect();
    let cells = |name: &str| record.get(name).and_then(Value::as_u64).unwrap_or(0) as usize;
    Some(Frame {
        cols: cells("cols"),
        rows: cells("rows"),
        at: record.get("at").and_then(Value::as_str).unwrap_or_default().to_string(),
        lines,
    })
}

/// Run the panel against a host on stdin and stdout.
pub fn run(_store: &Store) -> i32 {
    eprintln!("wsp: no herdr socket");
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_left_on_disk_is_read_back_as_text_with_its_size_and_stamp() {
        let dir = std::env::temp_dir().join(format!("wsp-surface-frame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(last_frame(&dir).is_none(), "nothing has drawn here yet");

        let record = serde_json::json!({
            "at": "2026-11-17T00:00:00Z",
            "cols": 12,
            "rows": 4,
            "lines": [[{"s": "wsp"}, {"s": "   "}], [{"s": "fork-009"}]],
        });
        std::fs::write(frame_path(&dir), record.to_string()).unwrap();

        let frame = last_frame(&dir).expect("a frame was just written");
        assert_eq!((frame.cols, frame.rows), (12, 4));
        assert_eq!(frame.lines, vec!["wsp".to_string(), "fork-009".to_string()]);
        assert!(!frame.at.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
