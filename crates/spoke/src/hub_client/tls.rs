//! What a client needs to dial a hub over TLS: the roots it trusts
//! and the name the certificate must carry. A hub URL of `tls://host:port` is
//! dialled over TLS with the platform's roots; a CA file ([`super::Client::dial_hub_ca`],
//! `--ca <file>`, `$WSP_HUB_CA`, or `ca` in `hubs.json`) *pins* a private CA
//! instead: then that CA's certificates are trusted and nothing else is, so a
//! test or a self-hosted hub does not widen what a client trusts.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};

use super::Silent;

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

/// When the certificate `der` holds stops being valid, in epoch seconds: the
/// `notAfter` of its validity, read from the DER without a parser of its own
/// (a certificate is `SEQUENCE { tbs SEQUENCE { [0] version, serial, signature,
/// issuer, validity SEQUENCE { notBefore, notAfter }, .. }, .. }`).
pub(crate) fn not_after(der: &[u8]) -> Option<i64> {
    fn read(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let (&tag, b) = b.split_first()?;
        let (&l0, b) = b.split_first()?;
        let (len, b) = if l0 < 0x80 {
            (l0 as usize, b)
        } else {
            let n = (l0 & 0x7f) as usize;
            if n == 0 || n > 4 || b.len() < n {
                return None;
            }
            (b[..n].iter().fold(0usize, |a, x| (a << 8) | *x as usize), &b[n..])
        };
        (b.len() >= len).then(|| (tag, &b[..len], &b[len..]))
    }
    let (_, cert, _) = read(der)?;
    let (_, tbs, _) = read(cert)?;
    let mut rest = tbs;
    let (mut tag, mut item, mut after) = read(rest)?;
    if tag == 0xa0 {
        rest = after;
        (tag, item, after) = read(rest)?;
    }
    let _ = (tag, item);
    // serial, signature algorithm, issuer, then the validity
    let (_, _, r) = read(after)?;
    let (_, _, r) = read(r)?;
    let (_, validity, _) = read(r)?;
    let (_, _, r) = read(validity)?;
    let (tag, t, _) = read(r)?;
    time_of(tag, std::str::from_utf8(t).ok()?)
}

/// A UTCTime (`YYMMDDHHMMSSZ`) or GeneralizedTime (`YYYYMMDDHHMMSSZ`) as epoch seconds.
fn time_of(tag: u8, t: &str) -> Option<i64> {
    let t = t.strip_suffix('Z')?;
    let (year, rest) = match tag {
        0x17 if t.len() == 12 => {
            let yy: i64 = t[..2].parse().ok()?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &t[2..])
        }
        0x18 if t.len() == 14 => (t[..4].parse().ok()?, &t[4..]),
        _ => return None,
    };
    let n = |i: usize| rest.get(i..i + 2)?.parse::<i64>().ok();
    let (mo, d, h, mi, s) = (n(0)?, n(2)?, n(4)?, n(6)?, n(8)?);
    // days from civil (Howard Hinnant)
    let y = if mo <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + s)
}

/// The host of `host:port`, `[::1]:port` without its brackets.
fn host_of(addr: &str) -> &str {
    let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
    host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A certificate `openssl` made, its `notAfter` Dec 23 21:10:37 2026 GMT.
    #[test]
    fn a_certificates_not_after_is_read_from_its_der() {
        const PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIDDzCCAfegAwIBAgIUArw+OtLN1jTAJq1ZTQZmxmezkKowDQYJKoZIhvcNAQEL\n\
BQAwDDEKMAgGA1UEAwwBeDAeFw0yNjExMjMyMTEwMzdaFw0yNjEyMjMyMTEwMzda\n\
MAwxCjAIBgNVBAMMAXgwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQC2\n\
V1mczOvxq88ZI4Sv9pE+lRDc/Ynhe3rdf6usPcTHVqCP+K5GbzZ3JRV0xZZ9sBvk\n\
QcqS8X17y4h3JNwfp9pKNQLlKjM/WsF78IqtxQlYBjax5AJ+CrLMk/K1alkuvI4m\n\
LBznPH8so18qMUmQx/vgchAQOQQ+ynL3ma9VIbQO/LzJH1xsWdkKcN3xsoU6KJdo\n\
cwnRcVtilj4w3kVnRsPGdwceGMaLKufpcHCwEV8laR+vf5LpIkI05MIidZupIx/7\n\
iQaTxoyULsy37+WlwNC7AF3MG9jgp8t/0lj2X0ajZLEXbeDm8hqYV7s7GwxWVVNn\n\
cPnqhbRG/9E3x84xKB+hAgMBAAGjaTBnMB0GA1UdDgQWBBTq20H8by6gto/3h3AS\n\
fDj4PtPJTjAfBgNVHSMEGDAWgBTq20H8by6gto/3h3ASfDj4PtPJTjAPBgNVHRMB\n\
Af8EBTADAQH/MBQGA1UdEQQNMAuCCWxvY2FsaG9zdDANBgkqhkiG9w0BAQsFAAOC\n\
AQEAUzYSNvua/go2lwU46bMapXr9lst1ZzoblCjUobnzl7j6QEA9XAFD8resp4el\n\
0q8A9dPUNUGRSHIxSRsGBEp/fFLmQ4ZPwA/9qd3+j1c0o3DToznTPZyEMhu8AOMC\n\
LJXchAus+K4SkHqT+c+1GWA0ZLv3f+sdyNNj7x5jC+X5VrMmyna/UW5HLgFGTYhH\n\
ZXNPyMdbIJZt22hpFbjY00nN7Wut0sKs8TXdCPu9nW//kewdhnqqKIlAXZs+RheX\n\
mKbGRUtEB6DC6+NugYXQMPDhG8jv0sGBVvii2/z9EsrSGU4EiyeFBMepl+dVm7JT\n\
0CpvXklkHg9gBfQTqykrbkPwlQ==\n\
-----END CERTIFICATE-----\n\
";
        let der = CertificateDer::from_pem_slice(PEM.as_bytes()).unwrap();
        assert_eq!(not_after(der.as_ref()), Some(1_798_060_237));
    }

    #[test]
    fn a_validity_time_reads_as_epoch_seconds() {
        assert_eq!(time_of(0x17, "700101000000Z"), Some(0));
        assert_eq!(time_of(0x17, "260101000000Z"), Some(1_767_225_600));
        assert_eq!(time_of(0x18, "20260101000000Z"), Some(1_767_225_600));
        assert_eq!(time_of(0x18, "20260101000000"), None);
    }

    #[test]
    fn the_host_is_what_precedes_the_port() {
        assert_eq!(host_of("hub.invalid:7489"), "hub.invalid");
        assert_eq!(host_of("[::1]:7489"), "::1");
        assert_eq!(host_of("localhost:1"), "localhost");
    }
}
