//! The manager's tree jobs (`docs/agent-manager.md` 6.2 M3, wsp-050): the job
//! table beside the seat table, served over `wsp-host`'s runner
//! ([`wsp_host::jobs`], wsp-049).
//!
//! - **`job.run`, keyed by [`JobKey`].** A retry of a key gets the same
//!   journalled `job.started` (or, once it ended, its `job.ended`) and never a
//!   second run. A key asked again with another class, argv or place is
//!   refused `duplicate`, and so is a key that ran before and has left the
//!   table: its `<key>.run` is still on disk.
//! - **A hard cap per class**, [`Config::build`](crate::Config::build) and
//!   [`Config::check`](crate::Config::check), as `hello`'s capacity says. A run
//!   past it is refused `full`, never queued: when to ask is the throttle's
//!   to decide (d1 Q11).
//! - **Where it runs.** A `dir` under [`Config::tree_root`](crate::Config::tree_root)
//!   on this host; anything outside the root, absent, or on another host is
//!   refused `no_tree`. A row's `tree` is refused `no_tree` until Q6 places
//!   trees on hosts (T1 holds): wsp names the directory instead.
//! - **Events.** `job.started {pgid}` and `job.ended` are journalled before
//!   they are sent. `job.output` goes live to the connection that asked, and
//!   only there, and is not journalled: the log is the whole of it.
//! - **`job.stop`** ends the job's own group: `TERM`, then `KILL` after
//!   [`Config::job_grace`](crate::Config::job_grace). The manager signals no
//!   pgid that is not in this table, and `manager.log` names every signal it
//!   sends and the group the runner sweeps at every end (wsp-453).
//! - **Ownership.** A job that is not `detach`ed is stopped when the
//!   connection that asked for it closes. A retry of the key from another
//!   connection takes the job over: its output, and its ownership.
//! - **`jobs`** answers with the table.
//! - **Restart (2.5).** `<exec_dir>/jobs/<key>.run` holds what was asked,
//!   written before the job is started; the runner's `<key>.job` and
//!   `<key>.exit` hold the rest. At start each one is joined to the journal:
//!   an end already journalled is kept as it is; an end the exit file holds
//!   is reported `job.ended` now; a job still running is re-adopted by pid
//!   and start time and watched again, with no owner until its key is asked
//!   again; and one that is gone with no end is reported `job.ended` with no
//!   exit.
//! - **Pruning, by age.** An ended job whose `job.ended` the journal holds
//!   (its frame has an id) is kept, table entry and files, for
//!   [`Config::job_retention`](crate::Config::job_retention) after its end, so
//!   a retry inside it is answered the same `job.ended` and its log can still
//!   be read. Past it the job is forgotten: its entry, then its `<key>.log`,
//!   `<key>.exit`, `<key>.job` and last its `<key>.run`, so a stop part-way
//!   leaves a `.run` the next start sees again. The beat sweeps, and so do
//!   `job.run`, `jobs` and a start. A key asked again past the retention is a
//!   new run: nothing of the old one is left to refuse it by. The old run's
//!   `job.ended` stays in the journal, so a start joins a key to the end that
//!   follows its latest `job.started`, never an earlier run's.
//! - **A rerun's place in the journal (wsp-064).** The runner writes
//!   `<key>.job` before the manager can journal `job.started` (its pgid is
//!   known only once the job is spawned), so a manager killed between the two
//!   leaves a rerun whose key's last frame is the old run's `job.ended`. Of
//!   wsp-064's two ways, this takes the second: a start treats the run on disk
//!   as newer than any end before it, the order read from the journal, whose
//!   frames carry no time. The `.run` holds the journal's head when it was
//!   written (`after_event`), before the runner starts, and a start counts
//!   only the key's `job.started` and `job.ended` past that id. A `.run`
//!   without it, from before wsp-064, is joined as before.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use wsp_host::jobs::{self as runner, Adopted, End, Job, Spec};
use wsp_host::InProcess;
use wsp_host_api::{
    At, EventId, Frame, JobAt, JobClass, JobEnded, JobKey, JobOutput, JobRow, JobRun, JobStarted, JobState, JobStop, JobsList,
    Message, Refusal, Refused,
};

use crate::seats::Asker;
use crate::Shared;

