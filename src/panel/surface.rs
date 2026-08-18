//! `wsp surface` — the panel drawn by somebody else.
//!
//! The panel has always been a pane: a tty it measured, escapes it painted,
//! and a pane id it asked herdr about. This is the same panel with none of
//! those, driven over a pipe by a host that owns the cells — herdr's forked
//! sidebar today, and whatever comes after it.
//!
//! # Why the surface and not the host draws
//!
//! The host could have linked wsp in and called `frame` itself. The reason it
//! does not is a deployment cost, and it is the whole argument for this file
//! existing: a herdr release build is five minutes and restarting herdr kills
//! every running agent, where `wsp install` swaps one file and costs nothing
//! that is running. Every row, colour and count that lives on this side of the
//! pipe is one that can be changed without either.
//!
//! So the split is: **wsp decides what the cells are, the host puts them on a
//! screen.** Nothing across the pipe knows what a task, a project or an agent
//! is, in either direction.
//!
//! # What crosses it
//!
//! One JSON object per line, both ways, and anything a side does not recognise
//! is ignored rather than fatal — so the two binaries can be upgraded in
//! either order, which given that one of them is expensive to rebuild is not a
//! nicety.
//!
//! In: `host` (the cells there are, whether the keyboard is here), `key` as it
//! was **typed**, `mouse` in the surface's own coordinates. A key arrives as a
//! code and a character rather than as [`Key`], deliberately: binding Tab here
//! tomorrow must not need a new host.
//!
//! Out: `frame`, as rows of spans carrying **attributes** rather than style
//! names, for the mirror-image reason — the palette is wsp's, and changing it
//! must not need a new host either.
//!
//! # What it is not
//!
//! Not a second panel. [`super::run::event_loop`] is the same loop the tty
//! runs, with a different [`Screen`] under it; the rows, the keys, the verbs
//! and the effects are the ones `wsp panel` has. If this file ever grows a
//! decision about what to draw, it is in the wrong place.

use std::io::{BufRead, Write};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::input::Key;
use crate::live::Live;
use crate::store::Store;

use super::render::{rgb_of, Line, Style};
use super::run::{event_loop, Msg, Screen};

/// The wire version this build speaks. Bumped only when a change is not
/// backwards compatible; most additions are not, because unknown messages are
/// dropped on both sides.
const PROTOCOL: u32 = 1;

/// Size to draw at before the host has said. It is `term_size`'s own fallback,
/// for the same reason: a first frame at a nonsense width is worse than a
/// first frame at a plausible one.
const UNTOLD: (usize, usize) = (26, 40);

/// What the host has told us, shared with the loop it is read by.
#[derive(Debug)]
struct Host {
    size: (usize, usize),
    /// Whether the host says the keyboard is in this surface. It is the answer
    /// to the question a pane has to ask herdr's census for, and a surface is
    /// simply told — which is the one place this is *better* than a pane and
    /// not merely different.
    focused: bool,
}

/// The panel drawn into a host's cells.
struct Wire {
    host: Arc<Mutex<Host>>,
    /// The frame already sent. A tick that changes nothing is most ticks, and
    /// a surface that resent its whole frame five times a second would put the
    /// cost the tty screen avoids onto a pipe instead.
    last: String,
}

impl Screen for Wire {
    fn size(&self) -> (usize, usize) {
        self.host.lock().map(|host| host.size).unwrap_or(UNTOLD)
    }

    fn paint(&mut self, lines: &[Line], w: usize, h: usize) -> (usize, usize) {
        let painted = frame_json(lines, w, h);
        if painted != self.last {
            println!("{painted}");
            let _ = std::io::stdout().flush();
            self.last = painted;
        }
        (w, h)
    }

    /// A surface is chrome: it is on screen whenever its host is drawing it,
    /// so it never falls to the background cadence the way a panel in a
    /// workspace nobody is looking at does. Whether it holds the keyboard is
    /// the host's word rather than a census question.
    fn focus(&self, _live: &Live, _me: Option<&str>, _ws: Option<&str>) -> (bool, bool) {
        let focused = self.host.lock().map(|host| host.focused).unwrap_or(false);
        (focused, true)
    }
}

