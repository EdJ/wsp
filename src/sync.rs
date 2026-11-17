//! Projection of the durable store into herdr's sidebar tokens.

use std::collections::HashMap;

use crate::store::Store;

#[derive(Default)]
pub struct Cache {
    sent: HashMap<String, String>,
}

impl Cache {
    pub fn clear(&mut self) {
        self.sent.clear();
    }
}

pub struct Report;

/// Compute and push tokens. The tokens, the roster and the reap all went
/// through herdr's socket, and herdr is gone (wsp-120): a host with no socket
/// answered the first listing with an error, and so does this.
pub fn sync(_store: &Store, cache: &mut Cache, force: bool) -> std::io::Result<Report> {
    if force {
        cache.clear();
    }
    Err(std::io::Error::new(std::io::ErrorKind::NotFound, "herdr is gone"))
}
