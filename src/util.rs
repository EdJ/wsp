//! Small helpers: time without chrono, path expansion, terminal colour.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The instance we are talking to, as environment for anything we spawn.
///
/// A pane, tab or workspace herdr creates starts a fresh shell, which inherits
/// nothing from us — so a panel pointed at a non-default store would open
/// editors pointed at the default one. They would then fail to find the task
/// and take the tab down with them, which looks exactly like the key not
/// working. An agent spawned onto a task has the same problem and a longer
/// fuse: it would sit in a tree reading the wrong backlog.
///
/// `WSP_BIN` joins them on the same argument: it is what `herdr-plugin/run.sh`
/// checks before `~/.local/bin/wsp`, so a pane opened by a wsp under test
/// should run the binary under test rather than whatever is installed. Nothing
/// but `wsp sandbox` sets it.
///
/// The socket is the exception, and carrying it unconditionally was a bug.
/// `HERDR_SOCKET_PATH` is set in *every* live herdr pane — herdr puts it there
/// itself, which is also why propagating it is pointless for an ordinary spawn:
/// whichever herdr creates the workspace tells its panes where it is. What it
/// is not is portable. This map goes into a seat's environment on whichever
/// machine that seat is being made on, so an ordinary `wsp spawn --on <machine>`
/// would bake *this* machine's socket path into a workspace on the executor — working by accident for as long as the two home layouts
/// match, failing silently the moment a username differs, and contradicting the
/// qualify-and-route design the whole executor stack rests on.
///
/// So it goes only where it means something: a sandbox, where the socket is the
/// entire point and a workspace whose panes talked to the live server would be
/// half of each instance. `WSP_BIN` is the marker, being the one variable
/// nothing but a sandbox sets.
pub fn store_env() -> serde_json::Map<String, serde_json::Value> {
    store_env_from(|k| std::env::var(k).ok())
}

/// The same, against any environment. Taking the lookup as an argument is what
/// makes the rule above testable: the claim that was wrong is a claim about
/// which variables are set, and a test that had to set them process-wide to
/// check it would be a test that cannot be trusted to run beside another.
fn store_env_from(
    get: impl Fn(&str) -> Option<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut env = serde_json::Map::new();
    let mut carry = |key: &str| {
        if let Some(v) = get(key) {
            if !v.is_empty() {
                env.insert(key.to_string(), serde_json::Value::String(v));
                return true;
            }
        }
        false
    };
    for key in ["WSP_HOME", "WSP_STATE", "WSP_NO_COMMIT"] {
        carry(key);
    }
    if carry("WSP_BIN") {
        carry("HERDR_SOCKET_PATH");
    }
    env
}

/// The lock every test that points wsp at a herdr takes.
///
/// `HERDR_SOCKET_PATH`, `WSP_HOME` and `WSP_STATE` are process-wide and cargo
/// runs tests in threads, so two tests halfway through pointing the process at
/// two different sockets is a flake that appears once a fortnight and is read as
/// a real failure. `herdr.rs` and `fake.rs` each kept a mutex of their own for
/// this, which protects a module from itself and not from the module next door —
/// and there are three of them now. One process-wide resource, one lock.
///
/// Taken bare only for the variables nothing else knows how to set up — a seat
/// id, a session marker, a credential the shed is being asked about. A test that
/// can reach a store or a herdr wants [`isolated`], which takes this lock and
/// points all three of the pointing-variables at something of its own.
#[cfg(test)]
pub static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`ENV`], surviving a test that panicked while holding it.
#[cfg(test)]
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV.lock().unwrap_or_else(|e| e.into_inner())
}

