//! The framing rule and the [`Connection`] trait: one [`Frame`] per line.
//!
//! A frame is written as `serde_json`'s compact form followed by one `\n`. The
//! compact form never holds a raw newline (one inside a string is escaped), so
//! a line is always exactly one frame. A line that does not decode is
//! [`ConnError::Malformed`] and the connection stays usable. A line longer
//! than [`MAX_LINE`] bytes is not read: the carrier fails with
//! [`ConnError::Io`], since it cannot find the next frame without reading the
//! long one to its end. The carriers (TCP, TLS) are the IO crates' own.

use crate::Frame;
use std::time::Duration;

/// The longest line a carrier reads, in bytes: 16 MB. No reply is sent
/// longer; [`Code::TooLarge`](crate::Code::TooLarge) is sent in its place.
pub const MAX_LINE: usize = 16 * 1024 * 1024;

/// Why a connection could not send or receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnError {
    /// The peer closed the connection, or it was lost.
    Closed,
    /// A line arrived that is not a frame. The text says what failed.
    Malformed(String),
    /// The carrier failed in some other way (an IO error, a line over
    /// [`MAX_LINE`], as text).
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

/// A connection between a client and the hub, either way round.
pub trait Connection {
    /// Write one frame as one line.
    fn send(&mut self, frame: &Frame) -> Result<(), ConnError>;

    /// Wait up to `wait` for the next frame. `Ok(None)` is a quiet `wait`.
    fn recv(&mut self, wait: Duration) -> Result<Option<Frame>, ConnError>;
}
