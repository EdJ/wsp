//! What a client needs to dial a hub over TLS (wsp-461): the roots it trusts
//! and the name the certificate must carry. A hub URL of `tls://host:port` is
//! dialled over TLS with the platform's roots; a CA file ([`crate::Client::dial_hub_ca`],
//! `--ca <file>`, `$WSP_HUB_CA`, or `ca` in `hubs.json`) *pins* a private CA
//! instead: then that CA's certificates are trusted and nothing else is, so a
//! test or a self-hosted hub does not widen what a client trusts.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};

use crate::Silent;

/// The roots, and the name the hub's certificate must carry, for one hub.
#[derive(Clone)]
pub(crate) struct Tls {
    pub(crate) config: Arc<ClientConfig>,
    pub(crate) name: ServerName<'static>,
}

impl fmt::Debug for Tls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Tls({:?})", self.name)
    }
}

impl Tls {
    /// TLS to the hub at `addr` (`host:port`): the certificate must be for
    /// `host` and chain to `ca` when one is given, else to the platform's roots.
    pub(crate) fn new(addr: &str, ca: Option<&Path>) -> Result<Tls, Silent> {
        let host = host_of(addr);
        let name = ServerName::try_from(host.to_string())
            .map_err(|e| Silent::Tls(format!("{addr}: {host} is not a name a certificate can carry: {e}")))?;
        let mut roots = RootCertStore::empty();
        match ca {
            Some(path) => {
                let certs = CertificateDer::pem_file_iter(path)
                    .map_err(|e| Silent::Tls(format!("ca file {}: {e}", path.display())))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| Silent::Tls(format!("ca file {}: {e}", path.display())))?;
                if certs.is_empty() {
                    return Err(Silent::Tls(format!("ca file {}: no certificate in it", path.display())));
                }
                let (added, _) = roots.add_parsable_certificates(certs);
                if added == 0 {
                    return Err(Silent::Tls(format!("ca file {}: no certificate in it reads", path.display())));
                }
            }
            None => {
                // A root the platform lists that does not read is skipped; with none left
                // every hub fails its certificate, by name, which says what is missing.
                let found = rustls_native_certs::load_native_certs();
                roots.add_parsable_certificates(found.certs);
            }
        }
        let config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| Silent::Tls(format!("tls versions: {e}")))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Tls { config: Arc::new(config), name })
    }
}

/// The host of `host:port`, `[::1]:port` without its brackets.
fn host_of(addr: &str) -> &str {
    let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
    host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_is_what_precedes_the_port() {
        assert_eq!(host_of("hub.invalid:7489"), "hub.invalid");
        assert_eq!(host_of("[::1]:7489"), "::1");
        assert_eq!(host_of("localhost:1"), "localhost");
    }
}
