//! TLS on the server's listener (wsp-461): a [`ServerConfig`] from the PEM
//! files a hub's config names, which [`crate::Config::tls`] hands to every
//! accepted connection (`tcp::Tcp::accept_tls`). The daemon's own server sets
//! none and stays plain TCP on loopback and the tailnet.
//!
//! The provider is `ring`, named where the config is built, so no
//! process-wide default is installed for anything else in the process.

use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;

/// The server config for the certificate chain at `cert` and its private key
/// at `key`, both PEM. The error names the file that did not read.
pub fn server_config(cert: &Path, key: &Path) -> Result<Arc<ServerConfig>, String> {
    let chain = CertificateDer::pem_file_iter(cert)
        .map_err(|e| format!("certificate {}: {e}", cert.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("certificate {}: {e}", cert.display()))?;
    if chain.is_empty() {
        return Err(format!("certificate {}: no certificate in it", cert.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("key {}: {e}", key.display()))?;
    let config = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls versions: {e}"))?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|e| format!("certificate {} and its key do not make a server identity: {e}", cert.display()))?;
    Ok(Arc::new(config))
}