/// Under `<exec_dir>/jobs`, beside the runner's files: what was asked.
const RUN_EXT: &str = "run";
/// The field of a `.run` beside the [`JobRun`]: the journal's head when it
/// was written.
const AFTER_FIELD: &str = "after_event";
/// How often a job's watcher looks at it and sends its output.
const TICK: Duration = Duration::from_millis(100);
/// The most of a log one `job.output` carries.
const CHUNK: usize = 64 * 1024;

/// One job, running or ended.
struct Entry {
    run: JobRun,
    pgid: u32,
    started: At,
    /// The journalled `job.started`, sent again to a retry.
    started_frame: Option<Frame>,
    /// The journalled `job.ended`, once it ended.
    ended: Option<Frame>,
    /// When it ended, which the retention counts from.
    ended_at: Option<SystemTime>,
    /// The connection that owns it: the last to ask for the key. `None` for a
    /// job re-adopted at start, until its key is asked again.
    owner: Option<Asker>,
    /// Asked to stop: why, and who waits on the `job.ended`.
    stopping: Option<(String, Vec<Asker>)>,
}

impl Entry {
    fn running(&self) -> bool {
        self.ended.is_none()
    }

    /// Ended, its `job.ended` journalled, and past `retention` at `now`.
    fn prunable(&self, now: SystemTime, retention: Duration) -> bool {
        let journalled = self.ended.as_ref().is_some_and(|f| f.id.is_some());
        let age = self.ended_at.and_then(|at| now.duration_since(at).ok()).unwrap_or_default();
        journalled && self.ended_at.is_some() && age >= retention
    }
}

#[derive(Default)]
pub(crate) struct Table {
    jobs: BTreeMap<JobKey, Entry>,
}

impl Table {
    fn running(&self, class: JobClass) -> u32 {
        self.jobs.values().filter(|e| e.running() && e.run.class == class).count() as u32
    }
}

fn table(shared: &Shared) -> MutexGuard<'_, Table> {
    shared.jobs.lock().unwrap_or_else(|e| e.into_inner())
}

fn exec_dir(shared: &Shared) -> &Path {
    &shared.cfg.exec_dir
}

fn run_path(shared: &Shared, file: &str) -> PathBuf {
    runner::jobs_dir(exec_dir(shared)).join(format!("{file}.{RUN_EXT}"))
}

/// A `.run`'s text: `r`, and the journal's head `after` it was written at.
fn asked_text(r: &JobRun, after: Option<EventId>) -> Vec<u8> {
    let mut v = serde_json::to_value(r).unwrap_or_default();
    if let (Some(obj), Some(id)) = (v.as_object_mut(), after) {
        obj.insert(AFTER_FIELD.into(), id.0.into());
    }
    serde_json::to_vec(&v).unwrap_or_default()
}

/// What a `.run` holds: the run asked, and the journal's head it was written
/// at (absent from one written before wsp-064).
fn read_asked(bytes: &[u8]) -> Option<(JobRun, Option<EventId>)> {
    let mut v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let after = v.as_object_mut()?.remove(AFTER_FIELD).and_then(|a| a.as_u64()).map(EventId);
    Some((serde_json::from_value(v).ok()?, after))
}

fn name(key: &JobKey) -> String {
    format!("{}/{}/{}", key.row.0, key.attempt, key.job)
}

fn refuse(asker: &Asker, why: Refusal, detail: impl Into<String>) {
    asker.send(Message::Refused(Refused { why, detail: detail.into() }));
}

/// Journal `msg` and send it to subscribers and to `to`: the first with the
/// frame's id through [`Shared::emit_for`], the rest the same frame. A
/// journal that cannot be written is logged, and `to` still answered.
fn emit(shared: &Shared, msg: Message, to: &[Asker]) -> Frame {
    match shared.emit_for(msg.clone(), to.first()) {
        Ok(id) => {
            let frame = Frame { id: Some(id), req: None, msg };
            for a in to.iter().skip(1) {
                a.send_frame(frame.clone());
            }
            frame
        }
        Err(e) => {
            shared.log.line(&format!("the journal could not be written, a job event not journalled: {e}"));
            let frame = Frame { id: None, req: None, msg };
            for a in to {
                a.send_frame(frame.clone());
            }
            frame
        }
    }
}

