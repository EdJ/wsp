//! A throwaway PKI for the tests of whatever serves or dials TLS (wsp-461),
//! behind `test-support`: a generated CA and a certificate for `localhost`
//! signed by it, written as PEM under a directory the test owns. No host is a
//! literal of anyone's: the certificate names `localhost` and nothing else.

use std::path::{Path, PathBuf};

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};

/// A CA and the certificate and key of a server named `localhost` under it.
#[derive(Debug, Clone)]
pub struct TestPki {
    /// The CA's certificate: what a client pins (`--ca`).
    pub ca: PathBuf,
    /// The server's certificate, the leaf alone.
    pub cert: PathBuf,
    /// The server's private key.
    pub key: PathBuf,
}

impl TestPki {
    /// A new CA named `name` and a `localhost` certificate under it, in `dir`
    /// (made if missing) as `<name>-ca.pem`, `<name>.pem` and `<name>-key.pem`.
    pub fn new(dir: &Path, name: &str) -> TestPki {
        std::fs::create_dir_all(dir).expect("the pki directory");
        let ca_key = KeyPair::generate().expect("a CA key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        ca_params.distinguished_name.push(DnType::CommonName, format!("wsp test CA {name}"));
        let ca_cert = ca_params.self_signed(&ca_key).expect("a CA certificate");

        let leaf_key = KeyPair::generate().expect("a leaf key");
        let leaf_params = CertificateParams::new(vec!["localhost".to_string()]).expect("leaf params");
        let issuer = Issuer::from_params(&ca_params, &ca_key);
        let leaf = leaf_params.signed_by(&leaf_key, &issuer).expect("a leaf certificate");

        let write = |file: String, pem: String| {
            let path = dir.join(file);
            std::fs::write(&path, pem).expect("a pem file");
            path
        };
        TestPki {
            ca: write(format!("{name}-ca.pem"), ca_cert.pem()),
            cert: write(format!("{name}.pem"), leaf.pem()),
            key: write(format!("{name}-key.pem"), leaf_key.serialize_pem()),
        }
    }
}
