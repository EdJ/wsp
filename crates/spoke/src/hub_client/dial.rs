//! Dialling the hub and saying `hello`: dial its address within one budget,
//! inside TLS for a `tls://` URL (the handshake is part of the dial, and a
//! certificate it refuses is [`Silent::Tls`], naming the certificate's fault),
//! send [`Hello`] and wait for [`Welcome`]. Anything else is [`Silent`], which
//! names what failed. [`login_github`] dials the same way and sends [`Login`]
//! in place of the hello.
//!
//! [`Tcp`] is the carrier: newline JSON over a stream, the framing rule of
//! `wsp_hub_protocol::conn`.

use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, Instant};

use wsp_hub_protocol::{
    ApiToken, ConnError, Connection, Frame, Hello, Login, LoginDone, LoginPrompt, Machine, Principal, Refused, Welcome,
    LOGIN_GITHUB, MAX_LINE, PROTOCOL,
};

use super::tls::Tls;
use super::Dial;

/// Why nothing answered. Never a panic or a hang.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Silent {
    /// The hub's URL is not one a spoke dials: not `host:port`, or a scheme
    /// other than `tls://` and `tcp://`.
    BadUrl(String),
    /// No address accepted a connection in time (each with why).
    Unreachable(Vec<String>),
    /// A connection was made, but no `welcome` came in time.
    NoWelcome(String),
    /// The hub refused the hello (`unauthorised` for a wrong token), or the login.
    Refused(Refused),
    /// The hub answered with something that is not a welcome.
    NotAServer(String),
    /// No user token for a hub: `$WSP_HUB_TOKEN` is unset and `hubs.json` names none.
    NotLoggedIn(String),
    /// `hubs.json` is unusable: wider than 0600, unreadable or not a login.
    BadHubsFile(String),
    /// TLS failed: the hub's certificate was refused (the text names why, as
    /// `invalid peer certificate: UnknownIssuer`), or the CA file did not read.
    Tls(String),
}

impl fmt::Display for Silent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Silent::BadUrl(why) => write!(f, "{why}"),
            Silent::Unreachable(tried) => write!(f, "nothing answered: {}", tried.join("; ")),
            Silent::NoWelcome(at) => write!(f, "{at} accepted the connection but sent no welcome in time"),
            Silent::Refused(r) => write!(f, "the server refused the hello: {}", super::words::refused(r)),
            Silent::NotAServer(what) => write!(f, "not a wsp server: {what}"),
            Silent::NotLoggedIn(why) => write!(f, "not logged in: {why}"),
            Silent::BadHubsFile(why) => write!(f, "hubs.json is not usable: {why}"),
            Silent::Tls(why) => write!(f, "tls: {why}"),
        }
    }
}

impl std::error::Error for Silent {}

/// A dialled, welcomed connection.
pub(crate) struct Open {
    pub(crate) conn: Tcp,
    pub(crate) welcome: Welcome,
}

/// Dial the hub `target` names and say hello as its `by`, in its `wait` at most.
pub(crate) fn open(target: &Dial) -> Result<Open, Silent> {
    let deadline = Instant::now() + target.wait;
    let (conn, at) = connect(&target.addr, target.tls.as_ref(), deadline)?;
    hello(conn, &at, target.token.clone(), &target.by, target.machine.clone(), deadline)
}

/// Dial `addr` within `deadline`, inside TLS when `tls` is given. A name that
/// resolves to several addresses (`localhost` to both loopbacks) is tried at
/// each, each with an even share of what is left. The connection, and the
/// address text it was made to.
fn connect(addr: &str, tls: Option<&Tls>, deadline: Instant) -> Result<(Tcp, String), Silent> {
    let mut tried = Vec::new();
    let candidates: Vec<SocketAddr> = match addr.to_socket_addrs() {
        Ok(found) => found.collect(),
        Err(_) => Vec::new(),
    };
    if candidates.is_empty() {
        tried.push(format!("{addr}: not an address"));
    }
    for (i, a) in candidates.iter().enumerate() {
        // Each address gets an even share of what is left, so one that swallows
        // its packets does not take the whole budget.
        let share = deadline.saturating_duration_since(Instant::now()) / (candidates.len() - i) as u32;
        match Tcp::dial(*a, share.max(Duration::from_millis(1)), tls, deadline) {
            Ok(conn) => return Ok((conn, addr.to_string())),
            Err(Dialled::Tls(why)) => return Err(Silent::Tls(format!("{addr}: {why}"))),
            Err(Dialled::Net(e)) => tried.push(format!("{addr}: {e}")),
        }
    }
    Err(Silent::Unreachable(tried))
}

