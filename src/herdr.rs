//! Newline-delimited JSON-RPC client for herdr's unix socket.
//!
//! Verified against the live server: requests are `{"id","method","params"}\n`,
//! replies `{"id","result"}\n`. `events.subscribe` replies once with
//! `{"type":"subscription_started"}` and then streams
//! `{"event":"workspace_focused","data":{…}}` lines on the same connection —
//! note the stream uses underscores where subscriptions use dots.
//!
//! One subscription is refused unless it names a pane: `pane.agent_status_changed`
//! is per-pane, and its request struct requires `pane_id` — there is no wildcard,
//! `*` and `""` both answer `pane_not_found`. A subscription list is validated as
//! a whole, so one entry missing its `pane_id` refuses every other entry with it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use crate::util;

pub fn socket_path() -> PathBuf {
    match std::env::var_os("HERDR_SOCKET_PATH") {
        Some(v) => PathBuf::from(v),
        None => util::home().join(".config/herdr/herdr.sock"),
    }
}

pub fn available() -> bool {
    socket_path().exists()
}

fn connect(timeout: Option<Duration>) -> std::io::Result<UnixStream> {
    let s = UnixStream::connect(socket_path())?;
    s.set_read_timeout(timeout)?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    Ok(s)
}

/// One request, one reply.
pub fn call(method: &str, params: Value) -> std::io::Result<Value> {
    let mut s = connect(Some(Duration::from_secs(3)))?;
    let req = json!({ "id": format!("wsp:{}", util::epoch_nanos()), "method": method, "params": params });
    s.write_all(format!("{req}\n").as_bytes())?;
    s.flush()?;

    let mut reader = BufReader::new(s);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let v: Value = serde_json::from_str(line.trim())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Some(err) = v.get("error") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("herdr error: {err}"),
        ));
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

/// Mirrors herdr's wire payload; fields we don't read yet are kept so the
/// struct stays a faithful record of what the server sends.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct Workspace {
    pub id: String,
    pub label: String,
    pub number: i64,
    pub focused: bool,
    pub agent_status: String,
    pub tokens: Value,
}

/// A pane. Some panes are running an agent; most are a shell someone opened.
/// The panel needs both, because a shell sitting in a project is a fact about
/// that project whether or not an agent ever attaches to it.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct Pane {
    pub pane_id: String,
    /// herdr's own label for the pane — `wsp` marks one of our panels.
    pub label: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub agent: String,
    pub agent_status: String,
    pub cwd: String,
    pub title: String,
    pub focused: bool,
    pub session_id: String,
}

pub fn workspaces() -> std::io::Result<Vec<Workspace>> {
    let r = call("workspace.list", json!({}))?;
    let arr = r.get("workspaces").and_then(|w| w.as_array()).cloned().unwrap_or_default();
    Ok(arr
        .iter()
        .map(|w| Workspace {
            id: sget(w, "workspace_id"),
            label: sget(w, "label"),
            number: w.get("number").and_then(|n| n.as_i64()).unwrap_or(0),
            focused: w.get("focused").and_then(|b| b.as_bool()).unwrap_or(false),
            agent_status: sget(w, "agent_status"),
            tokens: w.get("tokens").cloned().unwrap_or(Value::Null),
        })
        .collect())
}

