//! A wsp with no login is today's wsp (wsp-463; multiplayer v1, decision d6: a
//! local wsp works fully in isolation, and the hub is optional).
//!
//! The test that proves it before any hub code lands, so every later row keeps
//! it green. A `wsp sandbox --fake` store under an empty `HOME`, with no
//! `~/.config/wsp/hubs.json`: the daemon runs for two ticks while the verbs a
//! person uses alone run beside it (`project add`, `add`, `ls`, `show`, `wip
//! --json`, `worklist new`, `worklist add`, `worklist show`, `tell`). Then:
//!
//! 1. no thread of the daemon (read from /proc on Linux) and no log line it or
//!    the store wrote (`cycle.log`, `api.log`, what the daemon printed) names a
//!    hub;
//! 2. no socket of the daemon leaves loopback: none connected to an address
//!    that is not 127.0.0.0/8 or ::1, none bound to one (/proc/<pid>/net on
//!    Linux, `lsof` on macOS);
//! 3. no file appears under `$WSP_STATE`, the store or the home but the ones
//!    [`ALLOWED`] names, which is what today's build writes, and the home stays
//!    empty (`~/.config/wsp` is never made);
//! 4. `wip --json` prints the bytes in `tests/golden/standalone-wip.json`, the
//!    sandbox's own directory written `<dir>` and timestamps `<ts>`; the
//!    machine's readings are pinned as in `tests/view_golden.rs`.
//!
//! `WSP_STANDALONE_BLESS=1 cargo test --test standalone` writes the golden from
//! what `wip --json` prints now; only for a change that means to move it.
//!
//! The assertion helpers are held to a plant each, at the end: a planted
//! outbound socket, a planted file, a planted hub line and thread, a planted
//! change in `wip --json`. A helper that passes its plant is not looking.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const WSP: &str = env!("CARGO_BIN_EXE_wsp");

/// The machine every reading sees (as `tests/view_seed`): 8 cores at a quiet
/// load, memory to spare, no thermal limit, nobody's idle time to read.
const MACHINE: &str = r#"{"cores": 8, "load": [1.5, 1.25, 1.0], "mem_free": 60, "swap": [512, 4096], "thermal": 100}"#;
const NOW: &str = "1795000000";
const HOST: &str = "golden-host";
const GOLDEN: &str = "tests/golden/standalone-wip.json";

/// What today's build leaves under the sandbox's directory, relative to the
/// outer `$WSP_STATE` (`state/`). A segment is exact, `*` is any one segment,
/// `name*` any segment that begins `name`, and a last segment `**` anything
/// below. `<n>` is the sandbox's name.
const ALLOWED: &[&str] = &[
    // The outer store's call line for `sandbox` itself.
    "calls.jsonl",
    "sandbox/<n>/stage.json",
    // The fake backend's socket and the binary the sandbox runs.
    "sandbox/<n>/backend.sock",
    "sandbox/<n>/bin/wsp",
    // `$WSP_STATE` of the sandbox: the daemon's and the verbs' own record.
    "sandbox/<n>/state/api.json",
    "sandbox/<n>/state/api.log",
    "sandbox/<n>/state/api.token",
    "sandbox/<n>/state/burners",
    "sandbox/<n>/state/calls.jsonl",
    "sandbox/<n>/state/polls.jsonl",
    "sandbox/<n>/state/cycle.log",
    "sandbox/<n>/state/daemon.json",
    "sandbox/<n>/state/daemon.tick",
    "sandbox/<n>/state/events.jsonl",
    "sandbox/<n>/state/index/**",
    "sandbox/<n>/state/mem.json",
    // A starting daemon's once-per-store follow-up migration (`wsp-345`).
    "sandbox/<n>/state/migrated-follow-ups",
    "sandbox/<n>/state/state.db*",
    "sandbox/<n>/state/watches.json",
    // The store: a git repository of projects, tasks and worklists.
    "sandbox/<n>/wsp/.git/**",
    "sandbox/<n>/wsp/README.md",
    "sandbox/<n>/wsp/projects/*",
    "sandbox/<n>/wsp/tasks/*",
    "sandbox/<n>/wsp/worklists/*",
];

