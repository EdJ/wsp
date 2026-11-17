//! The tree-job runner (`docs/agent-manager.md` 6.2 M3, wsp-049): one job's
//! argv, run where the caller places it, in its own process group, to its end.
//! The agent manager links it to serve `job.run`, and so does wsp's in-process
//! host, so the rollback ("jobs in-process behind a flag") runs this code.
//!
//! - **The job** is `sh -c WRAPPER sh <exit> argv…` with `process_group(0)`,
//!   stdin null, and stdout and stderr both to `<exec_dir>/jobs/<key>.log`.
//!   The argv carries its own `env -u K -- K=V …` prefix (wsp-044); the
//!   runner adds nothing to it and strips wsp's own variables, as
//!   `check::run_step` does.
//! - **The end** is written by the wrapper to `<exec_dir>/jobs/<key>.exit`
//!   (atomically, through a `.tmp`), so a job that ends while no runner is
//!   listening is not lost (2.5). An end the runner makes itself (timeout,
//!   hung, `job.stop`, a signal from outside) it writes there in the same
//!   form, so the file is the job's end however it came.
//! - **The bounds** are `timeout_secs` from the start, whatever the job does,
//!   and `hung_after_secs` of no output and no CPU, judged as `run_step`
//!   judges it (wsp-609): the log's size and the group's CPU, probed every
//!   quarter of the bound. This is `run_step`'s loop, ported, not called.
//! - **The group is killed** (`KILL`) on every end, success included
//!   (wsp-453): the wrapper ends its own group once it has written the exit
//!   file, so a background child does not outlive a job nobody is watching,
//!   and the runner signals the group again on every end it sees.
//! - **CPU** is the group's, in milliseconds: the larger of the most the
//!   probes saw the group spend and what the wrapper's `times` says its
//!   waited children spent.
//! - **Re-adoption**: `<exec_dir>/jobs/<key>.job` records the pid (the group
//!   leader, so also the pgid), its start time as `ps` gives it, and the
//!   bounds. [`adopt`] after a restart reports a job that ended, keeps one
//!   still running whose pid has that start time, and calls the rest lost.
//! - **Output** is read back from any offset ([`read_from`]) for `job.output`.

use std::fs;
use std::io::{self, Read as _, Seek, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use wsp_base::calls::{Child, Command};
use wsp_host_api::{FileRef, HostId, JobEnded, JobKey};

use crate::inproc::InProcess;

/// The wrapper: run the job, write its end, then end its own group. `$1` is
/// the exit file; the job's argv follows.
const WRAPPER: &str = r#"e=$1; shift
"$@"
s=$?
{ echo "exit $s"; times; } > "$e.tmp" 2>/dev/null && mv -f "$e.tmp" "$e"
kill -KILL 0"#;

/// What to run, and where.
#[derive(Debug, Clone)]
pub struct Spec<'a> {
    /// The job's name on disk ([`file_key`] of its [`JobKey`]).
    pub key: &'a str,
    pub argv: &'a [String],
    /// Its place on the host: the tree or directory it runs in.
    pub cwd: &'a Path,
    pub timeout: Duration,
    pub hung_after: Option<Duration>,
}

/// How a job ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct End {
    /// The exit status, when it exited rather than was ended.
    pub exit: Option<i32>,
    /// The signal that ended it from outside, when one did (not a bound of
    /// ours, not `job.stop`).
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub hung: bool,
    /// Ended by [`Job::stop`].
    pub stopped: bool,
    pub cpu_ms: u64,
}

impl End {
    /// The `job.ended` this end makes, its log named on `host`.
    pub fn ended(&self, key: JobKey, host: HostId, log: &Path) -> JobEnded {
        JobEnded {
            key,
            exit: self.exit,
            timed_out: self.timed_out,
            hung: self.hung,
            log: FileRef { host, path: log.to_string_lossy().into_owned() },
            cpu_ms: self.cpu_ms,
        }
    }