/// A store nobody else is using, a backend that does not exist, and this
/// process pointed at both for as long as the guard lives. What a test takes
/// instead of [`env_lock`] when anything under it can reach a
/// [`crate::store::Store`] or a herdr.
///
/// **Written by two tests that failed because a machine was added to the real
/// store**, and neither of them was about machines (robustness-068).
/// `Store::open()` falls back to `~/wsp` when `WSP_HOME` is unset, so a test
/// that reaches a store reaches the developer's own — and `herdr::panes()` fans
/// out over `store.machines()`, so a test that stood up a fake backend and
/// asserted the one pane in it began asserting that pane *plus whatever was
/// running on eds-macbook-i5*, over a tunnel, from a test named after a fake.
/// Nothing in the code had changed. `wsp install` is gated on
/// `wsp verify --release`, which is how a laptop being switched on came to block
/// an install.
///
/// The fix belongs here rather than in `fanout()`, which is correct as written:
/// fanning out is what it is for, and a production path taught to notice tests
/// is a production path that behaves differently in the one place nobody is
/// watching. `Store::open` and [`crate::herdr::socket_path`] each carry a
/// `#[cfg(test)]` assertion that this guard has been taken — a line that
/// compiles out of the binary, so no shipped path can tell, and that turns
/// "somebody remembered" into "it will not run".
///
/// **All three variables, or none of it works.** `WSP_HOME` alone leaves state
/// at `~/.local/state/wsp`, which is where the machine liveness the fan-out
/// filters on is read from, so half an isolation still reads the real machine
/// list (robustness-011 is open on that asymmetry). And the socket is not a
/// third nicety: `HERDR_SOCKET_PATH` unset means the **live herdr**, which is a
/// server that gets written to. The three `cmd_govern` tests were renaming this
/// machine's `w1` on every `cargo test` — the failure that file's module doc
/// records as measured the hard way, still running. So the default here is a
/// path inside the temp directory that nothing ever binds: a test that wants a
/// backend stands one up and points the variable at it, and a test that does not
/// gets a refused connection instead of somebody's window.
///
/// It holds [`ENV`] because these are process-wide, so an isolated test is no
/// isolation at all from the test next door halfway through setting its own —
/// which also means **one guard per test**: taking a second on the same thread
/// waits for the first to be dropped, forever. And it puts everything back on
/// drop rather than at the end of the test body, because a test that fails while
/// holding them leaves the process pointed at a store and a socket that no
/// longer exist, and the test that runs next is the one that looks broken.
#[cfg(test)]
pub struct Isolated {
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: PathBuf,
}

