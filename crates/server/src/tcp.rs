//! The TCP carrier of `wsp-api`'s [`Connection`] (docs/state-core.md 4.1, D19):
//! newline-delimited JSON over one TCP stream, with the framing rule every
//! carrier keeps (`crates/api/src/conn.rs`). The server serves on it, and it is
//! `pub` so that `wsp-client`'s tests can speak to a server with it.
//!
//! One [`Tcp`] reads; [`Tcp::try_clone`] hands a second handle to the thread
//! that writes. Only one handle of a stream should call `recv`, since each
//! keeps the bytes of a line it has not finished reading.
//!
//! This is `wsp-host`'s carrier copied, not shared: that one is typed to the
//! manager's `Frame`, and this crate may not link `wsp-host` (section 6).

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use wsp_api::{ConnError, Connection, Frame, BEAT_SECS, MAX_LINE, MISSED_BEATS};

/// How long a write may block on a peer that reads nothing before it fails:
/// a peer that has stopped reading for this long has missed its beats.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(BEAT_SECS * MISSED_BEATS as u64);

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

    /// The stream itself, cloned: for a table that closes it from elsewhere.
    pub fn stream_clone(&self) -> std::io::Result<TcpStream> {
        self.stream.try_clone()
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
        self.write_bytes(line.as_bytes())
    }

    /// A frame already serialised, newline ended, written as it is.
    pub(crate) fn write_line(&mut self, line: &[u8]) -> Result<(), ConnError> {
        self.write_bytes(line)
    }

    /// All of `bytes`, on the socket.
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), ConnError> {
        self.stream.write_all(bytes).map_err(io)
    }

    /// Read what the socket has within `left`, into `partial`. A quiet socket is `Ok`.
    fn fill(&mut self, left: Duration) -> Result<(), ConnError> {
        let mut chunk = [0u8; 8192];
        // A zero read timeout is refused by the OS; a millisecond is the floor.
        self.stream.set_read_timeout(Some(left.max(Duration::from_millis(1)))).map_err(io)?;
        match self.stream.read(&mut chunk) {
            Ok(0) => Err(ConnError::Closed),
            Ok(n) => {
                self.partial.extend_from_slice(&chunk[..n]);
                Ok(())
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => Ok(()),
            Err(e) => Err(io(e)),
        }
    }

    /// The next whole line, without its newline, undecoded; `None` after a
    /// quiet `wait`. The server reads lines so that a malformed one can still
    /// be answered by its `id`.
    pub fn recv_line(&mut self, wait: Duration) -> Result<Option<Vec<u8>>, ConnError> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(line) = self.take_line() {
                return Ok(Some(line));
            }
            if self.partial.len() > MAX_LINE {
                return Err(ConnError::Io(format!("a line longer than {MAX_LINE} bytes")));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.fill(left)?;
        }
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
        ErrorKind::BrokenPipe
        | ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::UnexpectedEof
        | ErrorKind::NotConnected => ConnError::Closed,
        _ => ConnError::Io(e.to_string()),
    }
}

impl Connection for Tcp {
    fn send(&mut self, frame: &Frame) -> Result<(), ConnError> {
        let mut line = serde_json::to_vec(frame).map_err(|e| ConnError::Malformed(e.to_string()))?;
        line.push(b'\n');
        self.write_bytes(&line)
    }

    fn recv(&mut self, wait: Duration) -> Result<Option<Frame>, ConnError> {
        match self.recv_line(wait)? {
            Some(line) => serde_json::from_slice(&line).map(Some).map_err(|e| ConnError::Malformed(e.to_string())),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use wsp_api::{EventId, Feed};
    use wsp_store_contract::At;

    fn pair() -> (Tcp, Tcp) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = Tcp::dial(l.local_addr().unwrap(), Duration::from_secs(2)).unwrap();
        let (b, _) = l.accept().unwrap();
        (a, Tcp::new(b).unwrap())
    }

    fn beat(n: u64) -> Frame {
        Frame::Feed(Feed::Beat { beat: EventId(n), at: At(n) })
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
    fn a_line_split_across_writes_is_one_frame_and_a_bad_line_keeps_the_connection() {
        let (mut a, mut b) = pair();
        let line = format!("{}\n", serde_json::to_string(&beat(7)).unwrap());
        let (head, tail) = line.split_at(5);
        a.send_line(head).unwrap();
        assert_eq!(b.recv(Duration::from_millis(30)).unwrap(), None);
        a.send_line(tail).unwrap();
        assert_eq!(b.recv(Duration::from_secs(2)).unwrap(), Some(beat(7)));
        a.send_line("not json\n").unwrap();
        assert!(matches!(b.recv(Duration::from_secs(2)), Err(ConnError::Malformed(_))));
        a.send(&beat(8)).unwrap();
        assert_eq!(b.recv(Duration::from_secs(2)).unwrap(), Some(beat(8)));
        drop(a);
        assert_eq!(b.recv(Duration::from_secs(2)), Err(ConnError::Closed));
    }
}