    /// The exit file's lines.
    fn text(&self) -> String {
        let mut s = match self.exit {
            Some(n) => format!("exit {n}\n"),
            None => "exit -\n".to_string(),
        };
        if let Some(sig) = self.signal {
            s.push_str(&format!("signal {sig}\n"));
        }
        for (flag, word) in [(self.timed_out, "timed_out"), (self.hung, "hung"), (self.stopped, "stopped")] {
            if flag {
                s.push_str(word);
                s.push('\n');
            }
        }
        s.push_str(&format!("cpu_ms {}\n", self.cpu_ms));
        s
    }
}

/// The exit file read back: what the wrapper wrote (`exit N` and `times`'s
/// two lines) or what the runner wrote. `None` without an `exit` line.
pub fn parse_end(text: &str) -> Option<End> {
    let mut end = End::default();
    let mut saw_exit = false;
    let mut times = Vec::new();
    for line in text.lines().map(str::trim) {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match word {
            "exit" => {
                saw_exit = true;
                end.exit = rest.trim().parse().ok();
            }
            "signal" => end.signal = rest.trim().parse().ok(),
            "timed_out" => end.timed_out = true,
            "hung" => end.hung = true,
            "stopped" => end.stopped = true,
            "cpu_ms" => end.cpu_ms = end.cpu_ms.max(rest.trim().parse().unwrap_or(0)),
            _ => {
                if let Some(ms) = times_line(line) {
                    times.push(ms);
                }
            }
        }
    }
    // `times` prints the shell's own line, then its children's: the job.
    if let Some(&children) = times.last() {
        end.cpu_ms = end.cpu_ms.max(children);
    }
    saw_exit.then_some(end)
}

/// User plus system milliseconds out of one of `times`'s lines,
/// `0m0.003s 0m0.004s` (dash prints six places, bash three).
fn times_line(line: &str) -> Option<u64> {
    let mut parts = line.split_whitespace();
    let (u, s) = (parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let ms = |t: &str| -> Option<u64> {
        let (m, sec) = t.strip_suffix('s')?.split_once('m')?;
        let m: u64 = m.parse().ok()?;
        let sec: f64 = sec.parse().ok()?;
        Some(m * 60_000 + (sec * 1000.0).round() as u64)
    };
    Some(ms(u)? + ms(s)?)
}

/// A [`JobKey`] as a file name: its row, attempt and job joined by `+`, every
/// byte outside `[A-Za-z0-9._@-]` written `%XX`, so two keys never share one.
pub fn file_key(key: &JobKey) -> String {
    let esc = |s: &str| {
        let mut out = String::new();
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b'-') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    };
    format!("{}+{}+{}", esc(&key.row.0), key.attempt, esc(&key.job))
}

/// `<exec_dir>/jobs`, where every job's files are.
pub fn jobs_dir(exec_dir: &Path) -> PathBuf {
    exec_dir.join("jobs")
}

/// `<exec_dir>/jobs/<key>.log`.
pub fn log_path(exec_dir: &Path, key: &str) -> PathBuf {
    jobs_dir(exec_dir).join(format!("{key}.log"))
}

/// `<exec_dir>/jobs/<key>.exit`.
pub fn exit_path(exec_dir: &Path, key: &str) -> PathBuf {
    jobs_dir(exec_dir).join(format!("{key}.exit"))
}

/// `<exec_dir>/jobs/<key>.job`.
pub fn record_path(exec_dir: &Path, key: &str) -> PathBuf {
    jobs_dir(exec_dir).join(format!("{key}.job"))
}

/// The end in a job's exit file, if it has one.
pub fn read_end(exec_dir: &Path, key: &str) -> Option<End> {
    parse_end(&fs::read_to_string(exit_path(exec_dir, key)).ok()?)
}

/// What `<key>.job` records of a started job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The wrapper's pid, which leads the job's group: the pgid too.
    pub pid: u32,
    /// Its start time as `ps -o lstart=` gives it (words joined by `_`):
    /// with the pid, what tells this job from a later process given its pid.
    pub started: String,
    /// When it was started, in milliseconds since the epoch: the timeout's
    /// clock, which a restart keeps.
    pub started_ms: u64,
    pub timeout_secs: u64,
    pub hung_after_secs: Option<u64>,
}