/// When the certificate the hub at `url` presents runs out, in epoch seconds:
/// `Ok(None)` for a hub dialled in the clear (`tcp://`, a bare `host:port`).
/// The handshake is the one a dial does, so a certificate the roots (or the
/// pinned `ca`) do not vouch for is [`Silent::Tls`] naming the fault.
pub fn hub_cert_expiry(url: &str, ca: Option<&Path>, wait: Duration) -> Result<Option<i64>, Silent> {
    let (addr, secure) = super::login::hub_addr(url)?;
    if !secure {
        return Ok(None);
    }
    let tls = Tls::new(&addr, ca)?;
    let deadline = Instant::now() + wait;
    let mut tried = Vec::new();
    let found = addr.to_socket_addrs().map_err(|e| Silent::Unreachable(vec![format!("{addr}: {e}")]))?;
    for a in found {
        let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
        let stream = match TcpStream::connect_timeout(&a, left) {
            Ok(s) => s,
            Err(e) => {
                tried.push(format!("{a}: {e}"));
                continue;
            }
        };
        return match handshake(stream, &tls, deadline) {
            Ok(s) => Ok(s.conn.peer_certificates().and_then(|c| c.first()).and_then(|c| super::tls::not_after(c.as_ref()))),
            Err(Dialled::Tls(why)) => Err(Silent::Tls(format!("{addr}: {why}"))),
            Err(Dialled::Net(e)) => Err(Silent::Unreachable(vec![format!("{addr}: {e}")])),
        };
    }
    Err(Silent::Unreachable(tried))
}

/// Log in to the hub at `url` with GitHub (the hub runs GitHub's device flow):
/// `prompt` is handed the address to open and the code to enter, and the call
/// then waits until the person has entered it, for as long as the code lasts.
/// The answer is the hub user and its new token, which the caller keeps as it
/// keeps any hub token. `wait` bounds the dial and the hub's first answer.
/// No token is sent: the connection carries the login and nothing else.
pub fn login_github(
    url: &str,
    ca: Option<&Path>,
    wait: Duration,
    prompt: impl FnMut(&LoginPrompt),
) -> Result<LoginDone, Silent> {
    let (addr, secure) = super::login::hub_addr(url)?;
    let tls = if secure { Some(Tls::new(&addr, ca)?) } else { None };
    let (mut conn, at) = connect(&addr, tls.as_ref(), Instant::now() + wait)?;
    let done = login_github_on(&mut conn, &at, wait, prompt);
    conn.shutdown();
    done
}

/// [`login_github`] on a connection already made to the hub at `at`: send
/// `login { method: "github" }`, hand each `login_prompt` to `prompt` and wait
/// as long as its code lasts (and half a minute), until `login_done` or
/// `refused`. Nothing is sent while the hub waits on GitHub: the hub gives the
/// login up if the caller speaks.
pub fn login_github_on(
    conn: &mut dyn Connection,
    at: &str,
    wait: Duration,
    mut prompt: impl FnMut(&LoginPrompt),
) -> Result<LoginDone, Silent> {
    let login = Login { method: LOGIN_GITHUB.into(), client: client_name(), protocol: PROTOCOL };
    conn.send(&Frame::Login(login)).map_err(|e| Silent::NoWelcome(format!("{at}: {e}")))?;
    let mut left = wait;
    loop {
        match conn.recv(left) {
            Ok(Some(Frame::LoginPrompt(p))) => {
                prompt(&p);
                // The hub answers once the code is entered or has run out.
                left = Duration::from_secs(p.expires_secs.saturating_add(30));
            }
            Ok(Some(Frame::LoginDone(done))) => return Ok(done),
            Ok(Some(Frame::Refused(r))) => return Err(Silent::Refused(r)),
            Ok(Some(other)) => return Err(Silent::NotAServer(format!("{at} answered the login with {other:?}"))),
            Ok(None) => return Err(Silent::NoWelcome(at.to_string())),
            Err(e) => return Err(Silent::NoWelcome(format!("{at}: {e}"))),
        }
    }
}