/// A frame, as the host reads it.
///
/// Separate from [`Wire::paint`], which prints it, because this is the part
/// the two binaries have to agree on and printing is not something a test can
/// look at.
fn frame_json(lines: &[Line], w: usize, h: usize) -> String {
    let mut rows = Vec::with_capacity(lines.len().min(h));
    for line in lines.iter().take(h) {
        let mut line = line.clone();
        // Padded to the full width here rather than by the host, so an inverse
        // row is inverse to the edge: the selection is a bar, and a bar that
        // stops at the last character is a highlight.
        line.fit(w);
        let spans = line.spans.iter().map(|s| span(s.style, &s.text, line.selected)).collect();
        rows.push(Value::Array(spans));
    }
    json!({ "t": "frame", "lines": rows }).to_string()
}

/// One span, as attributes.
///
/// `selected` rides on every span of its row rather than on the row, because
/// the row is a list on the wire and an attribute on the row would be a second
/// shape for a host to handle for one flag.
fn span(style: Style, text: &str, selected: bool) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("s".into(), json!(text));
    if let Some((r, g, b)) = rgb_of(style) {
        out.insert("fg".into(), json!(format!("#{r:02x}{g:02x}{b:02x}")));
    }
    if let Style::Bold = style {
        out.insert("bold".into(), json!(true));
    }
    if let Style::Dim = style {
        out.insert("dim".into(), json!(true));
    }
    if selected {
        out.insert("inv".into(), json!(true));
    }
    Value::Object(out)
}

/// Read the host until it stops talking.
///
/// Every message is a `Msg::Tick` as well as whatever else it is: a host
/// message changes the size the next frame is built for, and a tick is how the
/// loop is told to build one without inventing a second kind of wake-up.
fn read_host(reader: impl BufRead, host: Arc<Mutex<Host>>, tx: Sender<Msg>) {
    let mut said_protocol = false;
    for line in reader.lines().map_while(Result::ok) {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match message.get("t").and_then(Value::as_str) {
            Some("host") => {
                // Said once, loudly, on the channel the host logs. A wire this
                // build cannot read is not something to keep drawing through:
                // the frames would be right and the keys silently wrong.
                if let Some(spoken) = message.get("protocol").and_then(Value::as_u64) {
                    if spoken != u64::from(PROTOCOL) && !said_protocol {
                        said_protocol = true;
                        eprintln!(
                            "wsp surface: host speaks protocol {spoken}, this build speaks {PROTOCOL}"
                        );
                    }
                }
                if let Ok(mut host) = host.lock() {
                    let cols = message.get("cols").and_then(Value::as_u64).unwrap_or(0);
                    let rows = message.get("rows").and_then(Value::as_u64).unwrap_or(0);
                    if cols > 0 && rows > 0 {
                        // The same floor `term_size` applies. A host that hands
                        // us four columns gets a frame built for sixteen and
                        // clipped, which is a bad picture; laying rows out in
                        // four columns is not a picture at all.
                        host.size = ((cols as usize).max(16), (rows as usize).max(6));
                    }
                    host.focused = message.get("focused").and_then(Value::as_bool).unwrap_or(false);
                }
            }
            Some("key") => {
                if let Some(key) = key_of(&message) {
                    if tx.send(Msg::Key(key)).is_err() {
                        return;
                    }
                    continue;
                }
            }
            Some("mouse") => {
                if let Some(key) = mouse_of(&message) {
                    if tx.send(Msg::Key(key)).is_err() {
                        return;
                    }
                    continue;
                }
            }
            // A message from a newer host. Dropped, not fatal.
            _ => continue,
        }
        if tx.send(Msg::Tick).is_err() {
            return;
        }
    }
    // The host has gone. Dropping the sender ends the loop's `recv`.
}