impl Record {
    fn text(&self) -> String {
        let mut s = format!(
            "pid {}\nstarted {}\nstarted_ms {}\ntimeout_secs {}\n",
            self.pid, self.started, self.started_ms, self.timeout_secs
        );
        if let Some(h) = self.hung_after_secs {
            s.push_str(&format!("hung_after_secs {h}\n"));
        }
        s
    }

    fn parse(text: &str) -> Option<Record> {
        let get = |name: &str| text.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(' ').map(str::trim));
        Some(Record {
            pid: get("pid")?.parse().ok()?,
            started: get("started")?.to_string(),
            started_ms: get("started_ms")?.parse().ok()?,
            timeout_secs: get("timeout_secs")?.parse().ok()?,
            hung_after_secs: get("hung_after_secs").and_then(|h| h.parse().ok()),
        })
    }
}

/// The record of the job `key`, if it was started here.
pub fn recorded(exec_dir: &Path, key: &str) -> Option<Record> {
    Record::parse(&fs::read_to_string(record_path(exec_dir, key)).ok()?)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Write `text` to `path` through a `.tmp` beside it, so a reader sees all of
/// it or none.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// A job this runner watches: one it started, or one it re-adopted.
#[derive(Debug)]
pub struct Job {
    pub key: String,
    pub record: Record,
    pub log: PathBuf,
    pub exit: PathBuf,
    /// The wrapper, when this process started it; `None` for an adopted job,
    /// which this process cannot wait on and watches by pid instead.
    child: Option<Child>,
    /// The log's size and the group's CPU (hundredths) at the last probe.
    seen: (u64, u64),
    quiet_since: Instant,
    probed: Instant,
    /// The most CPU (hundredths) a probe saw the group spend.
    peak_cc: u64,
}

/// What [`adopt`] found.
#[derive(Debug)]
pub enum Adopted {
    /// Still running, and watched again.
    Running(Job),
    /// It ended, and its exit file says how.
    Ended(End),
    /// No exit file, and its pid is gone or is another process now.
    Lost,
}

/// Start the job `spec` names, its files under `<exec_dir>/jobs`.
pub fn start(host: &InProcess, exec_dir: &Path, spec: &Spec<'_>) -> io::Result<Job> {
    if spec.argv.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a job needs an argv"));
    }
    fs::create_dir_all(jobs_dir(exec_dir))?;
    let (log, exit) = (log_path(exec_dir, spec.key), exit_path(exec_dir, spec.key));
    // A key is one run: what an earlier run of the same name left is not this
    // run's end.
    let _ = fs::remove_file(&exit);
    let out = fs::File::create(&log)?;
    let err = out.try_clone()?;
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(WRAPPER)
        .arg("sh")
        .arg(&exit)
        .args(spec.argv)
        .current_dir(spec.cwd)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0);
    wsp_base::util::clean_env(&mut cmd);
    let started_ms = now_ms();
    let child = cmd.spawn()?;
    let pid = child.id();
    let record = Record {
        pid,
        started: host.started(pid).ok().flatten().unwrap_or_default(),
        started_ms,
        timeout_secs: spec.timeout.as_secs(),
        hung_after_secs: spec.hung_after.map(|h| h.as_secs()),
    };
    write_atomic(&record_path(exec_dir, spec.key), &record.text())?;
    Ok(Job::new(spec.key.to_string(), record, log, exit, Some(child)))
}

/// After a restart: the job `key`, started as `pid` at `started` (`ps`'s
/// lstart, as [`Record::started`] holds it). Its exit file is its end; with
/// none, a live `pid` that started at `started` is the job, running, and is
/// watched again under the bounds its record holds.
pub fn adopt(host: &InProcess, exec_dir: &Path, key: &str, pid: u32, started: &str) -> Adopted {
    if let Some(end) = read_end(exec_dir, key) {
        return Adopted::Ended(end);
    }
    let theirs = !started.is_empty() && host.is_alive(pid) && host.started(pid).ok().flatten().as_deref() == Some(started);
    if !theirs {
        // It may have written its end between the two looks.
        return read_end(exec_dir, key).map_or(Adopted::Lost, Adopted::Ended);
    }
    let record = recorded(exec_dir, key)
        .filter(|r| r.pid == pid)
        .unwrap_or(Record { pid, started: started.to_string(), started_ms: now_ms(), timeout_secs: u64::MAX / 1000, hung_after_secs: None });
    Adopted::Running(Job::new(key.to_string(), record, log_path(exec_dir, key), exit_path(exec_dir, key), None))
}

