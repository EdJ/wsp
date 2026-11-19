//! Finding the server and saying `hello` (docs/state-core.md 4.1, D19): read
//! `api.json` and `api.token` under the state dir, dial the addresses in order
//! within one budget, send [`Hello`] and wait for [`Welcome`]. Anything else is
//! [`Silent`], which names what failed.
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
        }
    }
}

impl std::error::Error for Silent {}

/// A dialled, welcomed connection.
pub(crate) struct Open {
    pub(crate) conn: Tcp,
    pub(crate) welcome: Welcome,
}

/// Dial the server `state_dir` names and say hello as `by`, in `wait` at most.
pub(crate) fn open(state_dir: &Path, by: &Principal, wait: Duration) -> Result<Open, Silent> {
    let deadline = Instant::now() + wait;
    let listen = std::fs::read_to_string(state_dir.join(LISTEN_FILE)).map_err(|e| Silent::NoListenFile(e.to_string()))?;
    let listen: Listen = serde_json::from_str(&listen).map_err(|e| Silent::BadListenFile(e.to_string()))?;
    if listen.addrs.is_empty() {
        return Err(Silent::BadListenFile("it lists no address".into()));
    }
    let token = std::fs::read_to_string(state_dir.join(TOKEN_FILE)).map_err(|e| Silent::NoToken(e.to_string()))?;
    let token = ApiToken(token.trim().to_string());

    let mut tried = Vec::new();
    for (i, text) in listen.addrs.iter().enumerate() {
        // Each address gets an even share of what is left, so one that swallows
        // its packets does not take the whole budget.
        let share = deadline.saturating_duration_since(Instant::now()) / (listen.addrs.len() - i) as u32;
        let addr = match text.to_socket_addrs().ok().and_then(|mut a| a.next()) {
            Some(a) => a,
            None => {
                tried.push(format!("{text}: not an address"));
                continue;
            }
        };
        match Tcp::dial(addr, share.max(Duration::from_millis(1))) {
            Ok(conn) => return hello(conn, text, token, by, deadline),
            Err(e) => tried.push(format!("{text}: {e}")),
        }
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

/// One end of a TCP connection that carries frames.
pub(crate) struct Tcp {
    stream: TcpStream,
    /// Bytes read past the last whole line.
    partial: Vec<u8>,
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
    fn dial(addr: SocketAddr, timeout: Duration) -> Result<Tcp, ConnError> {
        let stream = TcpStream::connect_timeout(&addr, timeout).map_err(io)?;
        stream.set_nodelay(true).map_err(io)?;
        stream.set_write_timeout(Some(Duration::from_secs(10))).map_err(io)?;
        Ok(Tcp { stream, partial: Vec::new() })
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl Connection for Tcp {
    fn send(&mut self, frame: &Frame) -> Result<(), ConnError> {
        let mut line = serde_json::to_vec(frame).map_err(|e| ConnError::Malformed(e.to_string()))?;
        line.push(b'\n');
        self.stream.write_all(&line).map_err(io)
    }

    fn recv(&mut self, wait: Duration) -> Result<Option<Frame>, ConnError> {
        let deadline = Instant::now() + wait;
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(at) = self.partial.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.partial.drain(..=at).collect();
                return serde_json::from_slice(&line[..at]).map(Some).map_err(|e| ConnError::Malformed(e.to_string()));
            }
            if self.partial.len() > MAX_LINE {
                return Err(ConnError::Io(format!("a line longer than {MAX_LINE} bytes")));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            // A zero read timeout is refused by the OS; a millisecond is the floor.
            self.stream.set_read_timeout(Some(left.max(Duration::from_millis(1)))).map_err(io)?;
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(ConnError::Closed),
                Ok(n) => self.partial.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
                Err(e) => return Err(io(e)),
            }
        }
    }
}