fn parse_pane(a: &Value) -> Pane {
    Pane {
        pane_id: sget(a, "pane_id"),
        label: sget(a, "label"),
        workspace_id: sget(a, "workspace_id"),
        tab_id: sget(a, "tab_id"),
        agent: sget(a, "agent"),
        agent_status: sget(a, "agent_status"),
        cwd: match sget(a, "foreground_cwd") {
            fg if !fg.is_empty() => fg,
            _ => sget(a, "cwd"),
        },
        title: sget(a, "terminal_title_stripped"),
        focused: a.get("focused").and_then(|b| b.as_bool()).unwrap_or(false),
        session_id: a
            .get("agent_session")
            .and_then(|s| s.get("value"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

/// Every pane herdr knows about, agent or not.
pub fn panes() -> std::io::Result<Vec<Pane>> {
    let r = call("pane.list", json!({}))?;
    let arr = r.get("panes").and_then(|w| w.as_array()).cloned().unwrap_or_default();
    Ok(arr.iter().map(parse_pane).collect())
}

/// Only the panes running an agent.
pub fn agents() -> std::io::Result<Vec<Pane>> {
    let r = call("agent.list", json!({}))?;
    let arr = r.get("agents").and_then(|w| w.as_array()).cloned().unwrap_or_default();
    Ok(arr.iter().map(parse_pane).collect())
}

fn sget(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// Give a workspace a name. This is the durable `custom_name` in herdr's
/// session file, and it is what the sidebar draws.
///
/// A workspace nobody has named has no name to read back: `workspace.list`
/// answers with the agent standing in it, or the folder leaf, so three agents
/// in one tree all come back as `claude`. Setting it is the only way to tell
/// them apart, and there is no way to ask which of the two you are looking at.
pub fn rename_workspace(workspace_id: &str, label: &str) -> std::io::Result<()> {
    call("workspace.rename", json!({ "workspace_id": workspace_id, "label": label })).map(|_| ())
}

/// Name a pane. Unlike a workspace this one reads back as itself — an unnamed
/// pane has an empty `label`, which is how the panel finds its own.
pub fn rename_pane(pane_id: &str, label: &str) -> std::io::Result<()> {
    call("pane.rename", json!({ "pane_id": pane_id, "label": label })).map(|_| ())
}

/// Display-only metadata. Values are capped at 16 keys by the server, and the
/// TTL cannot exceed 24h — hence the daemon's refresh loop.
pub fn report_workspace_tokens(
    workspace_id: &str,
    tokens: &[(&str, Option<String>)],
    ttl_ms: u64,
) -> std::io::Result<()> {
    let mut map = serde_json::Map::new();
    for (k, v) in tokens.iter().take(16) {
        map.insert(
            (*k).to_string(),
            match v {
                Some(s) => Value::String(s.clone()),
                None => Value::Null,
            },
        );
    }
    call(
        "workspace.report_metadata",
        json!({
            "workspace_id": workspace_id,
            "source": "wsp",
            "seq": util::epoch_nanos(),
            "tokens": Value::Object(map),
            "ttl_ms": ttl_ms,
        }),
    )
    .map(|_| ())
}

pub fn report_pane_tokens(
    pane_id: &str,
    tokens: &[(&str, Option<String>)],
    ttl_ms: u64,
) -> std::io::Result<()> {
    let mut map = serde_json::Map::new();
    for (k, v) in tokens.iter().take(16) {
        map.insert(
            (*k).to_string(),
            match v {
                Some(s) => Value::String(s.clone()),
                None => Value::Null,
            },
        );
    }
    call(
        "pane.report_metadata",
        json!({
            "pane_id": pane_id,
            "source": "wsp",
            "seq": util::epoch_nanos(),
            "tokens": Value::Object(map),
            "ttl_ms": ttl_ms,
        }),
    )
    .map(|_| ())
}

/// Blocking event stream. `f` is called per event with (event_name, data);
/// returning false ends the subscription.
pub fn subscribe<F>(types: &[&str], mut f: F) -> std::io::Result<()>
where
    F: FnMut(&str, &Value) -> bool,
{
    let subs: Vec<Value> = types.iter().map(|t| json!({ "type": t })).collect();
    let mut s = connect(None)?;
    let req = json!({
        "id": format!("wsp:sub:{}", util::epoch_nanos()),
        "method": "events.subscribe",
        "params": { "subscriptions": subs },
    });
    s.write_all(format!("{req}\n").as_bytes())?;
    s.flush()?;

    let reader = BufReader::new(s);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        // A refused subscription arrives as one `error` reply and then the
        // server hangs up. Without this the stream simply ends, which is
        // indistinguishable from a clean close: the caller retries fast, sees
        // nothing, and goes on seeing nothing. It cost this whole feature.
        if let Some(e) = v.get("error") {
            let msg = e.get("message").and_then(|m| m.as_str()).unwrap_or("refused");
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("events.subscribe: {msg}"),
            ));
        }
        let Some(event) = v.get("event").and_then(|e| e.as_str()) else { continue };
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        if !f(event, &data) {
            break;
        }
    }
    Ok(())
}

/// Context handed to us inside a herdr pane or plugin command.
#[allow(dead_code)]
pub struct Env {
    pub pane_id: Option<String>,
    pub workspace_id: Option<String>,
    pub event: Option<String>,
    pub event_json: Option<Value>,
}

impl Env {
    pub fn read() -> Env {
        let ev = std::env::var("HERDR_PLUGIN_EVENT_JSON")
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());

        let from_event = |key: &str| -> Option<String> {
            ev.as_ref().and_then(|v| {
                v.get(key)
                    .or_else(|| v.get("data").and_then(|d| d.get(key)))
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
            })
        };

        Env {
            pane_id: std::env::var("HERDR_PANE_ID").ok().or_else(|| from_event("pane_id")),
            workspace_id: std::env::var("HERDR_WORKSPACE_ID")
                .ok()
                .or_else(|| from_event("workspace_id")),
            event: std::env::var("HERDR_PLUGIN_EVENT").ok(),
            event_json: ev,
        }
    }
}
