//! Finding the server and saying `hello` (docs/state-core.md 4.1, D19): read
//! `api.json` and `api.token` under the state dir (or take the addresses and
//! token [`crate::Client::dial_at`] was given), dial the addresses in order
//! within one budget, send [`Hello`] and wait for [`Welcome`]. Anything else is
//! [`Silent`], which names what failed.
//!
//! A hub dialled over TLS (wsp-461) has the same lines inside a TLS session;
//! the handshake is part of the dial, and a certificate it refuses is
//! [`Silent::Tls`], naming the certificate's fault.
//!
//! [`Tcp`] is the dial-side twin of `wsp-server`'s carrier: newline JSON over a
//! stream, the framing rule of `crates/api/src/conn.rs`. It is copied so that a
//! process that only calls does not link the server.

use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, Instant};

use wsp_api::{
    ApiToken, ConnError, Connection, Frame, Hello, Listen, Refused, Welcome, LISTEN_FILE, MAX_LINE, PROTOCOL,
    TOKEN_FILE,
};
use wsp_store_contract::Principal;

use crate::tls::Tls;
use crate::{Dial, Where};

/// Why nothing answered. Never a panic or a hang: the CLI falls back and
/// web-chrome reconnects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Silent {
    /// `api.json` is missing or unreadable: no server has started here.
    NoListenFile(String),
    /// `api.json` is not a [`Listen`], or names no address.
    BadListenFile(String),
    /// `api.token` is missing or unreadable.
    NoToken(String),
    /// No address accepted a connection in time (each with why).
    Unreachable(Vec<String>),
    /// A connection was made, but no `welcome` came in time.
    NoWelcome(String),
    /// The server refused the hello (`unauthorised` for a wrong token).
    Refused(Refused),
    /// The server answered with something that is not a welcome.
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
            Silent::NoListenFile(why) => write!(f, "no {LISTEN_FILE}: {why}"),
            Silent::BadListenFile(why) => write!(f, "{LISTEN_FILE} is not usable: {why}"),
            Silent::NoToken(why) => write!(f, "no {TOKEN_FILE}: {why}"),
            Silent::Unreachable(tried) => write!(f, "nothing answered: {}", tried.join("; ")),
            Silent::NoWelcome(at) => write!(f, "{at} accepted the connection but sent no welcome in time"),
            Silent::Refused(r) => write!(f, "the server refused the hello: {:?}: {}", r.code, r.text),
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

/// Dial the server `target` names and say hello as its `by`, in its `wait` at most.
pub(crate) fn open(target: &Dial) -> Result<Open, Silent> {
    match &target.at {
        Where::StateDir(dir) => {
            let (addrs, token) = read_state_dir(dir)?;
            open_at(&addrs, token, &target.by, target.wait, None)
        }
        Where::Hub { addr, token, tls } => {
            open_at(std::slice::from_ref(addr), token.clone(), &target.by, target.wait, tls.as_ref())
        }
        Where::Addrs { addrs, token } => {
            if addrs.is_empty() {
                return Err(Silent::BadListenFile("no address was given".into()));
            }
            open_at(addrs, token.clone(), &target.by, target.wait, None)
        }
    }
}

/// The addresses `api.json` lists under `state_dir`, and its `api.token`.
fn read_state_dir(state_dir: &Path) -> Result<(Vec<String>, ApiToken), Silent> {
    let listen = std::fs::read_to_string(state_dir.join(LISTEN_FILE)).map_err(|e| Silent::NoListenFile(e.to_string()))?;
    let listen: Listen = serde_json::from_str(&listen).map_err(|e| Silent::BadListenFile(e.to_string()))?;
    if listen.addrs.is_empty() {
        return Err(Silent::BadListenFile("it lists no address".into()));
    }
    let token = std::fs::read_to_string(state_dir.join(TOKEN_FILE)).map_err(|e| Silent::NoToken(e.to_string()))?;
    Ok((listen.addrs, ApiToken(token.trim().to_string())))
}