/// Whether `b` asks for the same run as `a`: a retry, not another job.
fn same_run(a: &JobRun, b: &JobRun) -> bool {
    a.class == b.class && a.argv == b.argv && a.at == b.at
}

/// The directory a job runs in, or why it may not.
fn place(shared: &Shared, at: &JobAt) -> Result<PathBuf, String> {
    let dir = match at {
        JobAt::Tree(t) => {
            return Err(format!(
                "row {}'s tree on {} is not placed by this manager until Q6 (T1 holds); name it as a dir under the tree root",
                t.row.0, t.host.0
            ))
        }
        JobAt::Dir(d) => d,
    };
    if dir.host != shared.cfg.host {
        return Err(format!("{} is another host's; this is {}", dir.host.0, shared.cfg.host.0));
    }
    if dir.escapes() {
        return Err(format!("{:?} falls outside the tree root", dir.path));
    }
    let root = &shared.cfg.tree_root;
    let root = root.canonicalize().map_err(|e| format!("the tree root {} cannot be read: {e}", root.display()))?;
    let path = root.join(&dir.path).canonicalize().map_err(|e| format!("{:?} under the tree root: {e}", dir.path))?;
    if !path.starts_with(&root) {
        return Err(format!("{:?} leads outside the tree root", dir.path));
    }
    if !path.is_dir() {
        return Err(format!("{:?} is not a directory", dir.path));
    }
    Ok(path)
}

fn cap(shared: &Shared, class: JobClass) -> u32 {
    match class {
        JobClass::Build => shared.cfg.build,
        JobClass::Check => shared.cfg.check,
    }
}

// ---------------------------------------------------------------- job.run

pub(crate) fn run(shared: &Arc<Shared>, asker: Asker, r: JobRun) {
    let mut t = table(shared);
    prune(shared, &mut t);
    if let Some(e) = t.jobs.get_mut(&r.key) {
        if !same_run(&e.run, &r) {
            return refuse(&asker, Refusal::Duplicate, format!("job {} was already run with other arguments", name(&r.key)));
        }
        if let Some(ended) = &e.ended {
            return asker.send_frame(ended.clone());
        }
        if let Some(started) = &e.started_frame {
            asker.send_frame(started.clone());
        }
        // The retry takes the job over: its output, and its ownership.
        e.owner = Some(asker);
        return;
    }
    let file = runner::file_key(&r.key);
    if run_path(shared, &file).exists() {
        return refuse(&asker, Refusal::Duplicate, format!("job {} already ran here, and has ended", name(&r.key)));
    }
    if r.argv.is_empty() {
        return refuse(&asker, Refusal::Malformed, "a job needs an argv");
    }
    let (running, most) = (t.running(r.class), cap(shared, r.class));
    if running >= most {
        return refuse(&asker, Refusal::Full, format!("{running} of {most} {:?} jobs run", r.class).to_lowercase());
    }
    let cwd = match place(shared, &r.at) {
        Ok(cwd) => cwd,
        Err(why) => return refuse(&asker, Refusal::NoTree, why),
    };
    // What was asked goes to disk before the job starts, so a restart knows
    // every job the runner's record names, with the journal's head: a frame
    // of this key at or before it is an earlier run's (wsp-064).
    let head = shared.journal.lock().unwrap_or_else(|e| e.into_inner()).head();
    let written = fs::create_dir_all(runner::jobs_dir(exec_dir(shared)))
        .and_then(|()| fs::write(run_path(shared, &file), asked_text(&r, head)));
    if let Err(e) = written {
        return refuse(&asker, Refusal::Failed, format!("the job's record could not be written: {e}"));
    }
    let host = InProcess::new();
    let spec = Spec {
        key: &file,
        argv: &r.argv,
        cwd: &cwd,
        timeout: Duration::from_secs(r.timeout_secs),
        hung_after: r.hung_after_secs.map(Duration::from_secs),
    };
    let job = match runner::start(&host, exec_dir(shared), &spec) {
        Ok(job) => job,
        Err(e) => {
            let _ = fs::remove_file(run_path(shared, &file));
            return refuse(&asker, Refusal::Failed, format!("the job could not be started: {e}"));
        }
    };
    let (key, pgid) = (r.key.clone(), job.pgid());
    shared.log.line(&format!("job {} started: {:?}, group {pgid}, in {}", name(&key), r.class, cwd.display()));
    // Journalled under the table, so a retry finds the frame it is sent.
    let frame = emit(shared, Message::JobStarted(JobStarted { key: key.clone(), pgid }), std::slice::from_ref(&asker));
    t.jobs.insert(
        key.clone(),
        Entry {
            run: r,
            pgid,
            started: crate::now(),
            started_frame: Some(frame),
            ended: None,
            ended_at: None,
            owner: Some(asker),
            stopping: None,
        },
    );
    drop(t);
    watch(shared.clone(), key, job);
}

