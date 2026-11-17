//! The TCP carrier of `wsp-host-api`'s [`Connection`] (`docs/agent-manager.md`
//! 3.4): newline-delimited JSON over one TCP stream, the framing every
//! carrier keeps (`crates/host-api/src/conn.rs`). The manager serves on it and
//! wire's client (`host::Tcp`, M1 group 4) dials with it.
//!
//! One [`Tcp`] reads; [`Tcp::try_clone`] hands a second handle to the thread
//! that writes. Only one handle of a stream should call `recv`, since each
//! keeps the bytes of a line it has not finished reading.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use wsp_host_api::{ConnError, Connection, Frame};

/// The longest line a peer may send. A peer that writes more without a
/// newline is broken or hostile, and the connection is failed rather than
/// grown without bound.
pub const MAX_LINE: usize = 16 * 1024 * 1024;

/// How long a write may block on a peer that reads nothing before the write
/// fails. A peer that has stopped reading for this long has missed its beats.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(wsp_host_api::BEAT_SECS * wsp_host_api::MISSED_BEATS as u64);

/// One end of a TCP connection that carries frames.
pub struct Tcp {
    stream: TcpStream,
    /// Bytes read past the last whole line.
    partial: Vec<u8>,
}

impl Tcp {
    /// Carry frames on `stream`.
    pub fn new(stream: TcpStream) -> std::io::Result<Tcp> {
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(Tcp { stream, partial: Vec::new() })
    }

    /// Dial `addr`, waiting up to `timeout` for it to accept.
    pub fn dial(addr: SocketAddr, timeout: Duration) -> Result<Tcp, ConnError> {
        let stream = TcpStream::connect_timeout(&addr, timeout).map_err(io)?;
        Tcp::new(stream).map_err(io)
    }

    /// A second handle on the same stream, for another thread to send on.
    pub fn try_clone(&self) -> std::io::Result<Tcp> {
        Ok(Tcp { stream: self.stream.try_clone()?, partial: Vec::new() })
    }

    /// The other end's address.
    pub fn peer(&self) -> Option<SocketAddr> {
        self.stream.peer_addr().ok()
    }

    /// Close both directions, which ends a `recv` blocked on another handle.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    /// Write `line` as it is, as a broken or hostile peer might.
    pub fn send_line(&mut self, line: &str) -> Result<(), ConnError> {
        self.stream.write_all(line.as_bytes()).map_err(io)
    }

    /// The next whole line already read, without its newline.
    fn take_line(&mut self) -> Option<Vec<u8>> {
        let at = self.partial.iter().position(|b| *b == b'\n')?;
        let mut line: Vec<u8> = self.partial.drain(..=at).collect();
        line.pop();
        Some(line)
    }
}

fn io(e: std::io::Error) -> ConnError {
    match e.kind() {
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::UnexpectedEof => {
            ConnError::Closed
        }
        _ => ConnError::Io(e.to_string()),
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
            if let Some(line) = self.take_line() {
                return serde_json::from_slice(&line).map(Some).map_err(|e| ConnError::Malformed(e.to_string()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use wsp_host_api::{At, Beat, Message};

    fn pair() -> (Tcp, Tcp) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = Tcp::dial(l.local_addr().unwrap(), Duration::from_secs(2)).unwrap();
        let (b, _) = l.accept().unwrap();
        (a, Tcp::new(b).unwrap())
    }

    fn beat(at: u64) -> Frame {
        Frame { id: None, req: None, msg: Message::Beat(Beat { at: At(at), host: None, seats: vec![] }) }
    }

    #[test]
    fn a_frame_crosses_as_one_line_and_a_quiet_wait_is_none() {
        let (mut a, mut b) = pair();
        assert_eq!(b.recv(Duration::from_millis(30)).unwrap(), None);
        a.send(&beat(1)).unwrap();
        a.send(&beat(2)).unwrap();
        assert_eq!(b.recv(Duration::from_secs(2)).unwrap(), Some(beat(1)));
        assert_eq!(b.recv(Duration::from_secs(2)).unwrap(), Some(beat(2)));
    }

    #[test]
    fn a_line_split_across_writes_is_one_frame() {
        let (mut a, mut b) = pair();
        let line = format!("{}\n", serde_json::to_string(&beat(7)).unwrap());
        let (head, tail) = line.split_at(5);
        a.send_line(head).unwrap();
        assert_eq!(b.recv(Duration::from_millis(30)).unwrap(), None);
        a.send_line(tail).unwrap();
        assert_eq!(b.recv(Duration::from_secs(2)).unwrap(), Some(beat(7)));
    }

    #[test]
    fn a_malformed_line_is_named_and_the_connection_stays_usable() {
        let (mut a, mut b) = pair();
        a.send_line("{not json\n").unwrap();
        a.send(&beat(3)).unwrap();
        assert!(matches!(b.recv(Duration::from_secs(2)), Err(ConnError::Malformed(_))));
        assert_eq!(b.recv(Duration::from_secs(2)).unwrap(), Some(beat(3)));
    }

    #[test]
    fn a_closed_peer_is_closed() {
        let (a, mut b) = pair();
        drop(a);
        assert_eq!(b.recv(Duration::from_secs(2)), Err(ConnError::Closed));
    }

    #[test]
    fn a_clone_sends_while_the_first_handle_reads() {
        let (mut a, b) = pair();
        let mut writer = b.try_clone().unwrap();
        writer.send(&beat(9)).unwrap();
        assert_eq!(a.recv(Duration::from_secs(2)).unwrap(), Some(beat(9)));
        b.shutdown();
        assert_eq!(a.recv(Duration::from_secs(2)), Err(ConnError::Closed));
    }
}