// ───────────────────────────── the assertion helpers ─────────────────────────────

/// One socket of a process: where it is bound and, if it is connected, to what.
#[derive(Debug, Clone, PartialEq)]
struct Sock {
    local: IpAddr,
    remote: Option<IpAddr>,
}

fn is_local(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    ip.is_loopback()
}

fn is_unspecified(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V6(v6) => v6.is_unspecified() || v6.to_ipv4_mapped().is_some_and(|v| v.is_unspecified()),
        v4 => v4.is_unspecified(),
    }
}

/// What in `socks` leaves loopback: a socket connected to an address that is
/// not loopback, or bound to one (a listener on `0.0.0.0` is reachable from
/// outside).
fn off_loopback(socks: &[Sock]) -> Vec<String> {
    let mut out = Vec::new();
    for s in socks {
        if let Some(r) = s.remote {
            if !is_unspecified(r) && !is_local(r) {
                out.push(format!("connected {} -> {r}", s.local));
            }
        }
        if !is_local(s.local) {
            out.push(format!("bound to {}", s.local));
        }
    }
    out
}

/// A hex address of /proc/net/{tcp,udp}{,6}: 8 hex digits for IPv4, 32 for
/// IPv6, each 32-bit word in the machine's byte order (little-endian here).
fn proc_addr(hex: &str) -> Option<(IpAddr, u16)> {
    let (addr, port) = hex.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let word = |w: &str| u32::from_str_radix(w, 16).ok().map(|n| n.to_le_bytes());
    let ip = match addr.len() {
        8 => {
            let b = word(addr)?;
            IpAddr::V4(Ipv4Addr::new(b[0], b[1], b[2], b[3]))
        }
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word(&addr[i * 8..i * 8 + 8])?);
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some((ip, port))
}

/// The sockets of /proc/net/tcp (or udp, tcp6, udp6) whose inode is in `inodes`.
fn parse_proc_net(text: &str, inodes: &BTreeSet<u64>) -> Vec<Sock> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        // sl local rem st tx:rx tr:when retrnsmt uid timeout inode
        if f.len() < 10 || !f[9].parse::<u64>().is_ok_and(|i| inodes.contains(&i)) {
            continue;
        }
        let (Some((local, _)), Some((remote, rport))) = (proc_addr(f[1]), proc_addr(f[2])) else { continue };
        // A listener or an unconnected datagram socket has no peer: 0.0.0.0:0.
        let remote = (!(is_unspecified(remote) && rport == 0)).then_some(remote);
        out.push(Sock { local, remote });
    }
    out
}

/// The `n` lines of `lsof -nP -F n` for a process: `127.0.0.1:47124`,
/// `127.0.0.1:50000->127.0.0.1:47124`, `*:5353`, `[::1]:80`.
fn parse_lsof(text: &str) -> Vec<Sock> {
    fn host(s: &str) -> Option<IpAddr> {
        let h = s.rsplit_once(':')?.0.trim_start_matches('[').trim_end_matches(']');
        match h {
            "*" => Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            h => h.parse().ok(),
        }
    }
    let mut out = Vec::new();
    for name in text.lines().filter_map(|l| l.strip_prefix('n')) {
        let (local, remote) = match name.split_once("->") {
            Some((l, r)) => (l, Some(r)),
            None => (name, None),
        };
        // Unix sockets and files are `n/path`: no address.
        let Some(local) = host(local) else { continue };
        out.push(Sock { local, remote: remote.and_then(host) });
    }
    out
}

/// The sockets of process `pid`.
fn sockets_of(pid: u32) -> Vec<Sock> {
    if cfg!(target_os = "linux") {
        let mut inodes = BTreeSet::new();
        for fd in std::fs::read_dir(format!("/proc/{pid}/fd")).into_iter().flatten().flatten() {
            let link = std::fs::read_link(fd.path()).unwrap_or_default().display().to_string();
            if let Some(n) = link.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')) {
                inodes.extend(n.parse::<u64>());
            }
        }
        ["tcp", "tcp6", "udp", "udp6"]
            .iter()
            .flat_map(|t| parse_proc_net(&std::fs::read_to_string(format!("/proc/{pid}/net/{t}")).unwrap_or_default(), &inodes))
            .collect()
    } else {
        let out = Command::new("lsof").args(["-nP", "-a", "-p", &pid.to_string(), "-i", "-F", "n"]).output().expect("lsof runs");
        parse_lsof(&String::from_utf8_lossy(&out.stdout))
    }
}