/// A thread that watches `job` to its end: sends its output to its owner,
/// carries out a stop, and journals the `job.ended`. It leaves a job running
/// when the manager stops, for the next start to adopt.
fn watch(shared: Arc<Shared>, key: JobKey, mut job: Job) {
    std::thread::spawn(move || {
        let host = InProcess::new();
        let mut from = 0u64;
        let end = loop {
            if shared.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let (owner, stop) = {
                let t = table(&shared);
                let Some(e) = t.jobs.get(&key) else { return };
                (e.owner.clone(), e.stopping.as_ref().map(|(why, _)| why.clone()))
            };
            from = output(&job, &key, owner.as_ref(), from);
            if let Some(end) = job.poll(&host) {
                break end;
            }
            if let Some(why) = stop {
                break stop_group(&shared, &host, &key, &mut job, &why);
            }
            std::thread::sleep(TICK);
        };
        let owner = table(&shared).jobs.get(&key).and_then(|e| e.owner.clone());
        output(&job, &key, owner.as_ref(), from);
        ended(&shared, &key, &job.log, end);
    });
}

/// Send what the job wrote since `from` to `owner`; the offset it got to.
fn output(job: &Job, key: &JobKey, owner: Option<&Asker>, mut from: u64) -> u64 {
    let Some(owner) = owner else {
        return from;
    };
    loop {
        let Ok((text, next)) = job.output(from, CHUNK) else { return from };
        if next == from {
            return from;
        }
        owner.send(Message::JobOutput(JobOutput { key: key.clone(), from, text }));
        from = next;
    }
}