/// Dial `addrs` in order and say hello with `token` as `by`, in `wait` at most,
/// inside TLS when `tls` is given. A name that resolves to several addresses
/// (`localhost` to both loopbacks) is tried at each.
fn open_at(addrs: &[String], token: ApiToken, by: &Principal, wait: Duration, tls: Option<&Tls>) -> Result<Open, Silent> {
    let deadline = Instant::now() + wait;
    let mut tried = Vec::new();
    let mut candidates: Vec<(&str, SocketAddr)> = Vec::new();
    for text in addrs {
        match text.to_socket_addrs() {
            Ok(found) if found.len() > 0 => candidates.extend(found.map(|a| (text.as_str(), a))),
            _ => tried.push(format!("{text}: not an address")),
        }
    }
    for (i, (text, addr)) in candidates.iter().enumerate() {
        // Each address gets an even share of what is left, so one that swallows
        // its packets does not take the whole budget.
        let share = deadline.saturating_duration_since(Instant::now()) / (candidates.len() - i) as u32;
        match Tcp::dial(*addr, share.max(Duration::from_millis(1)), tls, deadline) {
            Ok(conn) => return hello(conn, text, token, by, deadline),
            Err(Dialled::Tls(why)) => return Err(Silent::Tls(format!("{text}: {why}"))),
            Err(Dialled::Net(e)) => tried.push(format!("{text}: {e}")),
        }
    }
    Err(Silent::Unreachable(tried))
}

/// When the certificate the hub at `url` presents runs out, in epoch seconds:
/// `Ok(None)` for a hub dialled in the clear (`tcp://`, a bare `host:port`).
/// The handshake is the one a dial does, so a certificate the roots (or the
/// pinned `ca`) do not vouch for is [`Silent::Tls`] naming the fault.
pub fn hub_cert_expiry(url: &str, ca: Option<&Path>, wait: Duration) -> Result<Option<i64>, Silent> {
    let (addr, secure) = crate::credential::hub_addr(url)?;
    if !secure {
        return Ok(None);
    }
    let tls = Tls::new(&addr, ca)?;
    let deadline = Instant::now() + wait;
    let mut tried = Vec::new();
    let found = addr.to_socket_addrs().map_err(|e| Silent::Unreachable(vec![format!("{addr}: {e}")]))?;
    for a in found {
        let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
        let stream = match std::net::TcpStream::connect_timeout(&a, left) {
            Ok(s) => s,
            Err(e) => {
                tried.push(format!("{a}: {e}"));
                continue;
            }
        };
        return match handshake(stream, &tls, deadline) {
            Ok(s) => Ok(s
                .conn
                .peer_certificates()
                .and_then(|c| c.first())
                .and_then(|c| crate::tls::not_after(c.as_ref()))),
            Err(Dialled::Tls(why)) => Err(Silent::Tls(format!("{addr}: {why}"))),
            Err(Dialled::Net(e)) => Err(Silent::Unreachable(vec![format!("{addr}: {e}")])),
        };
    }
    Err(Silent::Unreachable(tried))
}

fn hello(mut conn: Tcp, at: &str, token: ApiToken, by: &Principal, deadline: Instant) -> Result<Open, Silent> {
    let frame = Frame::Hello(Hello { token, by: by.clone(), client: client_name(), protocol: PROTOCOL });
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
    /// only what it added, so a reply of n bytes costs n, not n²/8 KiB
    /// (wsp-692: a 16 MiB `state` spent 14 CPU-s rescanning its own head).
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
    let session = rustls::ClientConnection::new(tls.config.clone(), tls.name.clone())
        .map_err(|e| Dialled::Tls(e.to_string()))?;
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
    use std::net::TcpListener;
    use wsp_api::{Code, Failure, Outcome, Reply, ReqId};

    fn reply(id: u64, text: String) -> Frame {
        Frame::Reply(Reply {
            id: ReqId(id),
            outcome: Outcome::Err(Failure { code: Code::Stale, text, refusal: None }),
            events: Vec::new(),
        })
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
    /// ended (wsp-692) starts over at each line it hands back.
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
}
