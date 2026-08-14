//! Small helpers: time without chrono, path expansion, terminal colour.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Days between an ISO date (or timestamp) and today. Returns 0 on parse failure.
pub fn age_days(iso: &str) -> i64 {
    if iso.len() < 10 {
        return 0;
    }
    let y: i64 = iso[0..4].parse().unwrap_or(0);
    let m: i64 = iso[5..7].parse().unwrap_or(1);
    let d: i64 = iso[8..10].parse().unwrap_or(1);
    // days_from_civil
    let yy = if m <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let then = era * 146_097 + doe - 719_468;
    epoch_secs().div_euclid(86_400) - then
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
