//! Hooks report to the manager (`docs/agent-manager.md` 6.2, M1 group 3;
//! wsp-023).
//!
//! Two halves, both here so that the agent's hook command and the manager
//! read one definition:
//! - **the post** ([`report`]): the hook command (`wsp report <hook>`) dials
//!   its host's manager on loopback, sends one [`POST`] line carrying the
//!   manager token, the seat, its epoch, the hook's name and its payload, and
//!   reads one answer line. When `WSP_HOST` is `inproc` ([`Dial::Inproc`]; the
//!   default is `manager` since wsp-029), or the manager does not answer, it says so
//!   ([`Sent`]) and the caller appends to the seat's hook log as before;
//! - **the fold** ([`Fold`]): the manager turns a post, and the transcript it
//!   names, into the journal's events: `turn` began (with the order it was
//!   told to expect, [`Fold::awaiting`]) or ended, `seat.spent` (the burn
//!   tally, [`crate::inproc::tally_burn`]), `seat.said` (the turn's last
//!   assistant text) and `seat.did` (a digest of the turn, with the
//!   transcript's `(host, path)`).
//!
//! The post is the manager's **hooks endpoint** (2.1), a host-local line
//! beside the frames of 3.2, not one of them: the agent and the manager are on
//! one host, and no group of M1 changes the contract crate (6.2). It is told
//! apart from a frame by its `type`, [`POST`], which names no
//! [`wsp_host_api::Message`]. A connection that posts is answered and closed.
//!
//! `seat.stuck` (a command waiting on itself) is not a hook's to say: no hook
//! fires while a command runs, so it waits for the manager's seat table.

use std::collections::BTreeMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use wsp_host_api::{
    At, Epoch, FileRef, HostId, Message, OrderId, SeatDid, SeatId, SeatSaid, SeatSpent, Tokens, Turn, TurnEdge,
};

use crate::inproc::{read_json, str_of, tally_burn, InProcess, BURN_FILE};
use crate::price::cost_of;
use crate::tcp::Tcp;

/// The port a manager listens on unless told otherwise.
pub const PORT: u16 = 7487;
/// The token's file under the manager's `<exec_dir>`.
pub const TOKEN_FILE: &str = "manager.token";
/// `manager` (the default, wsp-029) or `inproc`: whether the hook posts at all.
pub const HOST_ENV: &str = "WSP_HOST";
/// The manager's address, `127.0.0.1:`[`PORT`] when unset.
pub const ADDR_ENV: &str = "WSP_MANAGER_ADDR";
/// The manager's `<exec_dir>`, `~/.local/state/wsp-manager` when unset.
pub const DIR_ENV: &str = "WSP_MANAGER_DIR";
/// The seat's start epoch, as the manager that started it set it; `0` for a
/// seat no manager started.
pub const EPOCH_ENV: &str = "WSP_SEAT_EPOCH";
/// The post's `type`, and the answers' (`taken`, `refused`).
pub const POST: &str = "hook.post";
pub const TAKEN: &str = "hook.taken";
pub const REFUSED: &str = "hook.refused";
/// Under the manager's `<exec_dir>`: a directory per seat, its burn tally and
/// the session it reports as.
pub const SEATS_DIR: &str = "seats";
/// Under a seat's directory: the session that stands in the seat.
const SESSION_FILE: &str = "session.json";
/// How long the hook waits for the manager to accept, then to answer. A
/// loopback manager accepts at once and folds in milliseconds; a hook runs
/// under a 5 s limit, and the fallback has to fit inside it.
pub const DIAL_WAIT: Duration = Duration::from_millis(500);
pub const ANSWER_WAIT: Duration = Duration::from_secs(3);
/// How much of a transcript's tail the fold reads for one turn.
const TAIL_BYTES: u64 = 4 * 1024 * 1024;
/// The longest last word journalled, in characters.
const SAID_CHARS: usize = 4000;

/// The manager's `<exec_dir>`: `$WSP_MANAGER_DIR`, else `~/.local/state/wsp-manager`.
pub fn manager_dir() -> PathBuf {
    match std::env::var_os(DIR_ENV) {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => wsp_base::util::home().join(".local/state/wsp-manager"),
    }
}

/// [`EPOCH_ENV`], read; `0` when unset or unreadable.
pub fn epoch_from_env() -> Epoch {
    Epoch(std::env::var(EPOCH_ENV).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0))
}