/// The names of `pid`'s threads (Linux's /proc; macOS gives no thread names to
/// a plain process, so the log lines and the files carry the check there).
fn thread_names(pid: u32) -> Vec<String> {
    let mut names = Vec::new();
    for t in std::fs::read_dir(format!("/proc/{pid}/task")).into_iter().flatten().flatten() {
        if let Ok(n) = std::fs::read_to_string(t.path().join("comm")) {
            names.push(n.trim().to_string());
        }
    }
    names
}

/// Whether `text` names a hub: the word "hub" (or "hubs"), any case, as a word
/// or the start of a CamelCase one (`HubClient`), not inside "github" or "hubble".
fn names_hub(text: &str) -> bool {
    let low = text.to_ascii_lowercase();
    let (b, lb) = (text.as_bytes(), low.as_bytes());
    let mut from = 0;
    while let Some(i) = low[from..].find("hub") {
        let at = from + i;
        from = at + 1;
        let before_ok = at == 0 || !lb[at - 1].is_ascii_alphabetic();
        let mut end = at + 3;
        if lb.get(end) == Some(&b's') {
            end += 1;
        }
        let after_ok = b.get(end).is_none_or(|c| !c.is_ascii_lowercase());
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// The lines of `text` that name a hub, each with where they were read.
fn hub_lines(source: &str, text: &str) -> Vec<String> {
    text.lines().filter(|l| names_hub(l)).map(|l| format!("{source}: {l}")).collect()
}

/// Whether the path `rel` (segments joined by `/`) is what `pattern` names.
fn matches_pattern(rel: &str, pattern: &str) -> bool {
    let (r, p): (Vec<&str>, Vec<&str>) = (rel.split('/').collect(), pattern.split('/').collect());
    if p.last() == Some(&"**") {
        return r.len() >= p.len() && p[..p.len() - 1].iter().zip(&r).all(|(p, r)| seg(r, p));
    }
    r.len() == p.len() && p.iter().zip(&r).all(|(p, r)| seg(r, p))
}

fn seg(name: &str, pattern: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some("") => true,
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

/// Every entry under `root` that is not a directory (a socket and a symlink
/// too), relative to it with `/`.
fn entries(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = format!("{rel}{}", e.file_name().to_string_lossy());
            match e.file_type() {
                Ok(t) if t.is_dir() => walk(&e.path(), &format!("{name}/"), out),
                _ => out.push(name),
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out.sort();
    out
}

/// What is under `root` that no pattern of `allowed` names.
fn unlisted(root: &Path, allowed: &[String]) -> Vec<String> {
    entries(root).into_iter().filter(|e| !allowed.iter().any(|p| matches_pattern(e, p))).collect()
}

/// `<ts>` for every `2026-11-18T11:06:40Z` in `text`, as the other goldens'
/// stamps are pinned (here there are none to move, and none may creep in).
fn mask_timestamps(text: &str) -> String {
    let b = text.as_bytes();
    let shape = b"dddd-dd-ddTdd:dd:ddZ";
    let mut out = String::new();
    let mut i = 0;
    while i < text.len() {
        let hit = i + shape.len() <= b.len()
            && shape.iter().enumerate().all(|(k, s)| if *s == b'd' { b[i + k].is_ascii_digit() } else { b[i + k] == *s });
        if hit {
            out.push_str("<ts>");
            i += shape.len();
        } else {
            let ch = text[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// The first line where `got` is not `want`, or none.
fn golden_diff(got: &str, want: &str) -> Option<String> {
    if got == want {
        return None;
    }
    let (g, w): (Vec<&str>, Vec<&str>) = (got.lines().collect(), want.lines().collect());
    let at = (0..g.len().max(w.len())).find(|&i| g.get(i) != w.get(i)).unwrap_or(0);
    Some(format!("line {}: printed {:?}, recorded {:?}", at + 1, g.get(at).unwrap_or(&"<none>"), w.get(at).unwrap_or(&"<none>")))
}

// ─────────────────────────────────── the sandbox ───────────────────────────────────

/// `wsp` with none of this seat's own `WSP_*`, `COMPOUND_*` or `CLAUDE*`, an
/// empty `home` for `HOME`, and every live reading pinned.
fn bare(env: &BTreeMap<String, String>, home: &Path) -> Command {
    let mut c = Command::new(WSP);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("WSP_") || k.starts_with("COMPOUND_") || k.starts_with("CLAUDE")) {
        c.env_remove(k);
    }
    c.envs(env)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("WSP_TEST_MACHINE", MACHINE)
        .env("HOSTNAME", HOST)
        .env("TZ", "UTC")
        .env("WSP_HOST", "manager");
    c
}

struct Sandbox {
    name: String,
    outer: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    dir: PathBuf,
    home: PathBuf,
    daemon: Option<Child>,
}

impl Sandbox {
    fn up() -> Sandbox {
        // Short: the fake backend's unix socket lives under it, and macOS caps that path at 104 bytes.
        let dir = PathBuf::from("/tmp").join(format!("sa-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("store").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer, &home).arg("init").output().unwrap();
        let name = "alone".to_string();
        let out = bare(&outer, &home).args(["sandbox", "--fake", "--json", "--name", &name]).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let env = v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        let sb = Sandbox { name, outer, env, dir, home, daemon: None };
        sb.ok(&["init"]);
        sb
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = bare(&self.env, &self.home);
        c.args(args);
        c
    }

    /// A verb that has to succeed: its stdout.
    fn ok(&self, args: &[&str]) -> String {
        let o = self.cmd(args).output().unwrap();
        assert!(o.status.success(), "wsp {}: {}{}", args.join(" "), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn state(&self) -> PathBuf {
        PathBuf::from(&self.env["WSP_STATE"])
    }

    fn start_daemon(&mut self) {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let out = std::fs::File::create(self.dir.join("daemon.out")).unwrap();
        let err = out.try_clone().unwrap();
        let mut c = self.cmd(&["daemon"]);
        // The pins above are for the commands; the daemon reads the real clock.
        c.env("WSP_API_PORT", port.to_string()).env_remove("WSP_TEST_NOW").stdout(out).stderr(err);
        self.daemon = Some(c.spawn().unwrap());
    }

    fn daemon_pid(&self) -> u32 {
        self.daemon.as_ref().expect("the daemon is up").id()
    }

    fn tick(&self) -> Option<String> {
        std::fs::read_to_string(self.state().join("daemon.tick")).ok().filter(|t| !t.trim().is_empty())
    }

    /// Waits for the daemon's tick stamp to be there (the first tick) or to be
    /// other than `after` (the next one).
    fn wait_tick(&self, after: Option<&str>, within: Duration) -> String {
        let end = Instant::now() + within;
        loop {
            if let Some(t) = self.tick() {
                if after.is_none_or(|a| a != t) {
                    return t;
                }
            }
            assert!(Instant::now() < end, "the daemon did not tick within {within:?}: {}", self.daemon_out());
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn daemon_out(&self) -> String {
        std::fs::read_to_string(self.dir.join("daemon.out")).unwrap_or_default()
    }

    fn stop_daemon(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
    }

    /// The patterns of [`ALLOWED`] for this sandbox.
    fn allowed(&self) -> Vec<String> {
        ALLOWED.iter().map(|p| p.replace("<n>", &format!("wsp-{}", self.name))).collect()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        self.stop_daemon();
        let _ = bare(&self.outer, &self.home).args(["sandbox", "rm", &self.name]).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What the test has seen of the daemon, over its run.
#[derive(Default)]
struct Seen {
    socks: Vec<Sock>,
    threads: BTreeSet<String>,
}

impl Seen {
    fn sample(&mut self, pid: u32) {
        for s in sockets_of(pid) {
            if !self.socks.contains(&s) {
                self.socks.push(s);
            }
        }
        self.threads.extend(thread_names(pid));
    }
}

// ───────────────────────────────────── the test ─────────────────────────────────────

#[test]
fn a_wsp_with_no_login_adds_no_thread_socket_or_file() {
    let mut sb = Sandbox::up();
    assert!(!sb.home.join(".config/wsp/hubs.json").exists(), "the home starts with a hubs.json");

    sb.start_daemon();
    let pid = sb.daemon_pid();
    let mut seen = Seen::default();
    let first = sb.wait_tick(None, Duration::from_secs(60));
    seen.sample(pid);

    // The verbs of a person on their own, while the daemon runs.
    sb.ok(&["project", "add", "alpha", "--name", "Alpha"]);
    sb.ok(&["add", "Open row", "-p", "alpha"]);
    sb.ok(&["add", "Second row", "-p", "alpha"]);
    sb.ok(&["ls"]);
    sb.ok(&["show", "alpha-001"]);
    sb.ok(&["wip", "--json"]);
    sb.ok(&["worklist", "new", "l1", "List one"]);
    sb.ok(&["worklist", "add", "l1", "alpha-001"]);
    sb.ok(&["worklist", "add", "l1", "alpha-002"]);
    sb.ok(&["worklist", "show", "l1"]);
    // Nothing holds the row, so `tell` says so and exits 1: the verb ran, and
    // what it did not do is dial anything (the sockets below).
    let told = sb.cmd(&["tell", "alpha-001", "hello"]).output().unwrap();
    assert!(told.status.code().is_some_and(|c| c <= 1), "tell: {}", String::from_utf8_lossy(&told.stderr));
    seen.sample(pid);

    // The second tick.
    sb.wait_tick(Some(&first), Duration::from_secs(60));
    seen.sample(pid);

    // `wip --json`, from the files as the other goldens are read.
    let text = {
        let o = sb.cmd(&["wip", "--json"]).env("WSP_READ", "files").env("WSP_TEST_NOW", NOW).output().unwrap();
        assert!(o.status.success(), "wip --json: {}", String::from_utf8_lossy(&o.stderr));
        mask_timestamps(&String::from_utf8(o.stdout).unwrap().replace(&sb.dir.display().to_string(), "<dir>"))
    };
    seen.sample(pid);
    let logs: Vec<(String, String)> = ["cycle.log", "api.log"]
        .iter()
        .map(|f| (f.to_string(), std::fs::read_to_string(sb.state().join(f)).unwrap_or_default()))
        .chain([("daemon output".to_string(), sb.daemon_out())])
        .collect();
    sb.stop_daemon();

    // 1. no thread or log line names the hub.
    if cfg!(target_os = "linux") {
        assert!(seen.threads.contains("mirror"), "the daemon's threads were not read: {:?}", seen.threads);
    }
    let named: Vec<&String> = seen.threads.iter().filter(|t| names_hub(t)).collect();
    assert!(named.is_empty(), "daemon threads name a hub: {named:?}");
    let lines: Vec<String> = logs.iter().flat_map(|(f, t)| hub_lines(f, t)).collect();
    assert!(lines.is_empty(), "the logs name a hub:\n{}", lines.join("\n"));

    // 2. no connection leaves loopback.
    assert!(!seen.socks.is_empty(), "the daemon's sockets were not read (it listens on loopback for its API)");
    let off = off_loopback(&seen.socks);
    assert!(off.is_empty(), "the daemon's sockets leave loopback: {off:?}");

    // 3. no file but today's.
    let extra = unlisted(&sb.dir.join("state"), &sb.allowed());
    assert!(extra.is_empty(), "files under $WSP_STATE that today's build does not write: {extra:?}");
    let home = entries(&sb.home);
    assert!(home.is_empty(), "the home is not empty (~/.config/wsp?): {home:?}");

    // 4. wip --json is the recording.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os("WSP_STANDALONE_BLESS").is_some_and(|v| !v.is_empty() && v != "0") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{GOLDEN}: {e} (WSP_STANDALONE_BLESS=1 writes it)"));
    if let Some(d) = golden_diff(&text, &want) {
        panic!("wip --json moved from {GOLDEN}: {d}\n{text}");
    }
}

// ──────────────────────────── the self-check: each helper bites ────────────────────────────

#[test]
fn a_planted_outbound_socket_is_seen_off_loopback() {
    // Today's daemon: a listener on loopback, and a peer on loopback.
    let quiet = [
        Sock { local: "127.0.0.1".parse().unwrap(), remote: None },
        Sock { local: "127.0.0.1".parse().unwrap(), remote: Some("127.0.0.1".parse().unwrap()) },
        Sock { local: "::1".parse().unwrap(), remote: Some("::ffff:127.0.0.1".parse().unwrap()) },
    ];
    assert!(off_loopback(&quiet).is_empty());

    // A /proc/net/tcp with one established connection to 93.184.216.34:443
    // (22D8B85D, little-endian) and one to loopback, both owned by inode 7.
    let proc = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:B814 0100007F:C350 01 00000000:00000000 00:00000000 00000000  1000 0 7 1 0 100 0 0 10 0\n\
   1: 0A00000A:B815 22D8B85D:01BB 01 00000000:00000000 00:00000000 00000000  1000 0 7 1 0 100 0 0 10 0\n\
   2: 0A00000A:B816 22D8B85D:01BB 01 00000000:00000000 00:00000000 00000000  1000 0 8 1 0 100 0 0 10 0\n";
    let mine: BTreeSet<u64> = [7].into();
    let socks = parse_proc_net(proc, &mine);
    assert_eq!(socks.len(), 2, "{socks:?}");
    let off = off_loopback(&socks);
    assert_eq!(off.len(), 2, "{off:?}"); // its peer, and its own bind to 10.0.0.10
    assert!(off.iter().any(|o| o.contains("-> 93.184.216.34")), "{off:?}");
    // IPv6, word by word: ::1 is loopback, 2001:db8::1 is not.
    assert_eq!(proc_addr("00000000000000000000000001000000:B814").unwrap().0, "::1".parse::<IpAddr>().unwrap());
    assert_eq!(proc_addr("B80D0120000000000000000001000000:0050").unwrap().0, "2001:db8::1".parse::<IpAddr>().unwrap());

    // lsof's side of it (macOS): the same two, and a wildcard listener.
    let lsof = "p1\nf5\nPTCP\nn127.0.0.1:47124\nf6\nPTCP\nn127.0.0.1:50000->127.0.0.1:47124\nf7\nn/tmp/x.sock\n";
    assert!(off_loopback(&parse_lsof(lsof)).is_empty());
    let lsof = "p1\nf5\nPTCP\nn10.0.0.10:50001->93.184.216.34:443\nf6\nPUDP\nn*:5353\nf7\nPTCP\nn[2001:db8::1]:80\n";
    let off = off_loopback(&parse_lsof(lsof));
    assert_eq!(off.len(), 4, "{off:?}"); // bind + peer, wildcard bind, v6 bind
}

#[test]
fn a_planted_socket_in_this_process_is_read_off_the_machine() {
    // The real path, `sockets_of`, on this very process: a listener on every
    // interface is what a hub's acceptor would be, and must be seen.
    let me = std::process::id();
    let loopback = TcpListener::bind("127.0.0.1:0").unwrap();
    let before = off_loopback(&sockets_of(me));
    assert!(sockets_of(me).iter().any(|s| s.local.is_loopback()), "the loopback listener is not read: {:?}", sockets_of(me));
    assert!(!before.iter().any(|o| o.contains("127.0.0.1")), "{before:?}");

    let wide = TcpListener::bind("0.0.0.0:0").unwrap();
    let after = off_loopback(&sockets_of(me));
    assert!(after.len() > before.len(), "a listener on 0.0.0.0 was not seen: {after:?}");

    // And one that is connected out, where this machine will route a datagram
    // socket to a documentation address (no packet is sent by `connect`).
    if let Ok(udp) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if udp.connect("192.0.2.1:9").is_ok() {
            let out = off_loopback(&sockets_of(me));
            assert!(out.iter().any(|o| o.contains("192.0.2.1")), "a connected datagram socket was not seen: {out:?}");
        }
    }
    drop((loopback, wide));
}

#[test]
fn a_planted_file_is_outside_the_allow_list() {
    let dir = PathBuf::from("/tmp").join(format!("sa-plant-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let name = "alone";
    let allowed: Vec<String> = ALLOWED.iter().map(|p| p.replace("<n>", &format!("wsp-{name}"))).collect();
    let put = |rel: &str| {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
    };
    for rel in [
        "calls.jsonl",
        "sandbox/wsp-alone/state/daemon.tick",
        "sandbox/wsp-alone/state/state.db-wal",
        "sandbox/wsp-alone/state/index/tasks.json",
        "sandbox/wsp-alone/wsp/tasks/alpha-001.md",
        "sandbox/wsp-alone/wsp/.git/objects/ab/cdef",
    ] {
        put(rel);
    }
    assert_eq!(unlisted(&dir, &allowed), Vec::<String>::new(), "today's files are all listed");

    for plant in [
        "hubs.json",
        "sandbox/wsp-alone/state/hub.json",
        "sandbox/wsp-alone/state/hubs/a.json",
        "sandbox/wsp-alone/state/state.sqlite",
        "sandbox/wsp-alone/wsp/hub/queue.md",
        "sandbox/wsp-other/state/daemon.tick",
        ".config/wsp/hubs.json",
    ] {
        put(plant);
        assert_eq!(unlisted(&dir, &allowed), vec![plant.to_string()], "a planted {plant} was not seen");
        std::fs::remove_file(dir.join(plant)).unwrap();
    }
    // A socket is a file for this purpose too.
    let sock = dir.join("sandbox/wsp-alone/state/hub.sock");
    let _l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    assert_eq!(unlisted(&dir, &allowed), vec!["sandbox/wsp-alone/state/hub.sock".to_string()]);
    drop(_l);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_planted_hub_line_or_thread_is_seen() {
    for line in ["connecting to hub j2:7000", "Hub login: ok", "hubs.json missing", "HubClient started", "joined (hub)", "wsp-hub-link"] {
        assert!(names_hub(line), "missed {line:?}");
    }
    for line in ["pushed to github", "GitHub", "hubble", "shub", "daemon: reaped defunct child 12", ""] {
        assert!(!names_hub(line), "named a hub in {line:?}");
    }
    assert_eq!(hub_lines("cycle.log", "a\nreaching the hub\nb"), vec!["cycle.log: reaching the hub".to_string()]);
    assert!(hub_lines("cycle.log", "a\nb\n").is_empty());
    // A thread called so is one `thread_names` would hand to the same check.
    if cfg!(target_os = "linux") {
        let t = std::thread::Builder::new().name("hub-link".into()).spawn(|| std::thread::sleep(Duration::from_millis(500))).unwrap();
        let names = thread_names(std::process::id());
        assert!(names.iter().any(|n| names_hub(n)), "a thread named hub-link was not read: {names:?}");
        t.join().unwrap();
    }
}

#[test]
fn a_planted_change_in_wip_is_seen_against_the_golden() {
    let want = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN)).unwrap_or_else(|_| "{\n  \"inbox\": 0\n}\n".into());
    assert_eq!(golden_diff(&want, &want), None);
    let moved = want.replacen("\"inbox\": 0", "\"inbox\": 1", 1);
    assert!(moved != want, "the golden has no inbox key to move");
    assert!(golden_diff(&moved, &want).is_some(), "a changed key was not seen");
    assert!(golden_diff(&format!("{want} "), &want).is_some(), "a trailing byte was not seen");
    // The masks: a stamp is `<ts>`, and nothing else is touched.
    assert_eq!(mask_timestamps("at 2026-11-18T11:06:40Z and 2026-11-18"), "at <ts> and 2026-11-18");
    assert_eq!(mask_timestamps("naïve — 1795000000"), "naïve — 1795000000");
}