fn hello(mut conn: Tcp, at: &str, token: ApiToken, by: &Principal, machine: Option<Machine>, deadline: Instant) -> Result<Open, Silent> {
    let frame = Frame::Hello(Hello { token, by: by.clone(), client: client_name(), protocol: PROTOCOL, machine });
    conn.send(&frame).map_err(|e| Silent::NoWelcome(format!("{at}: {e}")))?;
    let left = deadline.saturating_duration_since(Instant::now());
    match conn.recv(left) {
        Ok(Some(Frame::Welcome(welcome))) => Ok(Open { conn, welcome }),
        Ok(Some(Frame::Refused(r))) => Err(Silent::Refused(r)),
        Ok(Some(other)) => Err(Silent::NotAServer(format!("{at} answered the hello with {other:?}"))),
        Ok(None) => Err(Silent::NoWelcome(at.to_string())),
        Err(e) => Err(Silent::NoWelcome(format!("{at}: {e}"))),
    }
}

/// What the hello and the login say is calling: the name hubs have logged
/// since this code was `wsp-client`'s, kept so the bytes do not change.
fn client_name() -> String {
    format!("wsp-client {}", env!("CARGO_PKG_VERSION"))
}

/// Why a dial gave no connection.
enum Dialled {
    /// The connect failed or timed out.
    Net(ConnError),
    /// The TLS handshake failed; the text names why.
    Tls(String),
}

/// One end of a TCP connection that carries frames.
pub(crate) struct Tcp {
    wire: Wire,
    /// Bytes read past the last whole line.
    partial: Vec<u8>,
    /// How much of `partial` is known to hold no newline: each read searches
    /// only what it added, so a reply of n bytes costs n, not n²/8 KiB.
    scanned: usize,
}

/// The stream, plain or inside a TLS session this end owns whole (a client
/// reads and writes from one handle, so nothing of it is shared).
enum Wire {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Wire {
    fn socket(&self) -> &TcpStream {
        match self {
            Wire::Plain(s) => s,
            Wire::Tls(s) => &s.sock,
        }
    }
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Wire::Plain(s) => s.read(buf),
            Wire::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Wire::Plain(s) => s.write(buf),
            Wire::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Wire::Plain(s) => s.flush(),
            Wire::Tls(s) => s.flush(),
        }
    }
}

fn io(e: std::io::Error) -> ConnError {
    match e.kind() {
        ErrorKind::BrokenPipe
        | ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::UnexpectedEof
        | ErrorKind::NotConnected => ConnError::Closed,
        _ => ConnError::Io(e.to_string()),
    }
}

impl Tcp {
    /// Connect to `addr` within `timeout`; with `tls`, also finish the handshake
    /// before `deadline`.
    fn dial(addr: SocketAddr, timeout: Duration, tls: Option<&Tls>, deadline: Instant) -> Result<Tcp, Dialled> {
        let stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| Dialled::Net(io(e)))?;
        stream.set_nodelay(true).map_err(|e| Dialled::Net(io(e)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10))).map_err(|e| Dialled::Net(io(e)))?;
        let wire = match tls {
            None => Wire::Plain(stream),
            Some(tls) => Wire::Tls(Box::new(handshake(stream, tls, deadline)?)),
        };
        Ok(Tcp { wire, partial: Vec::new(), scanned: 0 })
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.wire.socket().shutdown(Shutdown::Both);
    }
}

/// The TLS handshake over `stream`, within `deadline`: a certificate the
/// client's roots do not vouch for ends it, and the error says which fault.
fn handshake(
    stream: TcpStream,
    tls: &Tls,
    deadline: Instant,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, Dialled> {
    let session = rustls::ClientConnection::new(tls.config.clone(), tls.name.clone()).map_err(|e| Dialled::Tls(e.to_string()))?;
    let mut s = rustls::StreamOwned::new(session, stream);
    while s.conn.is_handshaking() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Dialled::Tls("the handshake did not finish in time".into()));
        }
        s.sock.set_read_timeout(Some(left)).map_err(|e| Dialled::Net(io(e)))?;
        match s.conn.complete_io(&mut s.sock) {
            Ok(_) => {}
            // A refused certificate arrives as an `InvalidData` error that holds rustls's own.
            Err(e) if e.kind() == ErrorKind::InvalidData => return Err(Dialled::Tls(e.to_string())),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(Dialled::Tls("the handshake did not finish in time".into()))
            }
            Err(e) if io(e.kind().into()) == ConnError::Closed => {
                return Err(Dialled::Tls(format!("the hub closed the connection during the handshake: {e}")))
            }
            Err(e) => return Err(Dialled::Net(io(e))),
        }
    }
    Ok(s)
}