/// Where a hook goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dial {
    /// `WSP_HOST=inproc`: the seat's hook log, as at M0.
    Inproc,
    /// `WSP_HOST` unset or `manager`: the manager at `addr`, greeted with the token in
    /// `token_file`.
    Manager { addr: SocketAddr, token_file: PathBuf },
}

impl Dial {
    /// [`HOST_ENV`], [`ADDR_ENV`] and [`DIR_ENV`], read.
    pub fn from_env() -> Dial {
        Dial::of(std::env::var(HOST_ENV).ok().as_deref(), std::env::var(ADDR_ENV).ok().as_deref())
    }

    /// [`HOST_ENV`] and [`ADDR_ENV`] as given: only `inproc` keeps the hook
    /// off the wire, so unset is the manager (wsp-029).
    pub fn of(host: Option<&str>, addr: Option<&str>) -> Dial {
        if host.map(str::trim) == Some("inproc") {
            return Dial::Inproc;
        }
        let addr = addr
            .and_then(|a| a.trim().parse().ok())
            .unwrap_or(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), PORT));
        Dial::Manager { addr, token_file: manager_dir().join(TOKEN_FILE) }
    }
}

/// What became of a hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    /// The manager took it and journalled this many events.
    Taken(u64),
    /// Not posted: [`Dial::Inproc`].
    Inproc,
    /// Not taken, and why: no token, no answer, a refusal.
    Unanswered(String),
}

/// One hook, as the hook command posts it.
#[derive(Debug, Clone, PartialEq)]
pub struct Post {
    pub token: String,
    pub seat: SeatId,
    pub epoch: Epoch,
    pub hook: String,
    pub payload: Value,
}

impl Post {
    /// The line on the wire, without its newline.
    pub fn line(&self) -> String {
        json!({
            "type": POST,
            "token": self.token,
            "seat": self.seat.0,
            "epoch": self.epoch.0,
            "hook": self.hook,
            "payload": self.payload,
        })
        .to_string()
    }

    /// A post out of a line already read as JSON; `None` for anything that is
    /// not one ([`is_post`]), an error for a post missing what it must carry.
    pub fn read(v: &Value) -> Option<Result<Post, String>> {
        if !is_post(v) {
            return None;
        }
        let seat = str_of(v, "seat");
        let hook = str_of(v, "hook");
        Some(if seat.is_empty() || hook.is_empty() {
            Err("a hook post names its seat and its hook".into())
        } else {
            Ok(Post {
                token: str_of(v, "token"),
                seat: SeatId(seat),
                epoch: Epoch(v.get("epoch").and_then(Value::as_u64).unwrap_or(0)),
                hook,
                payload: v.get("payload").cloned().unwrap_or(json!({})),
            })
        })
    }
}

/// Whether a line read as JSON is a hook post rather than a frame.
pub fn is_post(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some(POST)
}

/// The manager's answer to a post it took.
pub fn taken(events: u64) -> String {
    json!({ "type": TAKEN, "events": events }).to_string()
}

/// The manager's answer to a post it did not take.
pub fn refused(why: &str) -> String {
    json!({ "type": REFUSED, "why": why }).to_string()
}

/// Post one hook to the manager `dial` names. Never blocks past
/// [`DIAL_WAIT`] + [`ANSWER_WAIT`], and never fails the hook: what did not
/// arrive is [`Sent::Unanswered`], for the caller to write down instead.
pub fn report(dial: &Dial, seat: &str, epoch: Epoch, hook: &str, payload: &Value) -> Sent {
    let Dial::Manager { addr, token_file } = dial else { return Sent::Inproc };
    let token = match fs::read_to_string(token_file) {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        Ok(_) => return Sent::Unanswered(format!("{} is empty", token_file.display())),
        Err(e) => return Sent::Unanswered(format!("no token at {}: {e}", token_file.display())),
    };
    let post = Post { token, seat: SeatId(seat.to_string()), epoch, hook: hook.to_string(), payload: payload.clone() };
    let mut conn = match Tcp::dial(*addr, DIAL_WAIT) {
        Ok(c) => c,
        Err(e) => return Sent::Unanswered(format!("no manager at {addr}: {e:?}")),
    };
    let sent = conn.send_line(&format!("{}\n", post.line()));
    let said = sent.and_then(|()| conn.recv_line(ANSWER_WAIT));
    conn.shutdown();
    let line = match said {
        Ok(Some(line)) => line,
        Ok(None) => return Sent::Unanswered(format!("the manager at {addr} did not answer in {ANSWER_WAIT:?}")),
        Err(e) => return Sent::Unanswered(format!("the manager at {addr}: {e:?}")),
    };
    let v: Value = serde_json::from_slice(&line).unwrap_or(Value::Null);
    match v.get("type").and_then(Value::as_str) {
        Some(TAKEN) => Sent::Taken(v.get("events").and_then(Value::as_u64).unwrap_or(0)),
        Some(REFUSED) => Sent::Unanswered(format!("the manager refused it: {}", str_of(&v, "why"))),
        _ => Sent::Unanswered(format!("the manager at {addr} answered something else: {}", String::from_utf8_lossy(&line))),
    }
}

