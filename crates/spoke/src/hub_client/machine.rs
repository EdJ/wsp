//! The machine a spoke's hello names (`hello { machine: {id, name} }`): a hub
//! holds a lease to its user *on that machine*, and refuses the same user's
//! other machines `held`, naming the one that holds it.
//!
//! **The id** is random, made once and kept in `.config/wsp/machine.json`
//! under the hub home ([`super::hub_home`]: `$WSP_HUB_HOME`, else `$HOME`),
//! beside `hubs.json` and not in it: `wsp logout` removes `hubs.json` and
//! leaves this file, and `wsp login` (again, or as someone else) never
//! touches it, so a lease taken before a re-login is still this machine's.
//! It is per hub home, not per state directory: one person's wsp instances on
//! one machine share a login and so a machine, and a sandbox, which sets
//! `$WSP_HUB_HOME` of its own, is a machine of its own (as two spokes in a
//! test are). It is not a secret and not the token: it says which machine,
//! never who.
//!
//! **The name** is the host name ([`wsp_base::util::hostname`]), read at each
//! dial, so a renamed machine is named anew while its leases stay its own. It
//! is what a refusal names to a person, held to what a hub takes
//! ([`MAX_MACHINE`] bytes, no control character), since a hub refuses a hello
//! whose machine breaks either.

use std::path::{Path, PathBuf};

use wsp_hub_protocol::{Machine, MAX_MACHINE};

/// The file of the machine's id, under the hub home.
pub const MACHINE_FILE: &str = ".config/wsp/machine.json";

/// This machine as the hello names it: the id kept under `home` (made now if
/// there is none) and the host name. `Err` when the id can be neither read
/// nor made (an unwritable home): the caller then names no machine, and the
/// hub holds its leases by user alone, as it did before machines.
pub fn this(home: &Path) -> Result<Machine, String> {
    Ok(Machine { id: id_in(home)?, name: name_of(&wsp_base::util::hostname()) })
}

/// The id kept under `home`; None when no dial has made one yet.
pub fn kept_id(home: &Path) -> Option<String> {
    read(&home.join(MACHINE_FILE))
}

fn read(path: &Path) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v["id"].as_str().filter(|id| fits(id)).map(str::to_string)
}

/// The id under `home`, made once. Two processes making it at once agree: the
/// file appears whole or not at all (written aside, then linked into place,
/// which fails where one is there already), and the loser reads the winner's.
fn id_in(home: &Path) -> Result<String, String> {
    let path = home.join(MACHINE_FILE);
    if let Some(id) = read(&path) {
        return Ok(id);
    }
    let dir: PathBuf = path.parent().expect("under .config/wsp").to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let id = format!("m-{}", random_hex(16)?);
    // Named by the id it holds: no other writer, thread or process, shares it.
    let tmp = dir.join(format!(".machine.json.{id}.tmp"));
    let body = serde_json::json!({ "id": id, "made": wsp_base::util::iso_at(wsp_base::util::epoch_secs()) }).to_string();
    std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let linked = std::fs::hard_link(&tmp, &path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(id),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read(&path).ok_or_else(|| format!("{} is there and holds no machine id", path.display()))
        }
        // A file that is there but did not read (written by hand, cut short):
        // it is left for the person to look at, and no machine is named.
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// An id a hub takes: not empty, at most [`MAX_MACHINE`] bytes, no control character.
fn fits(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_MACHINE && !s.chars().any(char::is_control)
}

/// `host` as a hub takes a machine's name: control characters dropped, cut to
/// [`MAX_MACHINE`] bytes on a character's boundary.
pub fn name_of(host: &str) -> String {
    let mut out = String::new();
    for c in host.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_MACHINE {
            break;
        }
        out.push(c);
    }
    out
}

fn random_hex(n: usize) -> Result<String, String> {
    use std::io::Read;
    let mut bytes = vec![0u8; n];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("the system's randomness (/dev/urandom): {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tag: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("wsp-spoke-machine-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    #[test]
    fn machine_id_is_made_once_kept_beside_the_login_and_differs_per_home() {
        let a = home("a");
        assert_eq!(kept_id(&a), None);
        let first = this(&a).unwrap();
        assert!(first.id.starts_with("m-") && first.id.len() == 34, "{}", first.id);
        assert_eq!(this(&a).unwrap().id, first.id, "made once");
        assert_eq!(kept_id(&a).as_deref(), Some(first.id.as_str()));
        assert!(a.join(MACHINE_FILE).exists() && !a.join(super::super::HUBS_FILE).exists(), "its own file, not the login's");
        let b = home("b");
        assert_ne!(this(&b).unwrap().id, first.id, "another hub home is another machine");
    }

    #[test]
    fn machine_ids_made_at_once_by_many_agree() {
        let h = home("race");
        let ids: Vec<String> = (0..8)
            .map(|_| {
                let h = h.clone();
                std::thread::spawn(move || this(&h).unwrap().id)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();
        assert!(ids.iter().all(|i| *i == ids[0]), "{ids:?}");
    }

    #[test]
    fn machine_name_is_held_to_what_a_hub_takes() {
        assert_eq!(name_of("box-1"), "box-1");
        assert_eq!(name_of("a\u{7}b\nc"), "abc");
        let long = "é".repeat(200);
        let cut = name_of(&long);
        assert!(cut.len() <= MAX_MACHINE && cut.chars().all(|c| c == 'é'), "{}", cut.len());
    }

    #[test]
    fn machine_a_file_that_does_not_read_is_left_and_no_machine_is_named() {
        let h = home("junk");
        std::fs::create_dir_all(h.join(".config/wsp")).unwrap();
        std::fs::write(h.join(MACHINE_FILE), "not json").unwrap();
        assert!(this(&h).is_err());
        assert_eq!(std::fs::read_to_string(h.join(MACHINE_FILE)).unwrap(), "not json");
    }
}
