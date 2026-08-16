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

/// Take a host-qualified id apart: `w0:p3@mb2` is pane `w0:p3` on machine
/// `mb2`, and a bare `w0:p3` is here.
///
/// **The only place `@` means anything.** herdr has no concept of a host — one
/// server is one machine — so wsp spans machines by qualifying the id and
/// routing on it, and "which machine is this id on" answered in two places is
/// the bug that design exists to avoid. Everything that needs the answer, from
/// picking a socket to deciding whose claims may be reaped, comes here.
///
/// `@` and not `:`, because a herdr pane id already contains a colon: `w0:p3`
/// is one id, not two. Split from the right for the same reason.
///
/// A bare id is this machine and stays bare, which is what lets every existing
/// call site, state file and claim go on working untouched.
pub fn split_host(id: &str) -> (&str, Option<&str>) {
    match id.rsplit_once('@') {
        Some((bare, machine)) if !machine.is_empty() => (bare, Some(machine)),
        _ => (id, None),
    }
}

/// The machine an id names, or `None` for this one.
pub fn host_of(id: &str) -> Option<&str> {
    split_host(id).1
}

/// The params a herdr method is addressed by.
///
/// Nearly all of herdr's 89 methods take one of these, which is what makes
/// spanning machines cheap: the id is the routing key, so no host parameter has
/// to be threaded through the 179 `herdr::` references in this crate. The
/// leftovers — `pane.list`, `workspace.list`, `agent.list`, `events.subscribe`
/// and `workspace.create` — carry no id at all and are t-260816-037's problem,
/// not this list's.
///
/// wsp's own `task_id` is deliberately not here. It is not a herdr id, nothing
/// routes on it, and a `@` in one would mean something else entirely.
const ID_KEYS: [&str; 6] =
    ["pane_id", "workspace_id", "target", "target_pane_id", "source_pane_id", "tab_id"];

/// Which machine a call is addressed to, and the params with the ids made bare
/// again.
///
/// Both halves matter. The far herdr has never heard of `@mb2` — it is one
/// server on one machine and its ids are bare — so the suffix is wsp's own and
/// has to come off on the way out, at the same point it is read. Doing those
/// two things anywhere but together is how an id reaches a server that cannot
/// find it.
///
/// Two ids naming two different machines is refused rather than resolved. It
/// means a call is trying to move a pane from one machine into another's
/// layout, which herdr cannot do and which no correct caller asks for; picking
/// one of them would send it somewhere plausible and wrong.
fn route(mut params: Value) -> std::io::Result<(Option<String>, Value)> {
    let Some(obj) = params.as_object_mut() else { return Ok((None, params)) };
    let mut machine: Option<String> = None;
    for key in ID_KEYS {
        let Some(id) = obj.get(key).and_then(|v| v.as_str()) else { continue };
        let (bare, found) = split_host(id);
        let Some(found) = found else { continue };
        if let Some(first) = &machine {
            if first != found {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("one call cannot span machines: {first} and {found}"),
                ));
            }
        } else {
            machine = Some(found.to_string());
        }
        let bare = Value::String(bare.to_string());
        obj.insert(key.to_string(), bare);
    }
    Ok((machine, params))
}

/// The socket a machine's herdr is reachable on from here: this one's own, or
/// the path the daemon's tunnel forwards the far one to.
///
/// Computed rather than looked up in `machines.json`. The path is a convention
/// with one home — `Store::machine_socket` — so computing it is not a second
/// implementation, and `Store::open` is two paths out of the environment with
/// no file touched. Looking the record up would mean a read of state per call
/// on a socket the panel polls, to learn something we already know, and would
/// make a caller's opinion of where the socket is depend on whether the daemon
/// had got round to writing it down.
///
/// Liveness is not consulted either. A machine that is not up has no socket to
/// connect to and `connect` says so immediately; asking `machines.json` first
/// would be a second opinion on reachability, held to a slower clock than the
/// filesystem's.
fn socket_for(machine: Option<&str>) -> PathBuf {
    match machine {
        Some(name) => crate::store::Store::open().machine_socket(name),
        None => socket_path(),
    }
}

fn connect(timeout: Option<Duration>) -> std::io::Result<UnixStream> {
    connect_to(&socket_path(), timeout)
}

fn connect_to(path: &std::path::Path, timeout: Option<Duration>) -> std::io::Result<UnixStream> {
    let s = UnixStream::connect(path)?;
    s.set_read_timeout(timeout)?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    Ok(s)
}

/// One request, one reply. Three seconds, which is generous for everything
/// herdr answers out of its own state.
pub fn call(method: &str, params: Value) -> std::io::Result<Value> {
    call_for(method, params, Duration::from_secs(3))
}

/// The same, for the calls that wait on something outside herdr.
///
/// `agent.start` types `claude` at a shell and comes back when the agent
/// answers, which is three seconds on this machine and up to thirty by its own
/// documented default. Sent through [`call`] it read as a socket that had gone
/// quiet: the agent started, the reply arrived after we had stopped listening,
/// and the caller reported a failure that had not happened.
pub fn call_for(method: &str, params: Value, timeout: Duration) -> std::io::Result<Value> {
    let (machine, params) = route(params)?;
    send(machine.as_deref(), method, params, timeout)
}

