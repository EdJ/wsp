//! The manager token (`docs/agent-manager.md` 3.3): made at the manager's
//! first start, mode 0600 in `<exec_dir>`, one per host. A client's `hello`
//! must carry it, and a connection whose `hello` does not is closed. wsp
//! keeps a copy in the machine record's secret file (wsp-024 installs it).

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use wsp_host_api::Token;

/// 32 random bytes, as hex.
const BYTES: usize = 32;

/// The token at `path`, made there first if there is none. A token file
/// that others could read is narrowed to 0600, and the second value says so,
/// for the log.
pub fn ensure(path: &Path) -> io::Result<(Token, bool)> {
    match OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
        Ok(mut f) => {
            let mut raw = [0u8; BYTES];
            fs::File::open("/dev/urandom")?.read_exact(&mut raw)?;
            let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
            f.write_all(hex.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
            Ok((Token(hex), false))
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let mode = fs::metadata(path)?.permissions().mode() & 0o777;
            let narrowed = mode != 0o600;
            if narrowed {
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
            let text = fs::read_to_string(path)?;
            let token = text.trim();
            if token.is_empty() {
                return Err(io::Error::other(format!("{} is empty; remove it to make a new token", path.display())));
            }
            Ok((Token(token.to_string()), narrowed))
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn made_once_at_0600_and_read_back_after() {
        let d = std::env::temp_dir().join(format!("wsp-manager-token-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let p = d.join("manager.token");
        let (made, narrowed) = ensure(&p).unwrap();
        assert!(!narrowed);
        assert_eq!(made.0.len(), BYTES * 2);
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let (again, _) = ensure(&p).unwrap();
        assert_eq!(made, again, "a restart keeps the token wsp holds a copy of");

        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        let (_, narrowed) = ensure(&p).unwrap();
        assert!(narrowed);
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
