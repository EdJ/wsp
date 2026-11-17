//! The manager's tree jobs over TCP on loopback, with `sh` jobs (wsp-050's
//! done-when): two `job.run`s under one key run once; `job.stop` ends the
//! job's group and nothing else; a run past capacity is refused `full`; a
//! dropped connection stops its job unless the job is detached; a manager
//! killed with SIGKILL mid-job and restarted reports each job's end once and
//! resumes events `from` the last id; and `jobs` lists the job.
//!
//! Every job is a `sh` script under the test's scratch directory, and every
//! test kills the groups it started however it ends ([`Sweep`]).

use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use wsp_host::Tcp;
use wsp_host_api::{
    Capacity, ConnError, Connection, EventId, Frame, Hello, HostDir, HostId, JobAt, JobClass, JobKey, JobRun, JobState,
    JobStop, Jobs, ManagerHello, Message, Refusal, ReqId, Role, RowId, Subscribe, Token, WspHello, PROTOCOL,
};
use wsp_manager::{Config, Manager, TOKEN_FILE};

const WAIT: Duration = Duration::from_secs(10);
const HOST: &str = "test-host";

/// An exec dir and a tree root with a `work` directory in it.
fn place(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::var_os("WSP_SCRATCH").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let top = base.join(format!("wmj-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&top);
    let (dir, root) = (top.join("m"), top.join("trees"));
    fs::create_dir_all(root.join("work")).unwrap();
    fs::create_dir_all(top.join("inproc")).unwrap();
    (dir, root)
}

/// Ends what a test started, however it ends: every job group the runner
/// recorded, a manager run as its own process, and an outsider.
struct Sweep {
    dir: PathBuf,
    manager: Option<Child>,
    others: Vec<Child>,
}

impl Sweep {
    fn new(dir: &Path) -> Sweep {
        Sweep { dir: dir.to_path_buf(), manager: None, others: Vec::new() }
    }
}

impl Drop for Sweep {
    fn drop(&mut self) {
        if let Some(mut m) = self.manager.take() {
            let _ = m.kill();
            let _ = m.wait();
        }
        for pgid in groups(&self.dir) {
            let _ = Command::new("kill").args(["-KILL", "--", &format!("-{pgid}")]).stderr(Stdio::null()).status();
        }
        for mut c in self.others.drain(..) {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// The groups the runner recorded under `<dir>/jobs`.
fn groups(dir: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir.join("jobs")).into_iter().flatten().flatten() {
        if e.path().extension().and_then(|x| x.to_str()) == Some("job") {
            let text = fs::read_to_string(e.path()).unwrap_or_default();
            if let Some(pid) = text.lines().find_map(|l| l.strip_prefix("pid ")?.trim().parse().ok()) {
                out.push(pid);
            }
        }
    }
    out
}

fn config(dir: &Path, root: &Path) -> Config {
    let mut cfg = Config::new(dir, HOST);
    cfg.port = 0;
    cfg.tree_root = root.to_path_buf();
    cfg.job_grace = Duration::from_secs(1);
    cfg.inproc_runs = vec![];
    cfg
}

fn token(dir: &Path) -> Token {
    Token(fs::read_to_string(dir.join(TOKEN_FILE)).unwrap().trim().to_string())
}

/// A connection that numbers its requests and keeps every event and every
/// `job.output` it hears.
struct Client {
    conn: Tcp,
    next: u64,
    events: Vec<Frame>,
    output: String,
}

impl Client {
    fn new(addr: SocketAddr, dir: &Path) -> (Client, ManagerHello) {
        let mut conn = Tcp::dial(addr, WAIT).unwrap();
        let hello =
            Hello::Wsp(WspHello { role: Role::Cli, protocol: PROTOCOL, version: "test".into(), last_event: None, token: token(dir) });
        conn.send(&Frame { id: None, req: Some(ReqId(1)), msg: Message::Hello(hello) }).unwrap();
        loop {
            match conn.recv(WAIT).unwrap() {
                Some(Frame { msg: Message::Beat(_), .. }) => continue,
                Some(Frame { req: Some(ReqId(1)), msg: Message::Hello(Hello::Manager(m)), .. }) => {
                    return (Client { conn, next: 10, events: Vec::new(), output: String::new() }, m)
                }
                other => panic!("expected the manager's hello, got {other:?}"),
            }
        }
    }

    fn recv(&mut self, wait: Duration) -> Option<Frame> {
        let until = Instant::now() + wait;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            match self.conn.recv(left) {
                Ok(Some(Frame { msg: Message::Beat(_), .. })) => continue,
                Ok(Some(f)) => {
                    if f.id.is_some() {
                        self.events.push(f.clone());
                    }
                    if let Message::JobOutput(o) = &f.msg {
                        self.output.push_str(&o.text);
                    }
                    return Some(f);
                }
                Ok(None) => return None,
                Err(ConnError::Closed) => panic!("the manager closed the connection"),
                Err(e) => panic!("{e}"),
            }
        }
    }

    fn send(&mut self, msg: Message) -> ReqId {
        self.next += 1;
        let req = ReqId(self.next);
        self.conn.send(&Frame { id: None, req: Some(req), msg }).unwrap();
        req
    }

    /// The next frame answering `req` that is not `job.output`.
    fn answer_frame(&mut self, req: ReqId) -> Frame {
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            if let Some(f) = self.recv(until.saturating_duration_since(Instant::now())) {
                if f.req == Some(req) && !matches!(f.msg, Message::JobOutput(_)) {
                    return f;
                }
            }
        }
        panic!("no answer to {req:?} within {WAIT:?}");
    }

    fn ask(&mut self, msg: Message) -> Message {
        let req = self.send(msg);
        self.answer_frame(req).msg
    }

    fn subscribe(&mut self, from: Option<EventId>) {
        self.conn.send(&Frame { id: None, req: None, msg: Message::Subscribe(Subscribe { from }) }).unwrap();
    }

    fn hear_until(&mut self, wait: Duration, done: impl Fn(&Client) -> bool) {
        let until = Instant::now() + wait;
        while !done(self) && Instant::now() < until {
            self.recv(Duration::from_millis(100));
        }
    }

    fn ended(&self, job: &str) -> usize {
        self.events.iter().filter(|f| matches!(&f.msg, Message::JobEnded(e) if e.key.job == job)).count()
    }

    fn jobs(&mut self) -> Vec<(String, u32, JobState)> {
        match self.ask(Message::Jobs(Jobs {})) {
            Message::JobsList(l) => l.jobs.into_iter().map(|r| (r.key.job, r.pgid, r.state)).collect(),
            other => panic!("expected jobs.list, got {other:?}"),
        }
    }
}

fn key(job: &str) -> JobKey {
    JobKey { row: RowId("wsp-050".into()), attempt: 0, job: job.into() }
}

fn run_msg(job: &str, class: JobClass, script: &str, detach: bool) -> Message {
    Message::JobRun(JobRun {
        key: key(job),
        class,
        argv: vec!["sh".into(), "-c".into(), script.into()],
        at: JobAt::Dir(HostDir { host: HostId(HOST.into()), path: "work".into() }),
        timeout_secs: 60,
        hung_after_secs: None,
        detach,
    })
}

fn stop_msg(job: &str) -> Message {
    Message::JobStop(JobStop { key: key(job) })
}

fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn within(wait: Duration, mut f: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + wait;
    while Instant::now() < until {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    f()
}

fn started(msg: Message) -> u32 {
    match msg {
        Message::JobStarted(s) => s.pgid,
        other => panic!("expected job.started, got {other:?}"),
    }
}

#[test]
fn two_runs_under_one_key_run_once_and_jobs_lists_it() {
    let (dir, root) = place("once");
    let _sweep = Sweep::new(&dir);
    let m = Manager::start(config(&dir, &root)).expect("the manager starts");
    let (mut c, hello) = Client::new(m.addrs()[0], &dir);
    assert_eq!(hello.capacity, Capacity { seats: wsp_manager::DEFAULT_SEATS, build: 2, check: 4 });

    let script = "echo ran >> count; sleep 1; echo done";
    let first = c.send(run_msg("check@1", JobClass::Check, script, false));
    let first = c.answer_frame(first);
    // The retry after a lost reply, on this connection and on another.
    let again = c.send(run_msg("check@1", JobClass::Check, script, false));
    let again = c.answer_frame(again);
    assert_eq!((&again.msg, again.id), (&first.msg, first.id), "the same journalled job.started");
    assert!(first.id.is_some(), "job.started is journalled");
    let pgid = started(first.msg);

    let rows = c.jobs();
    assert_eq!(rows, vec![("check@1".to_string(), pgid, JobState::Running)], "jobs lists it running");

    let (mut other, _) = Client::new(m.addrs()[0], &dir);
    assert_eq!(started(other.ask(run_msg("check@1", JobClass::Check, script, false))), pgid);
    // It took the job over, so its end goes there.
    other.hear_until(WAIT, |c| c.ended("check@1") == 1);
    assert_eq!(other.ended("check@1"), 1, "{:?}", other.events);
    assert!(other.output.contains("done"), "job.output followed its new owner: {:?}", other.output);
    assert_eq!(fs::read_to_string(root.join("work/count")).unwrap(), "ran\n", "it ran once");

    // Asked after its end: the same job.ended, still run once.
    let Message::JobEnded(e) = c.ask(run_msg("check@1", JobClass::Check, script, false)) else { panic!("job.ended") };
    assert_eq!((e.exit, e.timed_out, e.hung), (Some(0), false, false));
    assert_eq!(fs::read_to_string(&e.log.path).unwrap(), "done\n");
    assert_eq!(fs::read_to_string(root.join("work/count")).unwrap(), "ran\n");

    match c.ask(run_msg("check@1", JobClass::Check, "echo other", false)) {
        Message::Refused(r) => assert_eq!(r.why, Refusal::Duplicate, "{}", r.detail),
        other => panic!("another argv under the key is refused, got {other:?}"),
    }
    assert_eq!(c.jobs(), vec![("check@1".to_string(), pgid, JobState::Ended)], "jobs lists it ended");
    m.stop();
}

#[test]
fn stop_ends_the_jobs_group_and_nothing_else() {
    let (dir, root) = place("stop");
    let mut sweep = Sweep::new(&dir);
    let outsider = Command::new("sleep").arg("60").process_group(0).spawn().unwrap();
    let outsider_pid = outsider.id();
    sweep.others.push(outsider);
    let m = Manager::start(config(&dir, &root)).expect("the manager starts");
    let (mut c, _) = Client::new(m.addrs()[0], &dir);

    let script = "sleep 60 & echo $! > bg.pid; echo up; sleep 60";
    let pgid = started(c.ask(run_msg("build@1", JobClass::Build, script, false)));
    c.hear_until(WAIT, |c| c.output.contains("up"));
    assert!(c.output.contains("up"), "job.output came live to the asker");
    let bg: u32 = fs::read_to_string(root.join("work/bg.pid")).unwrap().trim().parse().unwrap();
    assert!(alive(bg) && alive(pgid));

    match c.ask(stop_msg("build@1")) {
        Message::JobEnded(e) => assert_eq!(e.exit, None, "{e:?}"),
        other => panic!("job.stop is answered with job.ended, got {other:?}"),
    }
    assert!(within(WAIT, || !alive(pgid) && !alive(bg)), "the job's group, its background child included, is gone");
    assert!(alive(outsider_pid), "a process outside the job survives its stop");

    // Every signal the manager names is on a group in its job table.
    let table: Vec<u32> = m.jobs().into_iter().map(|(_, g, _)| g).collect();
    assert_eq!(table, vec![pgid]);
    let log = fs::read_to_string(m.log_path()).unwrap();
    assert!(log.contains(&format!("TERM to its group {pgid}")), "{log}");
    for line in log.lines().filter(|l| l.contains("TERM") || l.contains("KILL") || l.contains("killed")) {
        let named: u32 = line.split("group ").nth(1).and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next()).and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("a kill that names no group: {line}"));
        assert!(table.contains(&named), "a kill on a group not in the table: {line}");
    }
    match c.ask(stop_msg("nothing@1")) {
        Message::Refused(r) => assert_eq!(r.why, Refusal::NoJob),
        other => panic!("{other:?}"),
    }
    m.stop();
}

#[test]
fn a_run_past_capacity_is_refused_full() {
    let (dir, root) = place("full");
    let _sweep = Sweep::new(&dir);
    let mut cfg = config(&dir, &root);
    (cfg.build, cfg.check) = (1, 1);
    let m = Manager::start(cfg).expect("the manager starts");
    let (mut c, hello) = Client::new(m.addrs()[0], &dir);
    assert_eq!((hello.capacity.build, hello.capacity.check), (1, 1));

    started(c.ask(run_msg("check@a", JobClass::Check, "sleep 30", false)));
    match c.ask(run_msg("check@b", JobClass::Check, "sleep 30", false)) {
        Message::Refused(r) => assert_eq!(r.why, Refusal::Full, "{}", r.detail),
        other => panic!("a second check past a cap of 1 is refused full, got {other:?}"),
    }
    started(c.ask(run_msg("build@a", JobClass::Build, "sleep 30", false)));
    assert_eq!(c.jobs().len(), 2, "the refused run is not queued");

    assert!(matches!(c.ask(stop_msg("check@a")), Message::JobEnded(_)));
    started(c.ask(run_msg("check@c", JobClass::Check, "sleep 30", false)));
    assert!(matches!(c.ask(stop_msg("check@c")), Message::JobEnded(_)));
    assert!(matches!(c.ask(stop_msg("build@a")), Message::JobEnded(_)));
    m.stop();
}

#[test]
fn a_dropped_connection_stops_its_job_unless_detached() {
    let (dir, root) = place("drop");
    let _sweep = Sweep::new(&dir);
    let m = Manager::start(config(&dir, &root)).expect("the manager starts");
    let (mut watcher, _) = Client::new(m.addrs()[0], &dir);
    watcher.subscribe(None);

    let (mut c, _) = Client::new(m.addrs()[0], &dir);
    let held = started(c.ask(run_msg("build@held", JobClass::Build, "sleep 30", false)));
    let free = started(c.ask(run_msg("build@free", JobClass::Build, "sleep 30", true)));
    c.conn.shutdown();
    drop(c);

    watcher.hear_until(WAIT, |w| w.ended("build@held") == 1);
    assert_eq!(watcher.ended("build@held"), 1, "the undetached job ended with its connection");
    assert!(within(WAIT, || !alive(held)));
    watcher.hear_until(Duration::from_millis(500), |_| false);
    assert_eq!(watcher.ended("build@free"), 0, "the detached job runs on");
    assert!(alive(free));
    let rows = watcher.jobs();
    assert!(rows.contains(&("build@held".into(), held, JobState::Ended)), "{rows:?}");
    assert!(rows.contains(&("build@free".into(), free, JobState::Running)), "{rows:?}");
    assert!(fs::read_to_string(m.log_path()).unwrap().contains("its connection closed"));

    assert!(matches!(watcher.ask(stop_msg("build@free")), Message::JobEnded(_)));
    assert!(within(WAIT, || !alive(free)));
    m.stop();
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The manager as its own process, so it can be killed with SIGKILL.
fn run_manager(dir: &Path, root: &Path, port: u16) -> Child {
    Command::new(env!("CARGO_BIN_EXE_wsp-manager"))
        .arg("--exec-dir")
        .arg(dir)
        .args(["--host", HOST, "--port", &port.to_string(), "--no-tailnet", "--compound-sup", "/usr/bin/false"])
        .arg("--inproc-run")
        .arg(dir.parent().unwrap().join("inproc"))
        .arg("--tree-root")
        .arg(root)
        .args(["--build", "3", "--check", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("wsp-manager runs")
}

/// Dial until the manager answers.
fn reach(port: u16, dir: &Path) -> (Client, ManagerHello) {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let until = Instant::now() + WAIT;
    loop {
        if dir.join(TOKEN_FILE).exists() && Tcp::dial(addr, Duration::from_millis(200)).is_ok() {
            return Client::new(addr, dir);
        }
        assert!(Instant::now() < until, "the manager did not answer on {addr}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_manager_killed_mid_job_reports_its_end_once_and_resumes_from_the_last_id() {
    let (dir, root) = place("kill");
    let mut sweep = Sweep::new(&dir);
    let port = free_port();
    sweep.manager = Some(run_manager(&dir, &root, port));
    let (mut c, hello) = reach(port, &dir);
    assert_eq!((hello.capacity.build, hello.capacity.check), (3, 1), "the flags set the capacity");
    c.subscribe(None);
    // One job ends while the manager is down; the other outlives the restart.
    let short = started(c.ask(run_msg("build@short", JobClass::Build, "sleep 1; echo short", false)));
    let long = started(c.ask(run_msg("build@long", JobClass::Build, "sleep 4; echo long", false)));
    c.hear_until(Duration::from_millis(300), |_| false);
    let last = c.events.last().and_then(|f| f.id).expect("job.started was heard as an event");
    assert_eq!(c.ended("build@short") + c.ended("build@long"), 0, "killed before either ended");

    let mut m = sweep.manager.take().unwrap();
    m.kill().unwrap(); // SIGKILL
    m.wait().unwrap();
    assert!(alive(long), "the job outlives its manager");
    assert!(within(WAIT, || dir.join("jobs/wsp-050+0+build@short.exit").exists()), "the short job ended while no manager ran");
    assert!(!alive(short));
    assert!(alive(long), "the long one is still running at the restart");

    sweep.manager = Some(run_manager(&dir, &root, port));
    let (mut back, _) = reach(port, &dir);
    back.subscribe(Some(last));
    back.hear_until(WAIT, |b| b.ended("build@short") >= 1 && b.ended("build@long") >= 1);
    back.hear_until(Duration::from_millis(700), |_| false);
    assert_eq!((back.ended("build@short"), back.ended("build@long")), (1, 1), "each end is reported once: {:?}", back.events);
    for f in &back.events {
        if let Message::JobEnded(e) = &f.msg {
            assert_eq!(e.exit, Some(0), "{e:?}");
        }
    }
    assert!(back.events.iter().all(|f| f.id > Some(last)), "only events after {last:?}");
    assert_eq!(back.events.first().map(|f| f.id), Some(Some(EventId(last.0 + 1))), "ids run on from the last");
    assert!(!back.events.iter().any(|f| matches!(f.msg, Message::JobStarted(_))), "no job is started again");

    // The table is in key order.
    let rows = back.jobs();
    assert_eq!(rows, vec![("build@long".into(), long, JobState::Ended), ("build@short".into(), short, JobState::Ended)]);
    // Asked again after the restart: the same end, and no second run.
    assert!(matches!(back.ask(run_msg("build@short", JobClass::Build, "sleep 1; echo short", false)), Message::JobEnded(_)));
}

/// The files a job keeps under `<dir>/jobs`, by their extension.
fn job_files(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(dir.join("jobs"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.path().extension().and_then(|x| x.to_str()).map(str::to_string))
        .collect();
    out.sort();
    out
}

#[test]
fn an_ended_job_is_kept_for_a_retry_inside_the_retention_and_pruned_past_it() {
    let (dir, root) = place("prune");
    let _sweep = Sweep::new(&dir);
    let mut cfg = config(&dir, &root);
    cfg.job_retention = Duration::from_secs(3);
    cfg.beat = Duration::from_millis(200);
    let m = Manager::start(cfg).expect("the manager starts");
    let (mut c, _) = Client::new(m.addrs()[0], &dir);

    let script = "echo ran >> count; echo done";
    started(c.ask(run_msg("check@p", JobClass::Check, script, false)));
    c.hear_until(WAIT, |c| c.ended("check@p") == 1);
    let first = c.events.iter().find(|f| matches!(&f.msg, Message::JobEnded(_))).cloned().expect("job.ended heard");
    assert!(first.id.is_some(), "job.ended is journalled");
    assert_eq!(job_files(&dir), ["exit", "job", "log", "run"], "an ended job keeps its files inside the retention");

    // A retry inside the retention: the same journalled job.ended, not a second run.
    let again = c.send(run_msg("check@p", JobClass::Check, script, false));
    let again = c.answer_frame(again);
    assert_eq!((&again.msg, again.id), (&first.msg, first.id), "the same job.ended inside the retention");
    let Message::JobEnded(e) = &again.msg else { unreachable!() };
    assert_eq!(fs::read_to_string(&e.log.path).unwrap(), "done\n", "its log is still there to read");
    assert_eq!(fs::read_to_string(root.join("work/count")).unwrap(), "ran\n");

    // Past it, the beat prunes the job, its files and its row, with no client
    // asking (one left idle under a 200 ms beat would be dropped for silence).
    drop(c);
    assert!(within(WAIT, || job_files(&dir).is_empty()), "pruned past the retention: {:?}", job_files(&dir));
    assert!(m.jobs().is_empty(), "the table forgets it too: {:?}", m.jobs());
    let (mut c, _) = Client::new(m.addrs()[0], &dir);
    assert!(c.jobs().is_empty());
    let log = fs::read_to_string(dir.join("log/manager.log")).unwrap_or_default();
    assert!(log.contains("job wsp-050/0/check@p pruned"), "the log names the prune: {log}");
    m.stop();
}