/// Whether a hook came from a session other than the one standing in the seat
/// (`wsp-364`): a `claude -p` that the agent, or a script it ran, started from
/// inside the seat. The child inherits `WSP_SEAT_ID`, so its hooks arrive
/// naming the parent's seat, and its `SessionEnd` used to make the seat read
/// gone two minutes into a ten-minute turn.
///
/// The seat's session is whichever one reported first — `was`'s
/// `session_id`, written by the first hook on a fresh seat and kept by every
/// one after. A hook naming another session is a child's and says nothing
/// about the seat: not its state, not its end.
///
/// | `was` | the hook | a child's? |
/// |---|---|---|
/// | no session yet | any | no — the first to report takes the seat |
/// | the seat's session said `SessionEnd` | any | no — `/clear` ends one session and starts the next in the same agent |
/// | session A | `source: "clear"` | no — the same `/clear`, its start seen first |
/// | session A | no `session_id` | no — nothing to judge it by, so it is believed, as before |
/// | session A | session A | no |
/// | session A | session B | **yes** |
///
/// Moved down from wire's `place_super` (wsp-023), which re-exports it, so the
/// manager's fold and the hook log judge a child the same way.
pub fn a_childs(was: &Value, payload: &Value) -> bool {
    let theirs = str_of(payload, "session_id");
    let seats = str_of(was, "session_id");
    // `/clear` starts its new session with `source: "clear"`; a `claude -p`
    // child starts with `startup`. Either order of `/clear`'s two hooks lands.
    let cleared = str_of(payload, "source") == "clear";
    if theirs.is_empty() || seats.is_empty() || cleared || str_of(was, "state") == "gone" {
        return false;
    }
    theirs != seats
}

/// A seat id the fold may make a directory of: a plain file name.
fn plain(id: &str) -> bool {
    !id.is_empty() && !id.starts_with('.') && !id.contains('/') && !id.contains('\\') && id.len() < 128
}

/// The manager's side: hooks in, events out.
pub struct Fold {
    root: PathBuf,
    host: HostId,
    awaiting: BTreeMap<SeatId, OrderId>,
    inproc: InProcess,
}

impl Fold {
    /// A fold keeping its per-seat files under `<exec_dir>/`[`SEATS_DIR`], and
    /// naming transcripts as on `host`.
    pub fn new(exec_dir: &Path, host: HostId) -> Fold {
        Fold { root: exec_dir.join(SEATS_DIR), host, awaiting: BTreeMap::new(), inproc: InProcess::new() }
    }

    /// The next turn `seat` begins is `order`'s: its ack (3.2's `order.acked`
    /// is the same edge). The manager's order delivery says it before it asks
    /// compound-sup, whose `delivered` comes only after the agent's
    /// `UserPromptSubmit` hook has fired.
    pub fn awaiting(&mut self, seat: SeatId, order: OrderId) {
        self.awaiting.insert(seat, order);
    }

    /// `order` was not delivered: no turn of `seat`'s is its. A later order
    /// awaited for the seat is kept.
    pub fn not_awaiting(&mut self, seat: &SeatId, order: &OrderId) {
        if self.awaiting.get(seat) == Some(order) {
            self.awaiting.remove(seat);
        }
    }

