//! The `Connection` trait (3.4): one [`Frame`] per line of JSON, whatever
//! carries the lines. TCP on the tailnet is the first carrier, TLS or a
//! WebSocket a later one; each is an implementation of this trait in the
//! crate that does the IO, never a new protocol.
//!
//! The framing rule every implementation keeps: a frame is written as
//! `serde_json`'s compact form followed by one `\n`. The compact form never
//! holds a raw newline (one inside a string is escaped), so a line is always
//! exactly one frame. A line that does not decode is [`ConnError::Malformed`];
//! the connection stays usable and the peer is answered `refused malformed`.
//!
//! This crate holds one implementation, the in-memory [`pair`], for tests.

use crate::Frame;
use std::time::Duration;

/// Why a connection could not send or receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnError {
    /// The peer closed the connection, or it was lost.
    Closed,
    /// A line arrived that is not a frame. The text says what failed.
    Malformed(String),
    /// The carrier failed in some other way (an IO error, as text).
    Io(String),
}

impl std::fmt::Display for ConnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnError::Closed => f.write_str("connection closed"),
            ConnError::Malformed(why) => write!(f, "malformed frame: {why}"),
            ConnError::Io(why) => write!(f, "connection failed: {why}"),
        }
    }
}

impl std::error::Error for ConnError {}

/// A connection between wsp and a manager, either way round.
pub trait Connection {
    /// Write one frame as one line.
    fn send(&mut self, frame: &Frame) -> Result<(), ConnError>;

    /// Wait up to `wait` for the next frame. `Ok(None)` is a quiet `wait`,
    /// which is how a side counts missed beats.
    fn recv(&mut self, wait: Duration) -> Result<Option<Frame>, ConnError>;
}

#[cfg(any(test, feature = "test-support"))]
mod memory {
    use super::{ConnError, Connection};
    use crate::Frame;
    use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
    use std::time::Duration;

    /// One end of an in-memory connection. Frames cross as the lines a real
    /// carrier would write, so a test of the framing tests the real rule.
    pub struct Memory {
        tx: Sender<String>,
        rx: Receiver<String>,
    }

    /// Two connected ends. Dropping one closes the other.
    pub fn pair() -> (Memory, Memory) {
        let (a_tx, b_rx) = channel();
        let (b_tx, a_rx) = channel();
        (Memory { tx: a_tx, rx: a_rx }, Memory { tx: b_tx, rx: b_rx })
    }

    impl Memory {
        /// Write a raw line, as a broken or hostile peer might.
        pub fn send_line(&mut self, line: &str) -> Result<(), ConnError> {
            self.tx
                .send(line.to_string())
                .map_err(|_| ConnError::Closed)
        }
    }

    impl Connection for Memory {
        fn send(&mut self, frame: &Frame) -> Result<(), ConnError> {
            let mut line =
                serde_json::to_string(frame).map_err(|e| ConnError::Malformed(e.to_string()))?;
            line.push('\n');
            self.send_line(&line)
        }

        fn recv(&mut self, wait: Duration) -> Result<Option<Frame>, ConnError> {
            let line = match self.rx.recv_timeout(wait) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => return Err(ConnError::Closed),
            };
            let body = line
                .strip_suffix('\n')
                .ok_or_else(|| ConnError::Malformed("no newline".into()))?;
            if body.contains('\n') {
                return Err(ConnError::Malformed("more than one line".into()));
            }
            serde_json::from_str(body)
                .map(Some)
                .map_err(|e| ConnError::Malformed(e.to_string()))
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub use memory::{pair, Memory};