/// `job.stop`'s end: `TERM` to the job's own group, then `KILL` once the
/// grace is out. Both are named in the log, with the group.
fn stop_group(shared: &Shared, host: &InProcess, key: &JobKey, job: &mut Job, why: &str) -> End {
    let pgid = job.pgid();
    shared.log.line(&format!("job {}: TERM to its group {pgid} ({why})", name(key)));
    host.signal_group(pgid, "TERM");
    let until = Instant::now() + shared.cfg.job_grace;
    while Instant::now() < until {
        if let Some(end) = job.poll(host) {
            return End { stopped: true, ..end };
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    shared.log.line(&format!("job {}: KILL to its group {pgid}, {}s after TERM", name(key), shared.cfg.job_grace.as_secs()));
    job.stop(host)
}

/// Journal the job's end and send it to its owner and whoever asked it to
/// stop.
fn ended(shared: &Shared, key: &JobKey, log: &Path, end: End) {
    let mut t = table(shared);
    let Some(e) = t.jobs.get_mut(key) else { return };
    // Taken, so nothing keeps a closed connection's writer waiting.
    let mut to: Vec<Asker> = e.owner.take().into_iter().collect();
    if let Some((_, askers)) = e.stopping.take() {
        to.extend(askers);
    }
    let how = describe(&end);
    shared.log.line(&format!("job {} ended: {how}; its group {} killed (wsp-453)", name(key), e.pgid));
    let msg = Message::JobEnded(end.ended(key.clone(), shared.cfg.host.clone(), log));
    e.ended = Some(emit(shared, msg, &to));
    e.ended_at = Some(SystemTime::now());
}

fn describe(end: &End) -> String {
    let mut how = match (end.exit, end.signal) {
        (Some(n), _) => format!("exit {n}"),
        (None, Some(s)) => format!("signal {s}"),
        (None, None) => "no exit".to_string(),
    };
    for (flag, word) in [(end.timed_out, "timed out"), (end.hung, "hung"), (end.stopped, "stopped")] {
        if flag {
            how.push_str(", ");
            how.push_str(word);
        }
    }
    how.push_str(&format!(", {} ms CPU", end.cpu_ms));
    how
}

// ---------------------------------------------------------------- job.stop, jobs, a closed connection

pub(crate) fn stop(shared: &Shared, asker: Asker, s: JobStop) {
    let mut t = table(shared);
    let Some(e) = t.jobs.get_mut(&s.key) else {
        return refuse(&asker, Refusal::NoJob, format!("this manager runs no job {}", name(&s.key)));
    };
    if let Some(ended) = &e.ended {
        return asker.send_frame(ended.clone());
    }
    match &mut e.stopping {
        Some((_, askers)) => askers.push(asker),
        None => e.stopping = Some(("job.stop".into(), vec![asker])),
    }
}

pub(crate) fn list(shared: &Shared, asker: Asker) {
    let mut t = table(shared);
    prune(shared, &mut t);
    let jobs = t
        .jobs
        .iter()
        .map(|(key, e)| JobRow {
            key: key.clone(),
            class: e.run.class,
            pgid: e.pgid,
            started: e.started,
            state: if e.running() { JobState::Running } else { JobState::Ended },
        })
        .collect();
    asker.send(Message::JobsList(JobsList { jobs }));
}

/// Client `client` left: the jobs it owns that are not detached are stopped,
/// and no job's output goes to it any more.
pub(crate) fn closed(shared: &Shared, client: u64) {
    let mut t = table(shared);
    for (key, e) in t.jobs.iter_mut().filter(|(_, e)| e.running()) {
        if e.owner.as_ref().and_then(|o| o.client) != Some(client) {
            continue;
        }
        if e.run.detach {
            e.owner = None;
        } else if e.stopping.is_none() {
            shared.log.line(&format!("job {}: its connection closed, and it is not detached; stopping it", name(key)));
            e.owner = None;
            e.stopping = Some(("its connection closed".into(), vec![]));
        }
    }
}

// ---------------------------------------------------------------- pruning

/// The beat's sweep: [`prune`] under the table.
pub(crate) fn sweep(shared: &Shared) {
    prune(shared, &mut table(shared));
}

/// Forget every ended job past the retention (the module's pruning rule):
/// its table entry, then its files, the `.run` last.
fn prune(shared: &Shared, t: &mut Table) {
    let (now, retention) = (SystemTime::now(), shared.cfg.job_retention);
    let gone: Vec<JobKey> = t.jobs.iter().filter(|(_, e)| e.prunable(now, retention)).map(|(k, _)| k.clone()).collect();
    for key in gone {
        t.jobs.remove(&key);
        let (dir, file) = (exec_dir(shared), runner::file_key(&key));
        let paths = [
            runner::log_path(dir, &file),
            runner::exit_path(dir, &file),
            runner::record_path(dir, &file),
            run_path(shared, &file),
        ];
        let mut left = Vec::new();
        for path in paths {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => left.push(format!("{}: {e}", path.display())),
            }
        }
        let secs = retention.as_secs();
        if left.is_empty() {
            shared.log.line(&format!("job {} pruned: ended more than {secs}s ago, its files removed", name(&key)));
        } else {
            shared.log.line(&format!("job {} pruned past {secs}s, but not every file went: {}", name(&key), left.join("; ")));
        }
    }
}

/// When a job found at start ended: its exit file's time, else its record's,
/// else its `.run`'s, else now.
fn ended_at(shared: &Shared, file: &str) -> SystemTime {
    let dir = exec_dir(shared);
    [runner::exit_path(dir, file), runner::record_path(dir, file), run_path(shared, file)]
        .iter()
        .find_map(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
        .unwrap_or_else(SystemTime::now)
}

/// The jobs a running manager has, for tests: `(key, pgid, running)`.
pub(crate) fn rows(shared: &Shared) -> Vec<(JobKey, u32, bool)> {
    table(shared).jobs.iter().map(|(k, e)| (k.clone(), e.pgid, e.running())).collect()
}

// ---------------------------------------------------------------- restart

/// At start: every job a `<key>.run` names, joined to the journal and to the
/// runner's files. See the module's restart rule.
pub(crate) fn adopt(shared: &Arc<Shared>) {
    let dir = runner::jobs_dir(exec_dir(shared));
    let Ok(entries) = fs::read_dir(&dir) else { return };
    let mut asked = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some(RUN_EXT) {
            continue;
        }
        match fs::read(&path).ok().and_then(|b| read_asked(&b)) {
            Some(a) => asked.push(a),
            None => shared.log.line(&format!("{} could not be read; its job is not adopted", path.display())),
        }
    }
    if asked.is_empty() {
        return;
    }
    let (started, ended) = journalled(shared, asked.iter().map(|(r, _)| r.key.clone()).collect());
    let host = InProcess::new();
    let mut t = table(shared);
    for (r, after) in asked {
        let file = runner::file_key(&r.key);
        let key = r.key.clone();
        let Some(record) = runner::recorded(exec_dir(shared), &file) else {
            // The manager stopped between writing what was asked and starting it.
            shared.log.line(&format!("job {} was asked but never started; forgotten", name(&key)));
            let _ = fs::remove_file(run_path(shared, &file));
            continue;
        };
        let at = At(record.started_ms);
        let mut entry = Entry {
            run: r,
            pgid: record.pid,
            started: at,
            started_frame: this_run(started.get(&key), after),
            ended: this_run(ended.get(&key), after),
            ended_at: None,
            owner: None,
            stopping: None,
        };
        if entry.ended.is_some() {
            entry.ended_at = Some(ended_at(shared, &file));
            t.jobs.insert(key, entry);
            continue;
        }
        if entry.started_frame.is_none() {
            let msg = Message::JobStarted(JobStarted { key: key.clone(), pgid: record.pid });
            entry.started_frame = Some(emit(shared, msg, &[]));
        }
        let log = runner::log_path(exec_dir(shared), &file);
        match runner::adopt(&host, exec_dir(shared), &file, record.pid, &record.started) {
            Adopted::Running(job) => {
                shared.log.line(&format!("job {} re-adopted: group {}, running", name(&key), record.pid));
                t.jobs.insert(key.clone(), entry);
                watch(shared.clone(), key, job);
            }
            Adopted::Ended(end) => {
                shared.log.line(&format!("job {} ended while the manager was down: {}", name(&key), describe(&end)));
                let msg = Message::JobEnded(end.ended(key.clone(), shared.cfg.host.clone(), &log));
                entry.ended = Some(emit(shared, msg, &[]));
                entry.ended_at = Some(ended_at(shared, &file));
                t.jobs.insert(key, entry);
            }
            Adopted::Lost => {
                shared.log.line(&format!("job {} is gone with no end written (group {}); reported with no exit", name(&key), record.pid));
                let end = End::default();
                let msg = Message::JobEnded(end.ended(key.clone(), shared.cfg.host.clone(), &log));
                entry.ended = Some(emit(shared, msg, &[]));
                entry.ended_at = Some(SystemTime::now());
                t.jobs.insert(key, entry);
            }
        }
    }
    prune(shared, &mut t);
}

/// `frame`, if it is past `after`, the journal's head when this run's `.run`
/// was written: one at or before it is an earlier run's (wsp-064).
fn this_run(frame: Option<&Frame>, after: Option<EventId>) -> Option<Frame> {
    frame.filter(|f| after.is_none() || f.id > after).cloned()
}

/// The journalled `job.started` and `job.ended` of each of `keys`' latest
/// run. The journal is in order and never compacted, and a key pruned past
/// the retention may run again (wsp-062), so a `job.started` drops the key's
/// earlier `job.ended`: an end counts only after the start it ends.
fn journalled(shared: &Shared, keys: BTreeSet<JobKey>) -> (BTreeMap<JobKey, Frame>, BTreeMap<JobKey, Frame>) {
    let (mut started, mut ended) = (BTreeMap::new(), BTreeMap::new());
    let journal = shared.journal.lock().unwrap_or_else(|e| e.into_inner());
    let frames = match journal.after(None) {
        Ok((frames, _)) => frames,
        Err(e) => {
            shared.log.line(&format!("the journal could not be read for the jobs' events: {e}"));
            return (started, ended);
        }
    };
    for f in frames {
        match &f.msg {
            Message::JobStarted(JobStarted { key, .. }) if keys.contains(key) => {
                ended.remove(key);
                started.insert(key.clone(), f);
            }
            Message::JobEnded(JobEnded { key, .. }) if keys.contains(key) => {
                ended.insert(key.clone(), f);
            }
            _ => {}
        }
    }
    (started, ended)
}