    /// The events one post makes, oldest first. A nested session's hook makes
    /// none; a hook that says nothing of turns or spend makes none.
    pub fn fold(&mut self, post: &Post, at: At) -> Result<Vec<Message>, String> {
        if !plain(&post.seat.0) {
            return Err(format!("{:?} is not a seat name", post.seat.0));
        }
        let dir = self.root.join(&post.seat.0);
        fs::create_dir_all(&dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
        let session_file = dir.join(SESSION_FILE);
        let was = read_json(&session_file);
        if a_childs(&was, &post.payload) {
            return Ok(Vec::new());
        }
        let hook = post.hook.trim();
        let keep = |key: &str| match str_of(&post.payload, key) {
            s if s.is_empty() => str_of(&was, key),
            s => s,
        };
        let transcript = keep("transcript_path");
        let now = json!({
            "session_id": keep("session_id"),
            "transcript_path": transcript,
            "state": if hook == "SessionEnd" { "gone" } else { "live" },
            "hook": hook,
        });
        let _ = crate::inproc::write_atomic(&session_file, &now.to_string());

        let (seat, epoch) = (post.seat.clone(), post.epoch);
        let turn = |edge, order| Message::Turn(Turn { seat: seat.clone(), epoch, edge, at, order });
        let mut out = Vec::new();
        match hook {
            "UserPromptSubmit" => out.push(turn(TurnEdge::Began, self.awaiting.remove(&seat))),
            "PostToolUse" => out.extend(self.spent(&dir, post)),
            "Stop" | "StopFailure" => {
                out.extend(self.spent(&dir, post));
                let read = (!transcript.is_empty())
                    .then(|| self.inproc.transcript_tail(Path::new(&transcript), TAIL_BYTES))
                    .flatten();
                if let Some(text) = read {
                    let t = turn_of(&text);
                    if let Some(said) = t.said {
                        out.push(Message::SeatSaid(SeatSaid { seat: seat.clone(), epoch, at, text: said }));
                    }
                    out.push(Message::SeatDid(SeatDid {
                        seat: seat.clone(),
                        epoch,
                        at,
                        digest: t.digest,
                        transcript: FileRef { host: self.host.clone(), path: transcript.clone() },
                    }));
                }
                out.push(turn(TurnEdge::Ended, None));
            }
            _ => {}
        }
        Ok(out)
    }

    /// The seat's spend after tallying what the transcript gained, if it moved.
    fn spent(&self, dir: &Path, post: &Post) -> Option<Message> {
        let counters = |r: &Value| {
            let n = |k: &str| r.get(k).and_then(Value::as_u64).unwrap_or(0);
            (str_of(r, "session_id"), n("input"), n("output"), n("cache_read"), n("cache_write"), n("turns"))
        };
        let burn = dir.join(BURN_FILE);
        let before = counters(&read_json(&burn));
        tally_burn(&dir.to_path_buf(), &post.payload);
        let rec = read_json(&burn);
        let after = counters(&rec);
        if after == before || after.5 == 0 {
            return None;
        }
        Some(Message::SeatSpent(SeatSpent {
            seat: post.seat.clone(),
            epoch: post.epoch,
            tokens: Tokens { input: after.1, output: after.2, cache_read: after.3, cache_write: after.4 },
            cost_micros: cost_of(&rec),
        }))
    }
}

/// One turn read out of a Claude Code transcript's tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRead {
    /// The turn's last assistant text.
    pub said: Option<String>,
    /// What it was asked, which tools it ran and what it edited, in a line.
    pub digest: String,
}

