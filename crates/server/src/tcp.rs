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
//!
//! **TLS** (wsp-461). [`Tcp::accept_tls`] carries the same lines inside a TLS
//! session the server's side of which is one `rustls::ServerConnection` that the
//! handles of a stream share behind a mutex: the reading handle reads the
//! socket *without* the lock (it may wait a beat for bytes) and takes it only
//! to hand them to the session and to drain what they decrypt, so a writer is
//! never held up by a read. The handshake is no separate step: the first
//! `recv_line` drives it, and what the session wants sent (its replies, an
//! alert on a bad record) is flushed by whichever handle holds the lock.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::{ServerConfig, ServerConnection};

use wsp_api::{ConnError, Connection, Frame, BEAT_SECS, MAX_LINE, MISSED_BEATS};

/// How long a write may block on a peer that reads nothing before it fails:
/// a peer that has stopped reading for this long has missed its beats.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(BEAT_SECS * MISSED_BEATS as u64);

/// One end of a TCP connection that carries frames.
pub struct Tcp {
    stream: TcpStream,
    /// Bytes read past the last whole line.
    partial: Vec<u8>,
    /// The TLS session every handle of this stream shares, when it is TLS.
    tls: Option<Arc<Mutex<ServerConnection>>>,
    /// The peer sent `close_notify`: no more bytes will come.
    ended: bool,
}

impl Tcp {
    /// Carry frames on `stream`.
    pub fn new(stream: TcpStream) -> std::io::Result<Tcp> {
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(Tcp { stream, partial: Vec::new(), tls: None, ended: false })
    }

    /// Carry frames on `stream` inside a TLS session `config` makes: the server's
    /// side, whose handshake the first read drives.
    pub fn accept_tls(stream: TcpStream, config: Arc<ServerConfig>) -> std::io::Result<Tcp> {
        let mut tcp = Tcp::new(stream)?;
        let session = ServerConnection::new(config).map_err(std::io::Error::other)?;
        tcp.tls = Some(Arc::new(Mutex::new(session)));
        Ok(tcp)
    }

    /// Whether this stream is inside TLS.
    pub fn is_tls(&self) -> bool {
        self.tls.is_some()
    }

    /// Dial `addr`, waiting up to `timeout` for it to accept.
    pub fn dial(addr: SocketAddr, timeout: Duration) -> Result<Tcp, ConnError> {
        let stream = TcpStream::connect_timeout(&addr, timeout).map_err(io)?;
        Tcp::new(stream).map_err(io)
    }

    /// A second handle on the same stream, for another thread to send on.
    pub fn try_clone(&self) -> std::io::Result<Tcp> {
        Ok(Tcp { stream: self.stream.try_clone()?, partial: Vec::new(), tls: self.tls.clone(), ended: false })
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

    /// All of `bytes`, on the socket or through the TLS session.
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), ConnError> {
        let Some(tls) = &self.tls else { return self.stream.write_all(bytes).map_err(io) };
        let mut session = tls.lock().unwrap_or_else(|e| e.into_inner());
        let mut rest = bytes;
        let mut stalled = false;
        while !rest.is_empty() {
            // The session buffers a bounded amount of plaintext, so a long line is
            // fed in pieces, the encrypted bytes written out between them.
            let n = session.writer().write(rest).map_err(io)?;
            rest = &rest[n..];
            flush(&mut session, &self.stream)?;
            if n == 0 && std::mem::replace(&mut stalled, true) {
                return Err(ConnError::Io("tls: the session took no more bytes".into()));
            }
        }
        flush(&mut session, &self.stream)
    }

    /// Read what the socket has within `left`, into `partial`. A quiet socket is `Ok`.
    fn fill(&mut self, left: Duration) -> Result<(), ConnError> {
        let mut chunk = [0u8; 8192];
        // A zero read timeout is refused by the OS; a millisecond is the floor.
        self.stream.set_read_timeout(Some(left.max(Duration::from_millis(1)))).map_err(io)?;
        match self.stream.read(&mut chunk) {
            Ok(0) => Err(ConnError::Closed),
            Ok(n) => match self.tls.clone() {
                None => {
                    self.partial.extend_from_slice(&chunk[..n]);
                    Ok(())
                }
                Some(tls) => self.decrypt(&tls, &chunk[..n]),
            },
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => Ok(()),
            Err(e) => Err(io(e)),
        }
    }