/// A typed key, given wsp's meaning.
///
/// The two control keys are recognised here rather than sent as themselves,
/// because that is where the panel's vocabulary is: `Key::Interrupt` and
/// `Key::KillLine` exist because a raw terminal delivers ctrl-c and ctrl-u as
/// bytes, and a host that has already decoded the keyboard would otherwise
/// have to know which control characters wsp happens to care about.
fn key_of(message: &Value) -> Option<Key> {
    let code = message.get("code").and_then(Value::as_str)?;
    let ctrl = message
        .get("mods")
        .and_then(Value::as_array)
        .is_some_and(|mods| mods.iter().any(|m| m.as_str() == Some("ctrl")));
    Some(match code {
        "char" => {
            let ch = message.get("ch").and_then(Value::as_str)?.chars().next()?;
            match (ctrl, ch.to_ascii_lowercase()) {
                (true, 'c') => Key::Interrupt,
                (true, 'u') => Key::KillLine,
                (true, _) => return None,
                (false, _) => Key::Char(ch),
            }
        }
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "enter" => Key::Enter,
        "esc" => Key::Esc,
        "backspace" => Key::Backspace,
        "home" => Key::Home,
        "end" => Key::End,
        // Everything else a keyboard can send, and nothing here binds. Dropped
        // rather than mapped onto whatever is nearest: a key that does the
        // wrong thing is worse than a key that does nothing.
        _ => return None,
    })
}

/// A press or the wheel, in the surface's own cells.
///
/// Only a left press becomes a click. The panel has no use for a release, a
/// drag or another button, and forwarding them would put three more shapes
/// through `keys::click` for nothing.
fn mouse_of(message: &Value) -> Option<Key> {
    let kind = message.get("kind").and_then(Value::as_str)?;
    let at = |name: &str| message.get(name).and_then(Value::as_u64).unwrap_or(0) as usize;
    Some(match kind {
        "down" if message.get("button").and_then(Value::as_str) == Some("left") => {
            Key::Click { x: at("col"), y: at("row") }
        }
        "scrollup" => Key::Wheel { up: true },
        "scrolldown" => Key::Wheel { up: false },
        _ => return None,
    })
}