/// A call to a machine you name, for the methods that carry no id to route on.
///
/// `workspace.list`, `pane.list`, `agent.list` and `workspace.create` ask a
/// server about itself, so there is nothing in the params to read a machine
/// off. Everything else should go through [`call`] and let the id decide —
/// naming the machine at a call site that already carries one is how the two
/// answers get to disagree.
///
/// `None` is this machine, so a caller fanning out over "here and mb2" writes
/// one loop rather than a special case.
pub fn call_on(
    machine: Option<&str>,
    method: &str,
    params: Value,
    timeout: Duration,
) -> std::io::Result<Value> {
    send(machine, method, params, timeout)
}

fn send(
    machine: Option<&str>,
    method: &str,
    params: Value,
    timeout: Duration,
) -> std::io::Result<Value> {
    let mut s = connect_to(&socket_for(machine), Some(timeout)).map_err(|e| {
        // Named, because the bare errno is the wrong diagnosis. "No such file
        // or directory" on a path nobody typed reads as a broken install; what
        // it actually means is that the daemon is not holding a tunnel to that
        // machine, which is a different thing to go and look at.
        match &machine {
            Some(name) => std::io::Error::new(e.kind(), format!("{name}: no tunnel ({e})")),
            None => e,
        }
    })?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn params(v: Value) -> (Option<String>, Value) {
        route(v).expect("routable")
    }

    /// The suffix is wsp's own. It picks the socket and then it comes off,
    /// because the server on the other end is one machine that has never heard
    /// of `@mb2` and will not find `w0:p3@mb2` in its own pane table.
    #[test]
    fn the_machine_picks_the_socket_and_the_id_goes_out_bare() {
        let (machine, p) = params(json!({ "pane_id": "w0:p3@mb2", "text": "hello@example" }));
        assert_eq!(machine.as_deref(), Some("mb2"));
        assert_eq!(p["pane_id"], "w0:p3", "the far herdr is sent its own id");
        assert_eq!(p["text"], "hello@example", "and nothing else is touched");
    }

    /// The whole point of routing on the id: a local call is byte-for-byte the
    /// call it was before this existed, so no existing call site, state file or
    /// claim needs migrating.
    #[test]
    fn a_bare_id_is_this_machine_and_is_passed_through_unchanged() {
        let before = json!({ "workspace_id": "w0", "label": "wsp" });
        let (machine, after) = params(before.clone());
        assert_eq!(machine, None);
        assert_eq!(after, before);

        // And params that are not an object at all route nowhere rather than
        // panicking on the way past.
        let (machine, after) = params(json!([]));
        assert_eq!(machine, None);
        assert_eq!(after, json!([]));
    }

    /// Every key a herdr method is addressed by, and the two that agree.
    #[test]
    fn all_the_routing_keys_are_read_and_must_agree() {
        let (machine, p) = params(json!({ "source_pane_id": "w0:p1@mb2", "target_pane_id": "w0:p3@mb2" }));
        assert_eq!(machine.as_deref(), Some("mb2"));
        assert_eq!(p["source_pane_id"], "w0:p1");
        assert_eq!(p["target_pane_id"], "w0:p3");

        // Splitting a pane on one machine into a pane on another is not a thing
        // herdr can do, so it is refused rather than half-done.
        let err = route(json!({ "source_pane_id": "w0:p1@mb2", "target_pane_id": "w0:p3@gpu" }))
            .expect_err("a call across two machines");
        assert!(err.to_string().contains("span machines"), "{err}");

        // wsp's own ids are not herdr's and nothing routes on them.
        let (machine, p) = params(json!({ "task_id": "t-260816-036", "pane_id": "w0:p3" }));
        assert_eq!(machine, None);
        assert_eq!(p["task_id"], "t-260816-036");
    }

    /// The end of the wire, with a real socket at the end of it.
    ///
    /// A stand-in herdr is bound at exactly the path the daemon's tunnel would
    /// forward `mb2` to, and a call carrying `@mb2` has to arrive there rather
    /// than at this machine's socket — carrying the bare id. That is the whole
    /// mechanism: no proxy, no second protocol, no re-implementation of the 89
    /// methods, just a different socket and an id the far end recognises.
    ///
    /// One test, because it sets `WSP_STATE` for the process.
    #[test]
    fn a_qualified_id_arrives_at_that_machines_socket_and_nowhere_else() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let state = std::env::temp_dir().join(format!("wsp-route-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(state.join("sock")).unwrap();
        std::env::set_var("WSP_STATE", &state);

        let sock = state.join("sock").join("mb2.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let req: Value = serde_json::from_str(line.trim()).unwrap();
            // Hand the request back as the result, so the caller can see
            // exactly what reached the far end.
            let reply = json!({ "id": req["id"], "result": { "saw": req } });
            let mut stream = stream;
            stream.write_all(format!("{reply}\n").as_bytes()).unwrap();
            stream.flush().unwrap();
        });

        let got = call("agent.get", json!({ "target": "w0:p3@mb2" })).expect("routed to mb2");
        server.join().unwrap();

        assert_eq!(got["saw"]["method"], "agent.get");
        assert_eq!(got["saw"]["params"]["target"], "w0:p3", "the suffix did not survive the wire");

        // And a machine with no tunnel says so as a machine, not as an errno on
        // a path nobody typed.
        let err = call("agent.get", json!({ "target": "w0:p3@gpu" })).expect_err("no tunnel to gpu");
        assert!(err.to_string().contains("gpu: no tunnel"), "{err}");

        std::env::remove_var("WSP_STATE");
        let _ = std::fs::remove_dir_all(&state);
    }
}
