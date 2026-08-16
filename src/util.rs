//! Small helpers: time without chrono, path expansion, terminal colour.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The store we are talking to, as environment for anything we spawn.
///
/// A pane, tab or workspace herdr creates starts a fresh shell, which inherits
/// nothing from us — so a panel pointed at a non-default store would open
/// editors pointed at the default one. They would then fail to find the task
/// and take the tab down with them, which looks exactly like the key not
/// working. An agent spawned onto a task has the same problem and a longer
/// fuse: it would sit in a tree reading the wrong backlog.
pub fn store_env() -> serde_json::Map<String, serde_json::Value> {
    let mut env = serde_json::Map::new();
    for key in ["WSP_HOME", "WSP_STATE", "WSP_NO_COMMIT"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                env.insert(key.to_string(), serde_json::Value::String(v));
            }
        }
    }
    env
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

/// `2026-08-14`
pub fn today_ymd() -> String {
    let (y, m, d, ..) = parts(epoch_secs());
    format!("{y:04}-{m:02}-{d:02}")
}

/// `260814` — the task-id date stamp.
pub fn today_stamp() -> String {
    let (y, m, d, ..) = parts(epoch_secs());
    format!("{:02}{:02}{:02}", y % 100, m, d)
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
    unsafe { is_tty() }
}

unsafe fn is_tty() -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    isatty(1) == 1
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
}