/// The last turn in `text` (JSON lines): everything after the last prompt a
/// person or wsp typed (a `user` line that is not a tool's result).
pub fn turn_of(text: &str) -> TurnRead {
    let lines: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let prompt_at = lines.iter().rposition(is_prompt);
    let (asked, rest) = match prompt_at {
        Some(i) => (prompt_text(&lines[i]), &lines[i + 1..]),
        None => (String::new(), &lines[..]),
    };
    let mut tools: BTreeMap<String, u64> = BTreeMap::new();
    let mut edited: Vec<String> = Vec::new();
    let mut said = None;
    for line in rest.iter().filter(|l| str_of(l, "type") == "assistant") {
        let Some(blocks) = line.pointer("/message/content").and_then(Value::as_array) else { continue };
        for b in blocks {
            match str_of(b, "type").as_str() {
                "text" => {
                    let t = str_of(b, "text");
                    if !t.trim().is_empty() {
                        said = Some(t.trim().to_string());
                    }
                }
                "tool_use" => {
                    let name = str_of(b, "name");
                    *tools.entry(name.clone()).or_default() += 1;
                    if matches!(name.as_str(), "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
                        let file = b.get("input").map(|i| str_of(i, "file_path")).unwrap_or_default();
                        if !file.is_empty() && !edited.contains(&file) {
                            edited.push(file);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut parts = Vec::new();
    if !asked.is_empty() {
        parts.push(format!("asked: {}", clip(&asked.split_whitespace().collect::<Vec<_>>().join(" "), 80)));
    }
    parts.push(match tools.is_empty() {
        true => "no tools".to_string(),
        false => format!("tools: {}", tools.iter().map(|(n, c)| format!("{n} {c}")).collect::<Vec<_>>().join(", ")),
    });
    if !edited.is_empty() {
        parts.push(format!("edited: {}", edited.join(", ")));
    }
    TurnRead { said: said.map(|s| clip(&s, SAID_CHARS)), digest: parts.join("; ") }
}

/// A `user` line that is a prompt rather than a tool's result or Claude
/// Code's own meta line.
fn is_prompt(v: &Value) -> bool {
    if str_of(v, "type") != "user" || v.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    match v.pointer("/message/content") {
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(blocks)) => {
            blocks.iter().any(|b| str_of(b, "type") == "text") && !blocks.iter().any(|b| str_of(b, "type") == "tool_result")
        }
        _ => false,
    }
}

fn prompt_text(v: &Value) -> String {
    match v.pointer("/message/content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            blocks.iter().filter(|b| str_of(b, "type") == "text").map(|b| str_of(b, "text")).collect::<Vec<_>>().join(" ")
        }
        _ => String::new(),
    }
}

/// At most `n` characters of `s`, with an ellipsis when cut.
fn clip(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_wsp_host_posts_to_the_manager() {
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), PORT);
        assert!(matches!(Dial::of(None, None), Dial::Manager { addr, .. } if addr == loopback));
        assert!(matches!(Dial::of(Some(""), None), Dial::Manager { .. }));
        assert!(matches!(Dial::of(Some("manager"), Some("127.0.0.1:9")), Dial::Manager { addr, .. } if addr.port() == 9));
        assert_eq!(Dial::of(Some("inproc"), None), Dial::Inproc);
        assert_eq!(Dial::of(Some(" inproc\n"), None), Dial::Inproc);
    }

    #[test]
    fn a_post_crosses_as_one_line_and_a_frame_is_not_one() {
        let p = Post {
            token: "t".into(),
            seat: SeatId("cpd-1".into()),
            epoch: Epoch(2),
            hook: "Stop".into(),
            payload: json!({ "session_id": "s" }),
        };
        let v: Value = serde_json::from_str(&p.line()).unwrap();
        assert_eq!(Post::read(&v), Some(Ok(p)));
        assert_eq!(Post::read(&json!({ "type": "beat", "at": 1 })), None);
        assert!(matches!(Post::read(&json!({ "type": POST, "hook": "Stop" })), Some(Err(_))));
    }

    #[test]
    fn the_last_turn_is_read_from_the_last_prompt_on() {
        let text = [
            json!({ "type": "user", "message": { "content": "an older turn" } }),
            json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "old answer" }] } }),
            json!({ "type": "user", "message": { "content": [{ "type": "text", "text": "fix   the\nbug" }] } }),
            json!({ "type": "assistant", "message": { "content": [
                { "type": "tool_use", "name": "Edit", "input": { "file_path": "src/a.rs" } },
                { "type": "tool_use", "name": "Bash", "input": { "command": "cargo test" } },
            ] } }),
            json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "content": "ok" }] } }),
            json!({ "type": "assistant", "message": { "content": [
                { "type": "tool_use", "name": "Edit", "input": { "file_path": "src/a.rs" } },
                { "type": "text", "text": "  Fixed it.  " },
            ] } }),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        let t = turn_of(&format!("{text}\n{{torn"));
        assert_eq!(t.said.as_deref(), Some("Fixed it."));
        assert_eq!(t.digest, "asked: fix the bug; tools: Bash 1, Edit 2; edited: src/a.rs");
        assert_eq!(turn_of("").digest, "no tools");
    }

    #[test]
    fn a_childs_hook_is_told_apart_by_its_session() {
        let was = json!({ "session_id": "A", "state": "live" });
        assert!(!a_childs(&was, &json!({ "session_id": "A" })));
        assert!(a_childs(&was, &json!({ "session_id": "B" })));
        assert!(!a_childs(&was, &json!({ "session_id": "B", "source": "clear" })));
        assert!(!a_childs(&json!({ "session_id": "A", "state": "gone" }), &json!({ "session_id": "B" })));
        assert!(!a_childs(&json!({}), &json!({ "session_id": "B" })));
    }
}