impl Job {
    fn new(key: String, record: Record, log: PathBuf, exit: PathBuf, child: Option<Child>) -> Job {
        let now = Instant::now();
        Job { key, record, log, exit, child, seen: (0, 0), quiet_since: now, probed: now, peak_cc: 0 }
    }

    /// The job's process group (its wrapper's pid).
    pub fn pgid(&self) -> u32 {
        self.record.pid
    }

    /// One look: `Some` once the job has ended, by itself or by a bound. A
    /// caller watching many jobs calls this on each in turn.
    pub fn poll(&mut self, host: &InProcess) -> Option<End> {
        // Whether it ended, and the status when this process is its parent.
        let (ended, status) = match self.child.as_mut() {
            Some(c) => match c.try_wait() {
                Ok(Some(s)) => (true, Some(s)),
                Ok(None) => (false, None),
                Err(_) => (true, None),
            },
            None => (self.exit.exists() || !host.is_alive(self.record.pid), None),
        };
        if ended {
            return Some(self.finish(host, status));
        }
        let elapsed = Duration::from_millis(now_ms().saturating_sub(self.record.started_ms));
        if elapsed > Duration::from_secs(self.record.timeout_secs) {
            return Some(self.end_by(host, End { timed_out: true, ..End::default() }));
        }
        let Some(quiet_for) = self.record.hung_after_secs.map(Duration::from_secs) else { return None };
        let probe = (quiet_for / 4).clamp(Duration::from_millis(200), Duration::from_secs(30));
        if self.probed.elapsed() < probe {
            return None;
        }
        self.probed = Instant::now();
        let wrote = fs::metadata(&self.log).map(|m| m.len()).unwrap_or(0);
        let (pids, cpu) = group_of(host, self.pgid())?;
        self.peak_cc = self.peak_cc.max(cpu);
        if (wrote, cpu) != self.seen {
            self.seen = (wrote, cpu);
            self.quiet_since = Instant::now();
            return None;
        }
        if self.quiet_since.elapsed() < quiet_for {
            return None;
        }
        // wsp-609: named with its process tree, as `run_step` names it.
        let note = format!("\nhung — no output and no CPU for {}s; its processes:\n{}\n", quiet_for.as_secs(), pids.join("\n"));
        if let Ok(mut f) = fs::OpenOptions::new().append(true).open(&self.log) {
            let _ = f.write_all(note.as_bytes());
        }
        Some(self.end_by(host, End { hung: true, ..End::default() }))
    }