impl Connection for Tcp {
    fn send(&mut self, frame: &Frame) -> Result<(), ConnError> {
        let mut line = serde_json::to_vec(frame).map_err(|e| ConnError::Malformed(e.to_string()))?;
        line.push(b'\n');
        self.wire.write_all(&line).map_err(io)
    }

    fn recv(&mut self, wait: Duration) -> Result<Option<Frame>, ConnError> {
        let deadline = Instant::now() + wait;
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(at) = self.partial[self.scanned..].iter().position(|b| *b == b'\n').map(|i| self.scanned + i) {
                self.scanned = 0;
                let line: Vec<u8> = self.partial.drain(..=at).collect();
                return serde_json::from_slice(&line[..at]).map(Some).map_err(|e| ConnError::Malformed(e.to_string()));
            }
            self.scanned = self.partial.len();
            if self.partial.len() > MAX_LINE {
                return Err(ConnError::Io(format!("a line longer than {MAX_LINE} bytes")));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            // A zero read timeout is refused by the OS; a millisecond is the floor.
            self.wire.socket().set_read_timeout(Some(left.max(Duration::from_millis(1)))).map_err(io)?;
            match self.wire.read(&mut chunk) {
                Ok(0) => return Err(ConnError::Closed),
                Ok(n) => self.partial.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
                Err(e) => return Err(io(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::net::TcpListener;
    use wsp_hub_protocol::{Code, Failure, Outcome, Reply, ReqId};

    fn reply(id: u64, text: String) -> Frame {
        Frame::Reply(Reply { id: ReqId(id), outcome: Outcome::Err(Failure { code: Code::Stale, text, refusal: None }), events: Vec::new() })
    }

    fn line(f: &Frame) -> Vec<u8> {
        let mut l = serde_json::to_vec(f).unwrap();
        l.push(b'\n');
        l
    }

    fn text(f: Frame) -> (u64, String) {
        match f {
            Frame::Reply(Reply { id, outcome: Outcome::Err(e), .. }) => (id.0, e.text),
            other => panic!("not the reply sent: {other:?}"),
        }
    }

    /// A frame that arrives in many reads, then two that arrive in one, come
    /// out whole and in order: the search that resumes where the last read
    /// ended starts over at each line it hands back.
    #[test]
    fn a_line_read_in_pieces_and_two_in_one_read_come_out_whole() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let long = "x".repeat(300_000);
        let (a, b, c) = (reply(1, long.clone()), reply(2, "two".into()), reply(3, "three".into()));
        let (first, rest) = (line(&a), [line(&b), line(&c)].concat());
        let writer = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            for piece in first.chunks(1000) {
                s.write_all(piece).unwrap();
                s.flush().unwrap();
            }
            s.write_all(&rest).unwrap();
        });
        let mut conn = Tcp::dial(addr, Duration::from_secs(5), None, Instant::now() + Duration::from_secs(5)).ok().unwrap();
        let wait = Duration::from_secs(10);
        assert_eq!(text(conn.recv(wait).unwrap().unwrap()), (1, long));
        assert_eq!(text(conn.recv(wait).unwrap().unwrap()), (2, "two".into()));
        assert_eq!(text(conn.recv(wait).unwrap().unwrap()), (3, "three".into()));
        writer.join().unwrap();
    }

    /// A hub's side of a login, scripted: what it answers, in order, and the
    /// waits it was asked to answer within; every frame the client sent.
    struct Scripted {
        answers: VecDeque<Result<Option<Frame>, ConnError>>,
        sent: Vec<Frame>,
        waits: Vec<Duration>,
    }

    impl Scripted {
        fn new(answers: Vec<Result<Option<Frame>, ConnError>>) -> Scripted {
            Scripted { answers: answers.into(), sent: Vec::new(), waits: Vec::new() }
        }
    }

    impl Connection for Scripted {
        fn send(&mut self, frame: &Frame) -> Result<(), ConnError> {
            self.sent.push(frame.clone());
            Ok(())
        }

        fn recv(&mut self, wait: Duration) -> Result<Option<Frame>, ConnError> {
            self.waits.push(wait);
            self.answers.pop_front().unwrap_or(Err(ConnError::Closed))
        }
    }

    fn prompt() -> LoginPrompt {
        LoginPrompt { url: "https://github.com/login/device".into(), code: "ABCD-1234".into(), expires_secs: 900 }
    }

    fn done() -> LoginDone {
        LoginDone { user: "ann".into(), token: ApiToken("s3cret-token".into()) }
    }

    /// The login sends one `login { method: "github" }` and nothing else, hands
    /// the prompt over, waits as long as the code lasts, and returns the user
    /// and token the hub sent.
    #[test]
    fn a_github_login_sends_one_login_frame_shows_the_prompt_and_returns_the_hubs_user_and_token() {
        let mut conn = Scripted::new(vec![Ok(Some(Frame::LoginPrompt(prompt()))), Ok(Some(Frame::LoginDone(done())))]);
        let mut shown = Vec::new();
        let got = login_github_on(&mut conn, "hub.invalid:7489", Duration::from_secs(10), |p| shown.push(p.clone())).unwrap();
        assert_eq!(got, done());
        assert_eq!(shown, vec![prompt()]);
        assert_eq!(conn.sent.len(), 1, "nothing but the login is sent: {:?}", conn.sent);
        assert_eq!(
            serde_json::to_string(&conn.sent[0]).unwrap(),
            format!(r#"{{"type":"login","method":"github","client":"wsp-client {}","protocol":1}}"#, env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(conn.waits, vec![Duration::from_secs(10), Duration::from_secs(930)], "the first answer within `wait`, then the code's life");
    }

    #[test]
    fn a_refused_github_login_is_the_hubs_refusal() {
        let refused = Refused { code: Code::Forbidden, text: "mallory is not on this hub's list of GitHub users".into() };
        let mut conn = Scripted::new(vec![Ok(Some(Frame::LoginPrompt(prompt()))), Ok(Some(Frame::Refused(refused.clone())))]);
        assert_eq!(login_github_on(&mut conn, "hub", Duration::from_secs(1), |_| {}), Err(Silent::Refused(refused)));
        // A hub with no GitHub login refuses before any prompt.
        let unsupported = Refused { code: Code::Unsupported, text: "no github".into() };
        let mut conn = Scripted::new(vec![Ok(Some(Frame::Refused(unsupported.clone())))]);
        let mut prompted = false;
        assert_eq!(login_github_on(&mut conn, "hub", Duration::from_secs(1), |_| prompted = true), Err(Silent::Refused(unsupported)));
        assert!(!prompted);
    }

    #[test]
    fn a_github_login_that_hears_nothing_or_another_frame_or_a_closed_connection_fails_by_name() {
        let mut quiet = Scripted::new(vec![Ok(None)]);
        assert_eq!(login_github_on(&mut quiet, "hub", Duration::from_millis(1), |_| {}), Err(Silent::NoWelcome("hub".into())));
        let welcome = Welcome { server: "old-hub".into(), protocol: PROTOCOL, head: wsp_hub_protocol::EventId(0), by: None, machine: None, admin: None };
        let mut old = Scripted::new(vec![Ok(Some(Frame::Welcome(welcome)))]);
        assert!(matches!(login_github_on(&mut old, "hub", Duration::from_secs(1), |_| {}), Err(Silent::NotAServer(_))));
        let mut gone = Scripted::new(vec![Ok(Some(Frame::LoginPrompt(prompt()))), Err(ConnError::Closed)]);
        assert!(matches!(login_github_on(&mut gone, "hub", Duration::from_secs(1), |_| {}), Err(Silent::NoWelcome(why)) if why.contains("closed")));
    }

    /// What a login says of itself never holds the token.
    #[test]
    fn a_github_login_answer_does_not_print_its_token() {
        assert!(!format!("{:?}", done()).contains("s3cret-token"));
        assert!(!format!("{:?}", Ok::<_, Silent>(done())).contains("s3cret-token"));
    }
}