/// Run the panel against a host on stdin and stdout.
pub fn run(store: &Store) -> i32 {
    if !crate::herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let host = Arc::new(Mutex::new(Host { size: UNTOLD, focused: false }));
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    super::run::spawn_events(tx.clone());
    {
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if tx.send(Msg::Tick).is_err() {
                return;
            }
        });
    }
    {
        let host = Arc::clone(&host);
        let tx = tx.clone();
        std::thread::spawn(move || read_host(std::io::stdin().lock(), host, tx));
    }

    // No workspace and no pane: that is what a surface *is*, and every effect
    // that would have named one already takes an `Option`.
    let mut screen = Wire { host, last: String::new() };
    event_loop(store, &tx, &rx, None, false, &mut screen);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn a_key_arrives_as_it_was_typed_and_is_given_its_meaning_here() {
        assert!(matches!(
            key_of(&message(r#"{"code":"char","ch":"j","mods":[]}"#)),
            Some(Key::Char('j'))
        ));
        assert!(matches!(
            key_of(&message(r#"{"code":"char","ch":"c","mods":["ctrl"]}"#)),
            Some(Key::Interrupt)
        ));
        assert!(matches!(
            key_of(&message(r#"{"code":"esc","mods":[]}"#)),
            Some(Key::Esc)
        ));
    }

    #[test]
    fn a_key_this_wsp_does_not_bind_is_dropped_rather_than_guessed_at() {
        assert!(key_of(&message(r#"{"code":"f","ch":"7","mods":[]}"#)).is_none());
        assert!(key_of(&message(r#"{"code":"tab","mods":[]}"#)).is_none());
        assert!(key_of(&message(r#"{"code":"char","ch":"k","mods":["ctrl"]}"#)).is_none());
    }

    #[test]
    fn the_wheel_and_a_left_press_are_the_only_mouse_the_panel_has_a_use_for() {
        assert!(matches!(
            mouse_of(&message(r#"{"kind":"down","button":"left","col":3,"row":7}"#)),
            Some(Key::Click { x: 3, y: 7 })
        ));
        assert!(matches!(
            mouse_of(&message(r#"{"kind":"scrollup","col":0,"row":0}"#)),
            Some(Key::Wheel { up: true })
        ));
        assert!(mouse_of(&message(r#"{"kind":"up","button":"left","col":3,"row":7}"#)).is_none());
        assert!(mouse_of(&message(r#"{"kind":"down","button":"right","col":3,"row":7}"#)).is_none());
    }

    #[test]
    fn a_host_message_sets_the_size_the_next_frame_is_built_for() {
        let host = Arc::new(Mutex::new(Host { size: UNTOLD, focused: false }));
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        let line = format!(
            "{{\"t\":\"host\",\"protocol\":{PROTOCOL},\"cols\":34,\"rows\":50,\"focused\":true}}\n"
        );

        read_host(line.as_bytes(), Arc::clone(&host), tx);

        let told = host.lock().unwrap();
        assert_eq!(told.size, (34, 50));
        assert!(told.focused);
        assert!(
            matches!(rx.recv(), Ok(Msg::Tick)),
            "a resize has to wake the loop, or the frame stays the old size"
        );
    }

    #[test]
    fn a_narrow_host_gets_a_clipped_frame_rather_than_a_layout_in_four_columns() {
        let host = Arc::new(Mutex::new(Host { size: UNTOLD, focused: false }));
        let (tx, _rx) = std::sync::mpsc::channel::<Msg>();

        read_host(
            "{\"t\":\"host\",\"cols\":4,\"rows\":2}\n".as_bytes(),
            Arc::clone(&host),
            tx,
        );

        assert_eq!(host.lock().unwrap().size, (16, 6));
    }

    #[test]
    fn a_message_from_a_newer_host_is_ignored_rather_than_fatal() {
        let host = Arc::new(Mutex::new(Host { size: UNTOLD, focused: false }));
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();

        read_host(
            concat!(
                "{\"t\":\"weather\",\"outlook\":\"fine\"}\n",
                "not json at all\n",
                "{\"t\":\"host\",\"cols\":30,\"rows\":40}\n",
            )
            .as_bytes(),
            Arc::clone(&host),
            tx,
        );

        assert_eq!(
            host.lock().unwrap().size,
            (30, 40),
            "the reader has to get past what it does not understand"
        );
        assert!(rx.recv().is_ok());
    }

    #[test]
    fn a_selected_row_is_inverse_to_the_edge_and_a_style_carries_its_colour() {
        let mut line = super::super::render::line(Style::Accent, "wsp");
        line.selected = true;

        let frame: Value = serde_json::from_str(&frame_json(&[line], 10, 4)).unwrap();
        let spans = frame["lines"][0].as_array().unwrap();

        assert_eq!(spans[0]["s"], json!("wsp"));
        assert_eq!(spans[0]["fg"], json!("#5fbfa4"));
        assert_eq!(spans[0]["inv"], json!(true));
        assert!(
            spans.iter().all(|s| s["inv"] == json!(true)),
            "the selection is a bar: a row inverse only up to its last character is a highlight"
        );
        assert_eq!(
            spans.iter().map(|s| s["s"].as_str().unwrap_or_default().chars().count()).sum::<usize>(),
            10,
            "the row is padded here rather than by the host, so the bar reaches the edge"
        );
    }

    #[test]
    fn a_frame_is_clipped_to_the_rows_the_host_has() {
        let lines: Vec<Line> = (0..9)
            .map(|i| super::super::render::line(Style::Plain, format!("row {i}")))
            .collect();

        let frame: Value = serde_json::from_str(&frame_json(&lines, 10, 4)).unwrap();

        assert_eq!(frame["lines"].as_array().unwrap().len(), 4);
    }
}