    /// Hand `raw`, bytes the socket gave, to the session; what it decrypts goes
    /// to `partial`, and what it wants sent (the handshake's replies, an alert
    /// for a record it refused) is written before an error is returned.
    fn decrypt(&mut self, tls: &Mutex<ServerConnection>, raw: &[u8]) -> Result<(), ConnError> {
        let mut session = tls.lock().unwrap_or_else(|e| e.into_inner());
        let mut cur = raw;
        let mut plain = [0u8; 8192];
        while !cur.is_empty() {
            let taken = session.read_tls(&mut cur).map_err(io)?;
            let processed = session.process_new_packets();
            let sent = flush(&mut session, &self.stream);
            processed.map_err(|e| ConnError::Io(format!("tls: {e}")))?;
            sent?;
            loop {
                match session.reader().read(&mut plain) {
                    Ok(0) => {
                        self.ended = true;
                        break;
                    }
                    Ok(n) => self.partial.extend_from_slice(&plain[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => return Err(io(e)),
                }
            }
            if taken == 0 {
                break;
            }
        }
        Ok(())
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
            if self.ended {
                return Err(ConnError::Closed);
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

/// Write what `session` has to send to `stream`.
fn flush(session: &mut ServerConnection, stream: &TcpStream) -> Result<(), ConnError> {
    let mut out = stream;
    while session.wants_write() {
        if session.write_tls(&mut out).map_err(io)? == 0 {
            return Err(ConnError::Closed);
        }
    }
    Ok(())
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

    /// A TLS client of the test's own, on rustls's blocking stream: a frame in, a frame out.
    #[cfg(feature = "test-support")]
    mod tls {
        use super::*;
        use crate::testing::TestPki;
        use rustls::pki_types::{pem::PemObject, CertificateDer, ServerName};
        use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

        fn pair() -> (StreamOwned<ClientConnection, TcpStream>, Tcp, std::path::PathBuf) {
            let dir = std::env::temp_dir().join(format!("wsp-tcp-tls-{}-{:?}", std::process::id(), std::thread::current().id()));
            let pki = TestPki::new(&dir, "t");
            let served = crate::tls::server_config(&pki.cert, &pki.key).unwrap();
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let sock = TcpStream::connect(l.local_addr().unwrap()).unwrap();
            let (accepted, _) = l.accept().unwrap();
            let mut roots = RootCertStore::empty();
            for c in CertificateDer::pem_file_iter(&pki.ca).unwrap() {
                roots.add(c.unwrap()).unwrap();
            }
            let config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let session = ClientConnection::new(Arc::new(config), ServerName::try_from("localhost").unwrap()).unwrap();
            (StreamOwned::new(session, sock), Tcp::accept_tls(accepted, served).unwrap(), dir)
        }

        fn line_of(f: &Frame) -> Vec<u8> {
            let mut l = serde_json::to_vec(f).unwrap();
            l.push(b'\n');
            l
        }

        #[test]
        fn frames_cross_inside_tls_in_both_directions_and_a_long_line_arrives_whole() {
            let (mut client, mut server, dir) = pair();
            let writer = std::thread::spawn(move || {
                client.write_all(&line_of(&beat(1))).unwrap();
                let long = Frame::Refused(wsp_api::Refused { code: wsp_api::Code::Malformed, text: "x".repeat(300_000) });
                client.write_all(&line_of(&long)).unwrap();
                // What the server says back, read on the same session.
                let mut got = Vec::new();
                let mut chunk = [0u8; 4096];
                while !got.contains(&b'\n') {
                    let n = client.read(&mut chunk).unwrap();
                    got.extend_from_slice(&chunk[..n]);
                }
                got
            });
            assert_eq!(server.recv(Duration::from_secs(5)).unwrap(), Some(beat(1)), "the handshake rides the first read");
            match server.recv(Duration::from_secs(5)).unwrap() {
                Some(Frame::Refused(r)) => assert_eq!(r.text.len(), 300_000),
                other => panic!("the long line: {other:?}"),
            }
            assert!(server.is_tls());
            server.send(&beat(2)).unwrap();
            let got = writer.join().unwrap();
            assert_eq!(serde_json::from_slice::<Frame>(&got[..got.len() - 1]).unwrap(), beat(2));
            let _ = std::fs::remove_dir_all(dir);
        }

        /// The design that lets a feed write while the connection waits to read: the reading
        /// handle blocks on the socket without the session's lock, so a clone's send is not held up.
        #[test]
        fn a_clone_sends_while_the_reading_handle_waits_and_the_peer_closing_is_closed() {
            let (mut client, mut server, dir) = pair();
            let mut writer = server.try_clone().unwrap();
            let (said, heard) = std::sync::mpsc::channel();
            // The handshake needs both ends running, so the server reads on its own thread.
            let reader = std::thread::spawn(move || {
                let first = server.recv(Duration::from_secs(10));
                said.send(()).unwrap();
                let second = server.recv(Duration::from_secs(10));
                let third = server.recv(Duration::from_secs(10));
                (first, second, third)
            });
            client.write_all(&line_of(&beat(1))).unwrap();
            heard.recv().unwrap();
            std::thread::sleep(Duration::from_millis(100)); // the reader is now waiting on the socket
            let started = Instant::now();
            writer.send(&beat(2)).unwrap();
            assert!(started.elapsed() < Duration::from_secs(2), "a send waited on a read: {:?}", started.elapsed());
            let mut got = Vec::new();
            let mut chunk = [0u8; 4096];
            while !got.contains(&b'\n') {
                let n = client.read(&mut chunk).unwrap();
                got.extend_from_slice(&chunk[..n]);
            }
            assert_eq!(serde_json::from_slice::<Frame>(&got[..got.len() - 1]).unwrap(), beat(2));
            client.write_all(&line_of(&beat(3))).unwrap();
            drop(client);
            let (first, second, third) = reader.join().unwrap();
            assert_eq!((first.unwrap(), second.unwrap()), (Some(beat(1)), Some(beat(3))));
            assert_eq!(third, Err(ConnError::Closed));
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn a_peer_that_does_not_speak_tls_is_an_error_and_not_a_hang() {
            let (client, mut server, dir) = pair();
            let mut raw = client.sock.try_clone().unwrap();
            // Not a ClientHello: the session refuses it and says so.
            raw.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
            match server.recv_line(Duration::from_secs(5)) {
                Err(ConnError::Io(why)) => assert!(why.starts_with("tls:"), "{why}"),
                other => panic!("{other:?}"),
            }
            let _ = std::fs::remove_dir_all(dir);
        }
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