#[cfg(test)]
pub fn isolated(name: &str) -> Isolated {
    let lock = env_lock();
    let dir = std::env::temp_dir().join(format!("wsp-t-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("wsp")).unwrap();
    // `sock` because a store's socket paths are read straight off state, so a
    // test that binds one is otherwise creating this directory itself.
    std::fs::create_dir_all(dir.join("state/sock")).unwrap();
    std::env::set_var("WSP_HOME", dir.join("wsp"));
    std::env::set_var("WSP_STATE", dir.join("state"));
    std::env::set_var("HERDR_SOCKET_PATH", dir.join("no-backend.sock"));
    Isolated { _lock: lock, dir }
}

#[cfg(test)]
impl Isolated {
    /// The store — `WSP_HOME`. Empty, and a test that wants a task in it writes
    /// one.
    pub fn home(&self) -> PathBuf {
        self.dir.join("wsp")
    }

    /// The machine state beside it — `WSP_STATE`.
    pub fn state(&self) -> PathBuf {
        self.dir.join("state")
    }

    /// Somewhere to put what a test needs and the store does not: a socket to
    /// bind, a git repository to stand a checkout in. Under the same directory,
    /// so it goes away with everything else.
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

#[cfg(test)]
impl Drop for Isolated {
    fn drop(&mut self) {
        for key in ["WSP_HOME", "WSP_STATE", "HERDR_SOCKET_PATH"] {
            std::env::remove_var(key);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The clock a wait is measured on, handed to whatever waits rather than read
/// by it.
///
/// **Written by flaky tests rather than by taste, twice.** A loop that polls
/// something until it is ready reads two things from the world: what time it is,
/// and how to do nothing until the next look. A test of such a loop that lets it
/// read both from the machine is not testing the rule, it is testing the
/// machine's load — and this repository has four agents building in it at once
/// by design, so the machine's load is never the quiet number the test was
/// written against. `wsp install` is gated on `wsp verify --release`, which
/// means a timing test fails exactly when several agents are working, which is
/// exactly when somebody needs to install (robustness-054).
///
/// Both halves have to be handed over, and that is the correction to the first
/// attempt (robustness-041, which handed over `now` alone). A test that drives
/// the clock but leaves a real `sleep` in the loop still spends real seconds, so
/// it gets shortened until it is fast, and a shortened window under load is the
/// same flake wearing a smaller number. Here `rest` *is* what advances the
/// clock: waiting is the only thing in a poll loop that takes time, so a fake
/// clock that moves on `rest` and nowhere else makes elapsed time exactly the
/// waiting the loop chose to do, and every assertion about it arithmetic.
///
/// The honest thing this loses is that a real ask takes real time too, so a
/// deadline reached on the wall clock arrives a little sooner than one reached
/// by counting polls. That is a fact about a slow backend and not about the
/// rule, and no test here asserts it. Production is [`Wall`] and reads the
/// machine, as it must.
pub trait Clock {
    /// What time it is.
    fn now(&self) -> Instant;
    /// Do nothing until the next look.
    fn rest(&self, d: Duration);
}

/// The machine's own clock, and a thread that really sleeps. What every caller
/// outside a test uses.
pub struct Wall;

impl Clock for Wall {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn rest(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// A clock the test winds, and the world's other timed events with it.
///
/// Nothing sleeps: `rest` moves the hands and returns, so a loop that polls for
/// five seconds costs nothing and can be checked at its real durations rather
/// than at a hundredth of them. What used to be a `sleep` on a second thread —
/// *the shell turns up eighty milliseconds in* — is [`Dial::at`], a thing that
/// happens when the clock reaches it, which is the same sentence with the race
/// taken out.
///
/// Single-threaded by construction ([`std::cell`] rather than a mutex), because
/// the point is that the waiting loop and the events it is waiting for are no
/// longer racing. The socket under a fake backend is still real; only the clock
/// is not.
#[cfg(test)]
pub struct Dial<'a> {
    at: std::cell::Cell<Instant>,
    began: Instant,
    due: std::cell::RefCell<Vec<(Duration, Box<dyn Fn() + 'a>)>>,
}

#[cfg(test)]
impl<'a> Dial<'a> {
    pub fn new() -> Dial<'a> {
        let began = Instant::now();
        Dial { at: std::cell::Cell::new(began), began, due: std::cell::RefCell::new(Vec::new()) }
    }

    /// Something happens this far in. Used for the half of a race a test is not
    /// driving: a backend that relents, a shell that arrives.
    pub fn at(self, when: Duration, then: impl Fn() + 'a) -> Dial<'a> {
        self.due.borrow_mut().push((when, Box::new(then)));
        self
    }

    /// How far the clock has been wound, which is how long the wait took.
    pub fn elapsed(&self) -> Duration {
        self.at.get().saturating_duration_since(self.began)
    }
}

#[cfg(test)]
impl Clock for Dial<'_> {
    fn now(&self) -> Instant {
        self.at.get()
    }

    fn rest(&self, d: Duration) {
        self.at.set(self.at.get() + d);
        let reached = self.elapsed();
        // Taken out of the list before any of them runs, so a callback is free
        // to touch the dial and none of them fires twice.
        let ripe: Vec<_> = {
            let mut due = self.due.borrow_mut();
            let (ripe, rest) =
                std::mem::take(&mut *due).into_iter().partition(|(w, _)| *w <= reached);
            *due = rest;
            ripe
        };
        for (_, then) in ripe {
            then();
        }
    }
}

pub fn epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn epoch_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Howard Hinnant's civil_from_days: days since epoch -> (year, month, day).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parts(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// `2026-08-14T16:22:51Z`
pub fn now_iso() -> String {
    let (y, m, d, hh, mm, ss) = parts(epoch_secs());
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// `260814` — the stamp task ids carried before they carried a project.
///
/// Nothing allocates with it any more; ids number in their project's space.
/// Kept because `cmd_migrate::is_dated` and every id written before
/// 2026-08-17 still have its shape in them, and because the date an id used to
/// encode is a thing a reader may still want to compute.
///
/// **UTC on purpose, and not to be "fixed" alongside the display dates.** An id
/// is durable and the store is shared across machines: with executors and
/// `--on <machine>` in play, two seats in different zones allocating at the same
/// instant would mint different prefixes for it, and every seat's ids would jump
/// at its own local midnight rather than all at once. A rendered date answers
/// "when did this happen, to me"; an id answers "which task", and the second
/// question has no reader whose clock it should follow.
#[allow(dead_code)]
pub fn today_stamp() -> String {
    let (y, m, d, ..) = parts(epoch_secs());
    format!("{:02}{:02}{:02}", y % 100, m, d)
}

/// Seconds east of UTC at a given instant, from the platform's own zone data.
///
/// This is the whole cost of "store in UTC, render local", and it is the one
/// thing `std::time` will not tell you. The three answers on offer were: parse
/// `/etc/localtime` ourselves (TZif v2, transition tables, a POSIX `TZ` rule
/// parser for dates past the last transition — a few hundred lines to maintain
/// for a two-digit answer), shell out to `date +%z` (a fork and an exec in a
/// binary a session hook runs and every panel re-execs, and it can only give
/// *today's* offset), or take a second dependency and stop claiming one.
///
/// `localtime_r` is the fourth, and it beats all three: no spawn, no dependency,
/// no table parser, and — unlike a cached `date +%z` — it answers *per instant*,
/// so a stamp from the far side of a DST change renders in the offset that was
/// actually in force when it was written. Declared here rather than taken from
/// `libc`, on the same argument `die_on_broken_pipe` makes in `main.rs`: this is
/// a struct and two symbols, against a dependency the README promises not to add.
///
/// The `struct tm` layout is the one thing that could go wrong — `tm_gmtoff` is
/// a BSD extension that macOS, glibc and musl all carry, but it sits behind nine
/// `int`s whose padding we are asserting. So we do not assert it: `localtime_r`
/// fills in the broken-down local time as well as the offset, and we check that
/// the two agree by running the offset back through our own civil arithmetic. If
/// the layout is wrong the fields are garbage, the check fails, and we render
/// UTC — which is exactly what this codebase did before, rather than an hour
/// invented from a misread byte.
fn local_offset(secs: i64) -> i64 {
    #[repr(C)]
    #[derive(Default)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: i64,
        zone: usize,
    }
    extern "C" {
        fn tzset();
        fn localtime_r(clock: *const i64, result: *mut Tm) -> *mut Tm;
    }

    // glibc's `localtime_r` is documented not to call `tzset` — it is the whole
    // difference between it and `localtime` — so `TZ` would go unread on Linux
    // without this. Once, because glibc's `tzset` re-stats the zone file on
    // every call and this is on the path of every line the panel draws.
    static TZ: std::sync::Once = std::sync::Once::new();
    TZ.call_once(|| unsafe { tzset() });

    let mut tm = Tm::default();
    let clock = secs;
    if unsafe { localtime_r(&clock, &mut tm) }.is_null() {
        return 0;
    }
    let got = (
        tm.year as i64 + 1900,
        tm.mon as i64 + 1,
        tm.mday as i64,
        tm.hour as i64,
        tm.min as i64,
        tm.sec as i64,
    );
    if got == parts(secs + tm.gmtoff) {
        tm.gmtoff
    } else {
        0
    }
}

/// True for the two shapes a stored dated line can carry: `2026-08-14` and
/// `2026-08-14T16:22:51Z`.
///
/// Both, for ever. Every line written before 2026-08-17 stored a bare UTC *date*
/// and the hour is simply gone — there is nothing to convert it from — so a
/// reader that accepted only the new shape would stop seeing the entire history
/// it exists to show.
pub fn is_stamp(s: &str) -> bool {
    let ymd = |s: &str| s.len() == 10 && s.bytes().all(|c| c.is_ascii_digit() || c == b'-');
    ymd(s) || (s.len() == 20 && s.ends_with('Z') && s.as_bytes()[10] == b'T' && ymd(&s[..10]))
}

/// A stored stamp as the reader's own calendar has it: `2026-08-17T01:15:23Z`
/// in Berlin is `2026-08-17`, not the `2026-08-16` its UTC half reads.
///
/// A bare date comes back untouched — see [`is_stamp`] — and so does anything
/// that is not a stamp at all, because a renderer's job here is to convert what
/// it recognises and pass on what it does not.
pub fn local_ymd(stamp: &str) -> String {
    if stamp.len() != 20 || !is_stamp(stamp) {
        return stamp.to_string();
    }
    let at = epoch_of(stamp);
    ymd_at(at, local_offset(at))
}

/// The date an instant falls on, `offset` seconds east of UTC. Split out from
/// [`local_ymd`] so the arithmetic that actually moves the date across midnight
/// can be tested against a zone we choose, rather than against wherever the
/// machine running the tests happens to be.
fn ymd_at(secs: i64, offset: i64) -> String {
    let (y, m, d, ..) = parts(secs + offset);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The inverse of `civil_from_days`: (year, month, day) -> days since epoch.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Days between an ISO date (or timestamp) and today. Returns 0 on parse failure.
pub fn age_days(iso: &str) -> i64 {
    if iso.len() < 10 {
        return 0;
    }
    let y: i64 = iso[0..4].parse().unwrap_or(0);
    let m: i64 = iso[5..7].parse().unwrap_or(1);
    let d: i64 = iso[8..10].parse().unwrap_or(1);
    epoch_secs().div_euclid(86_400) - days_from_civil(y, m, d)
}

/// Seconds since the epoch for `2026-08-14T16:22:51Z`; a bare date is midnight.
/// 0 when the stamp cannot be read, which every caller treats as "no time
/// recorded" rather than 1970 — a record written before this existed has no
/// start, and inventing one would date it to the epoch.
pub fn epoch_of(iso: &str) -> i64 {
    if iso.len() < 10 {
        return 0;
    }
    let n = |a: usize, b: usize| -> i64 { iso.get(a..b).and_then(|s| s.parse().ok()).unwrap_or(0) };
    let days = days_from_civil(n(0, 4), n(5, 7).max(1), n(8, 10).max(1));
    let (hh, mm, ss) = if iso.len() >= 19 { (n(11, 13), n(14, 16), n(17, 19)) } else { (0, 0, 0) };
    days * 86_400 + hh * 3600 + mm * 60 + ss
}

/// How long ago a stamp was, in seconds. Never negative: a stamp from the
/// future is two clocks disagreeing, not a duration to display as one.
pub fn since(iso: &str) -> i64 {
    match epoch_of(iso) {
        0 => 0,
        then => (epoch_secs() - then).max(0),
    }
}

/// Two units at most. This goes in a log line and a one-line field, and nobody
/// reads `3h 12m 07s` off either.
pub fn duration_human(secs: i64) -> String {
    let s = secs.max(0);
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h{m}m"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d{h}h"),
    }
}

/// This machine's name. Claims are machine-local — a workspace on the laptop
/// means nothing on another host — and the store is shared, so anything
/// crossing that line has to say where it came from.
pub fn hostname() -> String {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return h;
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Expand a leading `~` and `$HOME`. Leaves everything else alone.
pub fn expand(p: &str) -> PathBuf {
    let p = p.trim();
    if p == "~" {
        return home();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home().join(rest);
    }
    if let Some(rest) = p.strip_prefix("$HOME/") {
        return home().join(rest);
    }
    PathBuf::from(p)
}

/// Contract `$HOME/x` back to `~/x` for display and storage.
pub fn contract(p: &Path) -> String {
    let h = home();
    match p.strip_prefix(&h) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// Best-effort canonicalisation: resolves symlinks when the path exists,
/// otherwise just expands it. Used for root matching, so it must not fail.
pub fn real(p: &str) -> PathBuf {
    let e = expand(p);
    std::fs::canonicalize(&e).unwrap_or(e)
}

pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

pub fn truncate(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    if n <= 1 {
        return chars.iter().take(n).collect();
    }
    let mut out: String = chars.iter().take(n - 1).collect();
    out.push('…');
    out
}

/// Break `text` to `w` columns on word boundaries, falling back to a hard cut
/// for a single word longer than the pane is wide.
///
/// Here rather than in the detail pane it was written for: the panel's focus
/// dock breaks the same titles to the same rule, and two copies of a wrap would
/// agree until one of them learned about a hyphen.
pub fn wrap(text: &str, w: usize) -> Vec<String> {
    let w = w.max(8);
    let mut out: Vec<String> = Vec::new();
    for para in text.split('\n') {
        if para.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        for word in para.split_whitespace() {
            if word.chars().count() > w {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut rest: Vec<char> = word.chars().collect();
                while rest.len() > w {
                    out.push(rest[..w].iter().collect());
                    rest = rest[w..].to_vec();
                }
                cur = rest.into_iter().collect();
                continue;
            }
            let need = if cur.is_empty() { word.chars().count() } else { cur.chars().count() + 1 + word.chars().count() };
            if need > w {
                out.push(std::mem::take(&mut cur));
                cur = word.to_string();
            } else {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(word);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

/// The leading sentence, for prose that is read as an index rather than in
/// full.
///
/// A decision is written the way a commit message is: the rule first, the
/// argument after it. Measured across the eighteen on `wsp`, the first sentence
/// averages 106 characters against 764 for the whole entry, and in every one of
/// them it is the part that says what was settled — "Stage through a private
/// index, not the shared one." is fifty characters of nine hundred and seventy.
/// So a reader deciding whether *this* is the entry they wanted needs the first
/// sentence and nothing else.
///
/// A sentence ends at `.`, `?` or `!` followed by a space. Not at end-of-string,
/// because a body with no internal break is already one sentence and there is
/// nothing to cut. Abbreviations and the like will occasionally end one early;
/// the cost of that is a short line in an index, which is what this is for.
pub fn first_sentence(s: &str) -> &str {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if !matches!(b[i], b'.' | b'?' | b'!') {
            continue;
        }
        // The terminator has to be followed by a break, or `1.5s` and `e.g.`
        // cut the line in half.
        match b.get(i + 1) {
            Some(c) if c.is_ascii_whitespace() => return s[..=i].trim_end(),
            _ => {}
        }
    }
    s.trim_end()
}

pub fn pad(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len >= n {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(n - len))
    }
}

// ---- colour -------------------------------------------------------------

pub fn colour_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    stdout_is_tty()
}

/// Whether anything we print is going to a terminal at all.
///
/// Separate from [`colour_enabled`], which is the same question plus `NO_COLOR`
/// — a pane whose owner has turned colour off is still a pane, and a full-screen
/// view has to know the difference between "print this plainly" and "do not
/// take over the screen".
pub fn stdout_is_tty() -> bool {
    // Not a perfect isatty, but avoids pulling libc in for one call.
    unsafe { is_tty(1) }
}

/// Whether a command told to read stdin has anything to read.
///
/// A terminal answers `read` by waiting, so a `-` typed with nothing piped in
/// is not an empty note — it is a command that has stopped, with no output, in
/// a pane whose keystrokes are now going somewhere nobody can see. Refusing it
/// is the only answer that says what happened.
pub fn stdin_is_tty() -> bool {
    unsafe { is_tty(0) }
}

unsafe fn is_tty(fd: i32) -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    isatty(fd) == 1
}

pub struct Paint {
    on: bool,
}

impl Paint {
    pub fn new() -> Self {
        Paint { on: colour_enabled() }
    }
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    pub fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.wrap("1", s)
    }
    pub fn green(&self, s: &str) -> String {
        self.wrap("32", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.wrap("33", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.wrap("31", s)
    }
    pub fn cyan(&self, s: &str) -> String {
        self.wrap("36", s)
    }
    /// Struck through, for a line that is in the record and no longer true —
    /// a superseded decision. Never the only mark: SGR 9 is well supported but
    /// not universal, and `NO_COLOR` turns it off entirely, so every caller
    /// also says in words what the strike means.
    pub fn strike(&self, s: &str) -> String {
        self.wrap("9", s)
    }
}

/// Size and mtime of the binary we are running. Cheap enough to check on every
/// tick, and enough to notice an `install` underneath us.
///
/// Lives here rather than beside the panel for the same reason `shell_quote`
/// does: three unrelated long-lived processes now ask whether they have been
/// replaced — the panel, the detail view and the daemon — and the daemon has
/// no business importing the panel to find out.
///
/// Both halves are needed. A rebuild that changes nothing but a constant can
/// come out the same length, and mtime is then the only thing that moved; two
/// installs inside one second on a filesystem keeping whole seconds share an
/// mtime, and size is then the only thing that moved. `None` means we could
/// not read our own path, which is a reason not to reload rather than a
/// reason to.
pub fn exe_stamp() -> Option<(u64, u64)> {
    stamp(&std::env::current_exe().ok()?)
}

/// [`exe_stamp`] against a path you name, which is the half that can be tested.
pub fn stamp(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(path).ok()?;
    let secs = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some((m.len(), secs))
}

/// Wrap a string so a shell takes it as one literal argument.
///
/// Single quotes, with an embedded quote spelled the only way `sh` allows:
/// close, escape, reopen. Lives here rather than beside the panel because two
/// unrelated places now build shell for a pane to run — the panel when it
/// opens an edit tab, and the detail pane when the menu adds a column — and a
/// quoting rule with two copies is a quoting rule with one bug.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    /// The premise this was first written on was false: `HERDR_SOCKET_PATH` is
    /// not "unset outside a sandbox", it is set in *every* live herdr pane. So
    /// carrying it unconditionally put this machine's socket path into the env
    /// of a workspace created on another machine by `wsp spawn --on <machine>`
    /// — which works by accident while two home layouts match and fails
    /// silently when a username differs. The rule is that the socket travels
    /// only with `WSP_BIN`, which nothing but `wsp sandbox` sets, and this is
    /// the claim worth pinning rather than the code that implements it.
    #[test]
    fn the_socket_travels_only_inside_a_sandbox() {
        let live = |k: &str| match k {
            "WSP_HOME" => Some("/Users/edjames/wsp".to_string()),
            "WSP_STATE" => Some("/Users/edjames/.local/state/wsp".to_string()),
            // Set in every pane herdr makes, which is the whole point.
            "HERDR_SOCKET_PATH" => Some("/Users/edjames/.config/herdr/herdr.sock".to_string()),
            _ => None,
        };
        let env = store_env_from(live);
        assert!(
            !env.contains_key("HERDR_SOCKET_PATH"),
            "a spawn from a live pane would carry this machine's socket to another machine: {env:?}"
        );
        assert_eq!(env.get("WSP_HOME").and_then(|v| v.as_str()), Some("/Users/edjames/wsp"));

        // …and inside a sandbox it is the one thing that must travel, or the
        // workspace comes up with its panes on the live server.
        let sandboxed = |k: &str| match k {
            "WSP_BIN" => Some("/Users/edjames/claude/wsp/target/debug/wsp".to_string()),
            other => live(other),
        };
        let env = store_env_from(sandboxed);
        assert_eq!(
            env.get("HERDR_SOCKET_PATH").and_then(|v| v.as_str()),
            Some("/Users/edjames/.config/herdr/herdr.sock"),
            "a sandbox spawned a workspace that could not find its own herdr"
        );
    }

    fn write_at(path: &Path, body: &str, at: SystemTime) {
        fs::write(path, body).unwrap();
        let f = fs::File::options().write(true).open(path).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(at)).unwrap();
    }

    /// Both halves of the stamp earn their place, and each covers a case the
    /// other misses. A rebuild that changes a constant can come out exactly as
    /// long as the last one, and only the mtime moves; two installs inside one
    /// second share an mtime on a filesystem keeping whole seconds, and only
    /// the length moves. A stamp reduced to either half alone would let a
    /// long-lived process go on executing a binary that is no longer on disk.
    #[test]
    fn a_replaced_binary_is_never_the_same_stamp() {
        let dir = std::env::temp_dir().join(format!("wsp-stamp-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wsp");
        let t = UNIX_EPOCH + Duration::from_secs(1_800_000_000);

        write_at(&path, "aaaa", t);
        let first = stamp(&path).unwrap();

        // Same length, one second later: size alone would call this unchanged.
        write_at(&path, "bbbb", t + Duration::from_secs(1));
        let same_size = stamp(&path).unwrap();
        assert_ne!(first, same_size, "a same-length rebuild read as the same binary");

        // Same second, longer: mtime alone would call this unchanged.
        write_at(&path, "bbbbb", t + Duration::from_secs(1));
        let same_second = stamp(&path).unwrap();
        assert_ne!(same_size, same_second, "a second install in the same second was invisible");

        let _ = fs::remove_dir_all(&dir);
    }

    /// A path we cannot read is not a change. The callers reload on `!=`, so a
    /// `None` treated as a value would make every unreadable moment — an
    /// install mid-copy — look like a new binary worth exec'ing into.
    #[test]
    fn a_path_that_is_not_there_has_no_stamp() {
        assert_eq!(stamp(Path::new("/nonexistent/wsp")), None);
    }

    /// The index line has to be the rule, not the first clause of the argument
    /// for it. These are real entries out of `~/wsp/projects/wsp.md`.
    #[test]
    fn a_decision_cut_to_its_first_sentence_is_still_the_decision() {
        assert_eq!(
            first_sentence("Empty is not quiet. The panel's quiet-branch filter hides a project with no open tasks."),
            "Empty is not quiet."
        );
        assert_eq!(
            first_sentence("Stage through a private index, not the shared one. export GIT_INDEX_FILE=/tmp/x"),
            "Stage through a private index, not the shared one."
        );
    }

    /// A full stop is only a sentence end when something breaks after it.
    /// Versions, abbreviations and file names are full of the character and
    /// none of them end a sentence, so cutting at the first one seen would
    /// leave `Build with cargo 1.` in an index.
    #[test]
    fn a_full_stop_inside_a_word_does_not_end_a_sentence() {
        assert_eq!(
            first_sentence("Pin cargo 1.84.0 in CI. Newer toolchains warn."),
            "Pin cargo 1.84.0 in CI."
        );
        assert_eq!(first_sentence("Read src/util.rs first"), "Read src/util.rs first");
    }

    /// Prose with no internal break is already one sentence. Returning nothing,
    /// or an ellipsis, would lose the whole entry rather than shorten it.
    #[test]
    fn prose_that_never_breaks_comes_back_whole() {
        assert_eq!(first_sentence("one working tree per agent is not yet"), "one working tree per agent is not yet");
        assert_eq!(first_sentence("A decision.  "), "A decision.");
        assert_eq!(first_sentence(""), "");
    }

    /// The reported night, as arithmetic. Four decisions taken at 01:15 CEST
    /// came back stamped with the previous day, because 01:15 CEST is 23:15 UTC
    /// and the UTC date is what was written down. The offset is passed in
    /// rather than read from the machine so that this asserts the crossing
    /// itself, on every machine, rather than agreeing with wherever it runs.
    #[test]
    fn an_instant_before_utc_midnight_is_already_tomorrow_further_east() {
        let at = epoch_of("2026-08-16T23:15:00Z");
        assert_eq!(ymd_at(at, 0), "2026-08-16", "the instant as it is stored");
        assert_eq!(ymd_at(at, 2 * 3600), "2026-08-17", "01:15 in Berlin, the night this was reported");
        assert_eq!(ymd_at(at, -7 * 3600), "2026-08-16", "16:15 the previous afternoon in California");

        // And the other edge: early UTC is still yesterday to the west.
        let dawn = epoch_of("2026-08-17T04:00:00Z");
        assert_eq!(ymd_at(dawn, -7 * 3600), "2026-08-16", "21:00 the evening before");
        assert_eq!(ymd_at(dawn, 2 * 3600), "2026-08-17");
    }

    /// Both shapes, for ever. Every line written before 2026-08-17 stored a
    /// bare UTC date with the hour already discarded, so a reader that took
    /// only the new shape would stop seeing the history it exists to show —
    /// and one that tried to convert it would invent an hour it never had.
    #[test]
    fn a_date_stored_before_the_hour_was_kept_is_shown_exactly_as_it_stands() {
        assert!(is_stamp("2026-08-16"));
        assert!(is_stamp("2026-08-16T23:15:00Z"));
        assert!(!is_stamp("2026-08-16T23:15:00"), "an instant with no zone is not one of ours");
        assert!(!is_stamp("blocked:"));
        assert!(!is_stamp(""));

        assert_eq!(local_ymd("2026-08-16"), "2026-08-16", "no hour to convert from");
        assert_eq!(local_ymd("blocked:"), "blocked:", "not a stamp, not our business");
        assert_eq!(local_ymd("2026-08-16T23:15:00Z").len(), 10, "an instant renders as a date");
    }

    /// The one thing here we do not compute is the offset, and `local_offset`
    /// gets it out of a `struct tm` whose layout it is asserting — nine `int`s
    /// and the padding after them. It checks its own reading and falls back to
    /// UTC when the check fails, which means a wrong layout costs nothing but
    /// also *says* nothing: zero is a plausible offset.
    ///
    /// So this asks the platform the same question by a route that shares no
    /// code with ours. Shelling out is what the running binary must not do —
    /// it is a fork on the path of every line the panel draws — and it is
    /// exactly right in a test, which runs once and can afford the truth.
    #[test]
    fn the_offset_we_read_out_of_libc_is_the_one_the_system_reports() {
        let out = match std::process::Command::new("date").arg("+%z").output() {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            // No `date` on the box is not a claim about this code.
            _ => return,
        };
        let (sign, hhmm) = out.split_at(1);
        let hh: i64 = hhmm[..2].parse().expect("+HHMM");
        let mm: i64 = hhmm[2..4].parse().expect("+HHMM");
        let want = (hh * 3600 + mm * 60) * if sign == "-" { -1 } else { 1 };
        assert_eq!(
            local_offset(epoch_secs()),
            want,
            "libc says one thing and `date {out}` says another — the tm layout is being misread"
        );
    }
}