    /// Watch until the job ends.
    pub fn wait(&mut self, host: &InProcess) -> End {
        loop {
            if let Some(end) = self.poll(host) {
                return end;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// `job.stop`: end the job now. One that already ended is reported as it
    /// ended.
    pub fn stop(&mut self, host: &InProcess) -> End {
        if let Some(end) = self.poll(host) {
            return end;
        }
        self.end_by(host, End { stopped: true, ..End::default() })
    }

    /// What the job wrote from byte `from`, at most `max` bytes: see
    /// [`read_from`].
    pub fn output(&self, from: u64, max: usize) -> io::Result<(String, u64)> {
        read_from(&self.log, from, max)
    }

    /// End the job this runner judged over: its group killed, its end
    /// written.
    fn end_by(&mut self, host: &InProcess, mut end: End) -> End {
        if let Some((_, cpu)) = group_of(host, self.pgid()) {
            self.peak_cc = self.peak_cc.max(cpu);
        }
        host.signal_group(self.pgid(), "KILL");
        if let Some(c) = self.child.as_mut() {
            let _ = c.wait();
        }
        end.cpu_ms = self.peak_cc * 10;
        let _ = write_atomic(&self.exit, &end.text());
        end
    }

    /// The job ended by itself: its exit file says how, or, with none, the
    /// wrapper's own status.
    fn finish(&mut self, host: &InProcess, status: Option<std::process::ExitStatus>) -> End {
        // wsp-453: whatever is left in its group goes, however it ended. The
        // wrapper ends its group itself; this is for the end it did not see.
        host.signal_group(self.pgid(), "KILL");
        let peak_ms = self.peak_cc * 10;
        if let Some(mut end) = parse_end(&fs::read_to_string(&self.exit).unwrap_or_default()) {
            end.cpu_ms = end.cpu_ms.max(peak_ms);
            return end;
        }
        // No exit file: the wrapper was ended before it could write one.
        use std::os::unix::process::ExitStatusExt;
        let end = End {
            exit: status.and_then(|s| s.code()),
            signal: status.and_then(|s| s.signal()),
            cpu_ms: peak_ms,
            ..End::default()
        };
        let _ = write_atomic(&self.exit, &end.text());
        end
    }
}

/// The processes of group `pgid`: (`pid command` lines, their CPU summed in
/// hundredths of a second), as check.rs's `group_of` reads them. `None` where
/// `ps` does not answer.
pub fn group_of(host: &InProcess, pgid: u32) -> Option<(Vec<String>, u64)> {
    let text = host.table("pid=,pgid=,time=,command=", false)?;
    let (mut lines, mut cpu) = (Vec::new(), 0u64);
    for l in text.lines() {
        let mut w = l.split_whitespace();
        let (Some(pid), Some(g), Some(time)) = (w.next(), w.next(), w.next()) else { continue };
        if g.parse::<u32>().ok() != Some(pgid) {
            continue;
        }
        cpu += cpu_of(time);
        let command = w.collect::<Vec<_>>().join(" ");
        lines.push(format!("{pid} {}", wsp_base::util::truncate(&command, 160)));
    }
    Some((lines, cpu))
}

/// `ps`'s `time` (`[[dd-]hh:]mm:ss[.cc]`) in hundredths of a second.
pub fn cpu_of(time: &str) -> u64 {
    let (days, rest) = match time.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().unwrap_or(0), r),
        None => (0, time),
    };
    let (whole, frac) = rest.split_once('.').unwrap_or((rest, "0"));
    let secs = whole.split(':').fold(0u64, |acc, p| acc * 60 + p.parse::<u64>().unwrap_or(0));
    let cc: u64 = format!("{frac:0<2}")[..2].parse().unwrap_or(0);
    (days * 86_400 + secs) * 100 + cc
}

/// What `log` holds from byte `from`, at most `max` bytes, as text, and the
/// offset the next read starts at. A character cut by the end of what is
/// there is left for the next read; any other byte that is not UTF-8 is
/// replaced, so the text is always text. Past the end it is empty, at `from`.
pub fn read_from(log: &Path, from: u64, max: usize) -> io::Result<(String, u64)> {
    let mut f = fs::File::open(log)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::with_capacity(max.min(1 << 20));
    f.take(max as u64).read_to_end(&mut buf)?;
    let keep = buf.len() - cut_char(&buf);
    buf.truncate(keep);
    Ok((String::from_utf8_lossy(&buf).into_owned(), from + keep as u64))
}

/// How many bytes at the end of `buf` begin a UTF-8 character they do not
/// finish.
fn cut_char(buf: &[u8]) -> usize {
    for back in 1..=buf.len().min(3) {
        let b = buf[buf.len() - back];
        if b & 0xC0 == 0x80 {
            continue;
        }
        let need = match b {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return 0,
        };
        return if need > back { back } else { 0 };
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_host_api::RowId;

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::var_os("WSP_SCRATCH").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("host-jobs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    fn spec<'a>(key: &'a str, argv: &'a [String], cwd: &'a Path, timeout: u64, hung: Option<u64>) -> Spec<'a> {
        Spec { key, argv, cwd, timeout: Duration::from_secs(timeout), hung_after: hung.map(Duration::from_secs) }
    }

    /// Wait up to `secs` for `f` to hold.
    fn within(secs: u64, mut f: impl FnMut() -> bool) -> bool {
        let until = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < until {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        f()
    }

    #[test]
    fn a_jobs_group_is_gone_after_it_exits_0_its_background_child_included() {
        let (host, dir) = (InProcess::default(), scratch("group"));
        let argv = sh("sleep 60 & echo $! > bg.pid; echo started");
        let mut job = start(&host, &dir, &spec("group", &argv, &dir, 30, None)).unwrap();
        let end = job.wait(&host);
        assert_eq!(end.exit, Some(0), "{end:?}");
        assert!(!end.timed_out && !end.hung && end.signal.is_none(), "{end:?}");
        let bg: u32 = fs::read_to_string(dir.join("bg.pid")).unwrap().trim().parse().unwrap();
        assert!(within(5, || !host.is_alive(bg)), "the background sleep {bg} outlived its job");
        assert!(within(5, || group_of(&host, job.pgid()).is_some_and(|(p, _)| p.is_empty())), "group {} still has processes", job.pgid());
        assert_eq!(fs::read_to_string(&job.log).unwrap(), "started\n");
        assert_eq!(read_end(&dir, "group").map(|e| e.exit), Some(Some(0)));
    }

    #[test]
    fn a_job_past_its_timeout_is_ended_and_says_it_timed_out() {
        let (host, dir) = (InProcess::default(), scratch("timeout"));
        let argv = sh("echo going; sleep 60");
        let t = Instant::now();
        let mut job = start(&host, &dir, &spec("timeout", &argv, &dir, 1, None)).unwrap();
        let end = job.wait(&host);
        assert!(end.timed_out && !end.hung, "{end:?}");
        assert_eq!(end.exit, None);
        assert!(t.elapsed() < Duration::from_secs(20), "took {:?}", t.elapsed());
        assert!(within(5, || !host.is_alive(job.pgid())));
        assert!(read_end(&dir, "timeout").is_some_and(|e| e.timed_out), "the end is written for a restart to read");
    }

    #[test]
    fn a_quiet_job_past_its_hung_bound_is_ended_and_says_it_hung() {
        let (host, dir) = (InProcess::default(), scratch("hung"));
        let argv = sh("sleep 60");
        let t = Instant::now();
        let mut job = start(&host, &dir, &spec("hung", &argv, &dir, 120, Some(1))).unwrap();
        let end = job.wait(&host);
        assert!(end.hung && !end.timed_out, "{end:?}");
        assert!(t.elapsed() < Duration::from_secs(20), "took {:?}", t.elapsed());
        let log = fs::read_to_string(&job.log).unwrap();
        assert!(log.contains("hung — no output and no CPU for 1s") && log.contains("sleep 60"), "{log}");
        assert!(within(5, || !host.is_alive(job.pgid())));
        assert!(read_end(&dir, "hung").is_some_and(|e| e.hung));
    }

    #[test]
    fn an_exit_file_written_while_no_runner_listened_is_read_back_as_the_end() {
        let (host, dir) = (InProcess::default(), scratch("unheard"));
        let argv = sh("echo bye; exit 3");
        let job = start(&host, &dir, &spec("unheard", &argv, &dir, 30, None)).unwrap();
        let (pid, started) = (job.record.pid, job.record.started.clone());
        // Nobody listens: the runner is gone before the job ends.
        drop(job);
        assert!(within(10, || exit_path(&dir, "unheard").exists()), "the wrapper writes the end itself");
        assert_eq!(recorded(&dir, "unheard").map(|r| r.pid), Some(pid));
        match adopt(&host, &dir, "unheard", pid, &started) {
            Adopted::Ended(end) => {
                assert_eq!(end.exit, Some(3), "{end:?}");
                assert!(!end.timed_out && !end.hung);
            }
            other => panic!("expected the written end, got {other:?}"),
        }
        assert_eq!(read_from(&log_path(&dir, "unheard"), 0, 1024).unwrap(), ("bye\n".to_string(), 4));
    }

    #[test]
    fn a_running_job_is_readopted_by_pid_and_start_time() {
        let (host, dir) = (InProcess::default(), scratch("adopt"));
        let argv = sh("echo one; sleep 2; echo two");
        let job = start(&host, &dir, &spec("adopt", &argv, &dir, 30, Some(30))).unwrap();
        let Record { pid, started, .. } = job.record.clone();
        assert!(!started.is_empty(), "ps gave the start time");
        drop(job);
        // Another start time is another process: not this job.
        assert!(matches!(adopt(&host, &dir, "adopt", pid, "Mon_Jan_1_00:00:00_2001"), Adopted::Lost));
        let Adopted::Running(mut job) = adopt(&host, &dir, "adopt", pid, &started) else { panic!("not re-adopted") };
        assert_eq!(job.pgid(), pid);
        assert_eq!(job.record.hung_after_secs, Some(30), "the bounds come back from the record");
        let end = job.wait(&host);
        assert_eq!(end.exit, Some(0), "{end:?}");
        assert_eq!(fs::read_to_string(&job.log).unwrap(), "one\ntwo\n");
    }

    #[test]
    fn stop_ends_the_group_and_says_so() {
        let (host, dir) = (InProcess::default(), scratch("stop"));
        let argv = sh("sleep 60");
        let mut job = start(&host, &dir, &spec("stop", &argv, &dir, 60, None)).unwrap();
        let end = job.stop(&host);
        assert!(end.stopped && end.exit.is_none(), "{end:?}");
        assert!(within(5, || !host.is_alive(job.pgid())));
    }

    #[test]
    fn output_reads_from_an_offset_and_holds_back_a_cut_character() {
        let dir = scratch("output");
        let log = dir.join("x.log");
        fs::write(&log, "abc é").unwrap();
        assert_eq!(read_from(&log, 2, 100).unwrap(), ("c é".to_string(), 6));
        assert_eq!(read_from(&log, 0, 5).unwrap(), ("abc ".to_string(), 4), "the é's first byte waits");
        assert_eq!(read_from(&log, 6, 100).unwrap(), (String::new(), 6));
    }

    #[test]
    fn the_exit_file_reads_the_wrappers_and_the_runners_forms() {
        let e = parse_end("exit 2\n0m0.001s 0m0.002s\n0m1.250s 0m0.500s\n").unwrap();
        assert_eq!((e.exit, e.cpu_ms), (Some(2), 1750));
        let e = parse_end("exit 0\n0m0.000000s 0m0.000000s\n0m0.010000s 0m0.020000s\n").unwrap();
        assert_eq!(e.cpu_ms, 30, "dash's six places");
        let mine = End { timed_out: true, cpu_ms: 40, ..End::default() };
        assert_eq!(parse_end(&mine.text()), Some(mine));
        let sig = End { signal: Some(15), ..End::default() };
        assert_eq!(parse_end(&sig.text()), Some(sig));
        assert_eq!(parse_end("nothing"), None);
        assert_eq!(cpu_of("1-02:03:04.56"), (86_400 + 2 * 3600 + 3 * 60 + 4) * 100 + 56);
    }

    #[test]
    fn a_key_is_a_file_name_and_two_keys_never_share_one() {
        let k = |row: &str, job: &str| file_key(&JobKey { row: RowId(row.into()), attempt: 1, job: job.into() });
        assert_eq!(k("wsp-049", "check@17"), "wsp-049+1+check@17");
        assert_eq!(k("m3-agent-manager#1", "verify:clippy/x"), "m3-agent-manager%231+1+verify%3Aclippy%2Fx");
        assert_ne!(k("a+1", "b"), k("a", "1+b"));
    }

    #[test]
    fn the_end_makes_its_job_ended() {
        let key = JobKey { row: RowId("wsp-049".into()), attempt: 0, job: "check@1".into() };
        let end = End { exit: Some(0), cpu_ms: 12, ..End::default() };
        let ended = end.ended(key.clone(), HostId("mac".into()), Path::new("/x/jobs/k.log"));
        assert_eq!((ended.key, ended.exit, ended.timed_out, ended.hung, ended.cpu_ms), (key, Some(0), false, false, 12));
        assert_eq!(ended.log.path, "/x/jobs/k.log");
    }
}
