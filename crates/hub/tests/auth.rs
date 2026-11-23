//! Who is speaking to the hub (wsp-161), on a hub served in process and spoken
//! to in raw frames: a user's token is welcomed as that user whatever `by` the
//! hello sent; a revoked or unknown token is `unauthorised`; a request naming
//! another principal is `forbidden`; the bootstrap token makes the first admin
//! and nothing else.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wsp_api::{
    Answer, ApiToken, Call, Code, Frame, Hello, NoArgs, Outcome, ReqId, Request, SyncCall, Verb, WorkspaceId, PROTOCOL,
};
use wsp_hub::{serve, Config};
use wsp_machines::token;
use wsp_store_contract::{Command, IdempotencyKey, Principal, TokenId, UserName};

const WAIT: Duration = Duration::from_secs(5);

/// A hub served from a directory of its own, stopped and removed when dropped.
struct Served {
    dir: PathBuf,
    running: Option<serve::Running>,
}

impl Served {
    fn start(name: &str) -> Served {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("wsp-hub-auth-{name}-{}-{nanos}", std::process::id()));
        let cfg = Config {
            data_dir: dir.join("data"),
            bind: vec![],
            port: 0,
            peers: vec!["127.0.0.0/8".into()],
            tls: None,
            name: "hub-t".into(),
        };
        let running = serve::start(&cfg).expect("the hub starts");
        Served { dir, running: Some(running) }
    }

    fn running(&self) -> &serve::Running {
        self.running.as_ref().unwrap()
    }

    fn bootstrap(&self) -> String {
        std::fs::read_to_string(self.running().token_path()).unwrap().trim().to_string()
    }

    /// Applies an identity command in process, as the hub's host does.
    fn apply(&self, cmd: Command, by: &Principal) {
        let key = IdempotencyKey(format!("{cmd:?}"));
        self.running().hub().apply(cmd, key, by.clone()).expect("the identity machine takes it");
    }

    /// The first admin, `root`, and a token for it; and a user with a token.
    fn with_users(&self, users: &[&str]) -> Vec<String> {
        let root = person("root");
        self.apply(Command::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("hub-t".into()));
        let mut tokens = vec![self.issue("root", &root)];
        for user in users {
            self.apply(Command::HubUserAdd { name: UserName(user.to_string()), admin: false }, &root);
            tokens.push(self.issue(user, &root));
        }
        tokens
    }

    fn issue(&self, user: &str, by: &Principal) -> String {
        let (secret, hash) = token::mint();
        self.apply(Command::HubTokenIssue { id: TokenId(format!("t-{user}")), user: UserName(user.into()), hash }, by);
        secret
    }

    fn connect(&self, token: &str, by: Principal) -> Result<Wire, (Code, String)> {
        Wire::open(self.running().addrs()[0], token, by)
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        if let Some(r) = self.running.take() {
            r.stop();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn person(name: &str) -> Principal {
    Principal::Person(name.into())
}

/// A connection spoken to in frames, and who `welcome` said it is.
struct Wire {
    out: TcpStream,
    lines: BufReader<TcpStream>,
    by: Principal,
    next: u64,
}

impl Wire {
    fn open(addr: std::net::SocketAddr, token: &str, by: Principal) -> Result<Wire, (Code, String)> {
        let stream = TcpStream::connect_timeout(&addr, WAIT).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let mut w = Wire { out: stream.try_clone().unwrap(), lines: BufReader::new(stream), by: by.clone(), next: 1 };
        w.send(&Frame::Hello(Hello { token: ApiToken(token.into()), by, client: "auth test".into(), protocol: PROTOCOL }));
        match w.recv() {
            Frame::Welcome(welcome) => {
                w.by = welcome.by.expect("welcome says who the connection is");
                Ok(w)
            }
            Frame::Refused(r) => Err((r.code, r.text)),
            other => panic!("a hello is answered welcome or refused, not {other:?}"),
        }
    }

    fn send(&mut self, frame: &Frame) {
        let mut line = serde_json::to_vec(frame).unwrap();
        line.push(b'\n');
        self.out.write_all(&line).unwrap();
    }

    fn recv(&mut self) -> Frame {
        let mut line = String::new();
        self.lines.read_line(&mut line).expect("a frame within the wait");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"))
    }

    /// `call` as `by`, and its outcome.
    fn ask_as(&mut self, by: &Principal, call: Call) -> Outcome {
        let id = ReqId(self.next);
        self.next += 1;
        let key = matches!(call, Call::Command(_)).then(|| IdempotencyKey(format!("auth-test-{}", id.0)));
        self.send(&Frame::Request(Request { v: PROTOCOL, id, key, by: by.clone(), call }));
        loop {
            match self.recv() {
                Frame::Reply(r) if r.id == id => return r.outcome,
                _ => continue,
            }
        }
    }

    /// `call` as the principal `welcome` named.
    fn ask(&mut self, call: Call) -> Outcome {
        let by = self.by.clone();
        self.ask_as(&by, call)
    }
}

fn code(outcome: &Outcome) -> Option<Code> {
    match outcome {
        Outcome::Ok(_) => None,
        Outcome::Err(f) => Some(f.code),
    }
}

fn heads() -> Call {
    Call::Sync(SyncCall::Heads { workspace: WorkspaceId("shared:mp".into()) })
}

fn state() -> Call {
    Call::Verb(Verb::State(NoArgs {}))
}

fn add(name: &str, admin: bool) -> Call {
    Call::Command(Command::HubUserAdd { name: UserName(name.into()), admin })
}

#[test]
fn an_active_tokens_hello_is_welcomed_as_its_user_whatever_by_it_sent() {
    let hub = Served::start("welcome");
    let tokens = hub.with_users(&["ann"]);
    for sent in [person("mallory"), person("root"), Principal::Wsp("hub-t".into()), person("ann")] {
        let mut ann = hub.connect(&tokens[1], sent.clone()).expect("ann's token is welcomed");
        assert_eq!(ann.by, person("ann"), "a hello that said {sent:?} is ann's");
        assert!(matches!(ann.ask(heads()), Outcome::Ok(Answer::Heads(_))), "and is answered as ann");
    }
    let root = hub.connect(&tokens[0], person("ann")).unwrap();
    assert_eq!(root.by, person("root"));
}

#[test]
fn a_request_naming_another_principal_than_the_tokens_is_forbidden() {
    let hub = Served::start("forbidden");
    let tokens = hub.with_users(&["ann"]);
    let mut ann = hub.connect(&tokens[1], person("root")).unwrap();
    for (by, call) in [(person("root"), heads()), (person("root"), add("eve", true)), (person("bo"), state())] {
        let outcome = ann.ask_as(&by, call);
        assert_eq!(code(&outcome), Some(Code::Forbidden), "ann's connection asking as {by:?}: {outcome:?}");
    }
    assert!(hub.running().hub().state().unwrap().users.get(&UserName("eve".into())).is_none(), "nothing was added");
    assert!(matches!(ann.ask(state()), Outcome::Ok(Answer::State { .. })), "as ann it is answered");
    // The identity machine decides, under the token's principal: ann is no admin.
    assert_eq!(code(&ann.ask(add("eve", false))), Some(Code::Forbidden));
}

#[test]
fn a_revoked_or_unknown_token_is_unauthorised() {
    let hub = Served::start("revoked");
    let tokens = hub.with_users(&["ann", "bo"]);
    let (root, ann, bo) = (&tokens[0], &tokens[1], &tokens[2]);
    let (unknown, _) = token::mint();
    for bad in [unknown.as_str(), "", "not-a-token", &token::hash(ann)] {
        let refused = hub.connect(bad, person("ann")).err();
        assert_eq!(refused.map(|r| r.0), Some(Code::Unauthorised), "{bad:?}");
    }
    assert!(hub.connect(ann, person("ann")).is_ok() && hub.connect(bo, person("bo")).is_ok());

    // A token revoked by an admin over the wire; a user revoked, and every token of theirs with them.
    let mut admin = hub.connect(root, person("root")).unwrap();
    let revoke = Call::Command(Command::HubTokenRevoke { id: TokenId("t-ann".into()) });
    assert!(matches!(admin.ask(revoke), Outcome::Ok(Answer::Applied)));
    let refused = hub.connect(ann, person("ann")).err().expect("ann's revoked token is refused");
    assert_eq!(refused.0, Code::Unauthorised);
    assert!(!refused.1.contains("revoked token record"), "it says no more than an unknown token: {}", refused.1);
    let gone = Call::Command(Command::HubUserRevoke { name: UserName("bo".into()) });
    assert!(matches!(admin.ask(gone), Outcome::Ok(Answer::Applied)));
    assert_eq!(hub.connect(bo, person("bo")).err().map(|r| r.0), Some(Code::Unauthorised));
}

#[test]
fn the_bootstrap_token_adds_the_first_admin_and_nothing_else() {
    let hub = Served::start("bootstrap");
    let bootstrap = hub.bootstrap();
    let mut me = hub.connect(&bootstrap, person("root")).expect("the bootstrap token, while no admin exists");
    assert_eq!(me.by, Principal::Wsp("hub-t".into()), "it names the hub itself, whatever by it sent");
    for (what, call) in [
        ("a read", state()),
        ("a sync verb", heads()),
        ("a user who is no admin", add("ann", false)),
        ("a token", Call::Command(Command::HubTokenIssue { id: TokenId("t".into()), user: UserName("x".into()), hash: "0".repeat(64) })),
        ("a workspace", Call::Command(Command::HubWorkspaceAdd { name: wsp_store_contract::SharedName("mp".into()) })),
    ] {
        assert_eq!(code(&me.ask(call)), Some(Code::Forbidden), "{what}");
    }
    let me_by = me.by.clone();
    assert_eq!(code(&me.ask_as(&person("root"), add("root", true))), Some(Code::Forbidden), "only as itself");
    assert_eq!(me.by, me_by);

    assert!(matches!(me.ask(add("root", true)), Outcome::Ok(Answer::Applied)), "the first admin");
    let state = hub.running().hub().state().unwrap();
    assert!(state.users[&UserName("root".into())].admin);
    assert_eq!(code(&me.ask(add("second", true))), Some(Code::Forbidden), "and no second one");
    assert_eq!(state.users.len(), 1);

    // Once an admin exists the bootstrap token names nobody.
    assert_eq!(hub.connect(&bootstrap, Principal::Wsp("hub-t".into())).err().map(|r| r.0), Some(Code::Unauthorised));

    // The admin, given a token by the host, runs the identity machine over the wire.
    let root = hub.issue("root", &person("root"));
    let mut admin = hub.connect(&root, person("root")).unwrap();
    assert!(matches!(admin.ask(add("ann", false)), Outcome::Ok(Answer::Applied)));
    let (ann, hash) = token::mint();
    let issue = Call::Command(Command::HubTokenIssue { id: TokenId("t-ann".into()), user: UserName("ann".into()), hash });
    assert!(matches!(admin.ask(issue), Outcome::Ok(Answer::Applied)));
    assert_eq!(hub.connect(&ann, person("anyone")).unwrap().by, person("ann"));
}
