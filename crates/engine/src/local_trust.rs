//! Claude Code's folder trust for a sandbox seat's tree (`wsp-466`).
//!
//! `wsp-445` trusts a far seat's tree by name before claude starts there. The
//! same stop recurs for a seat spawned **locally inside a `wsp sandbox`**: its
//! tree is a new worktree under a real repository, claude has never been in it,
//! and it stops at "Quick safety check" (and, if the tree's CLAUDE.md imports a
//! file outside it, at the external-imports prompt) — dialogs only a person can
//! press. compounding-448's drive sat held 21 minutes on one.
//!
//! So `wsp spawn` inside a sandbox ([`trust`]) writes `hasTrustDialogAccepted`
//! and the two external-imports acknowledgements for the seat's tree into
//! `~/.claude.json` before claude starts, through the same helper `wsp-445` runs
//! on a far machine ([`crate::far::trust_helper_script`]), and `wsp sandbox rm`
//! ([`clear`]) takes them out again.
//!
//! **Only trees the sandbox made.** The file is a person's, so nothing is
//! written for a path that is not a worktree under `.worktrees/`, nor for a tree
//! the file already has an entry for that this sandbox did not add — the person
//! (or another sandbox) owns that entry. What it did add is written down in
//! `<sandbox>/trust.json`, and `rm` removes exactly that. The file is copied to
//! `<state>/trust-backups/` before the sandbox's first write.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};


const LEDGER: &str = "trust.json";

/// `$CLAUDE_CONFIG_DIR/.claude.json`, else `~/.claude.json`: the helper's rule.
pub fn claude_json() -> PathBuf {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default();
    base.join(".claude.json")
}

#[derive(Default)]
struct Ledger {
    trees: Vec<String>,
    backup: Option<String>,
}

fn read_ledger(dir: &Path) -> Ledger {
    let v: Value = std::fs::read_to_string(dir.join(LEDGER)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    Ledger {
        trees: v["trees"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(String::from)).collect(),
        backup: v["backup"].as_str().map(String::from),
    }
}

fn write_ledger(dir: &Path, l: &Ledger) -> Result<(), String> {
    std::fs::write(dir.join(LEDGER), json!({ "trees": l.trees, "backup": l.backup }).to_string())
        .map_err(|e| format!("cannot write {}: {e}", dir.join(LEDGER).display()))
}

/// Whether the file already has an entry for `tree`; an error when it is not a JSON object.
fn has_entry(file: &Path, tree: &str) -> Result<bool, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let doc: Value = serde_json::from_str(&text).map_err(|_| format!("{} is not JSON; not touching it", file.display()))?;
    let real = std::fs::canonicalize(tree).ok().map(|p| p.display().to_string());
    let projects = &doc["projects"];
    Ok(projects.get(tree).is_some() || real.is_some_and(|r| projects.get(&r).is_some()))
}

/// Run the trust helper (`add` or `remove`) for `tree`, written under the sandbox's `bin`.
fn helper(dir: &Path, op: &str, tree: &str) -> Result<(), String> {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).map_err(|e| format!("cannot make {}: {e}", bin.display()))?;
    let path = bin.join("wsp-trust");
    std::fs::write(&path, crate::far::trust_helper_script()).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    let out = Command::new("/bin/sh").arg(&path).arg(op).arg(tree).output().map_err(|e| format!("cannot run {}: {e}", path.display()))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

/// Trust a seat's tree before claude starts in it, when this is a sandbox's
/// spawn and the tree is one it made. `Ok(None)` for anything else (nothing was
/// written); `Ok(Some(line))` says what was. `sandbox` is the directory of the
/// sandbox this spawn runs in (`cmd_sandbox::dir_of`), `None` outside one: the
/// caller reads it, so this module needs nothing from the layer above it.
pub fn trust(sandbox: Option<PathBuf>, tree: &str) -> Result<Option<String>, String> {
    let Some(dir) = sandbox else { return Ok(None) };
    let t = Path::new(tree);
    let a_worktree = t.parent().and_then(Path::file_name).is_some_and(|n| n == crate::cmd_checkout::WORKTREES) && t.join(".git").exists();
    if !a_worktree {
        return Ok(None);
    }
    let file = claude_json();
    let mut ledger = read_ledger(&dir);
    let ours = ledger.trees.iter().any(|x| x == tree);
    if !ours && has_entry(&file, tree).map_err(|e| format!("not trusting {tree}: {e}"))? {
        return Ok(None);
    }
    if ledger.backup.is_none() && file.is_file() {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("sandbox");
        let to = dir
            .parent()
            .and_then(Path::parent)
            .ok_or("no state directory to keep a backup in")?
            .join("trust-backups")
            .join(format!("claude.json.{name}.{}", crate::util::epoch_secs()));
        std::fs::create_dir_all(to.parent().unwrap_or(Path::new("."))).map_err(|e| format!("cannot back up {}: {e}", file.display()))?;
        std::fs::copy(&file, &to).map_err(|e| format!("cannot back up {} to {}: {e}", file.display(), to.display()))?;
        ledger.backup = Some(to.display().to_string());
    }
    helper(&dir, "add", tree).map_err(|e| format!("trusting {tree}: {e}"))?;
    if !ours {
        ledger.trees.push(tree.to_string());
    }
    write_ledger(&dir, &ledger)?;
    Ok(Some(format!("trusted {tree} in {} before claude starts (`wsp sandbox rm` removes it)", file.display())))
}

/// What `wsp sandbox rm` does about the trust this sandbox added: the lines to
/// say, and whether an entry could not be removed (the caller then keeps the
/// directory, which holds the record, so a second `rm` can try again).
pub fn clear(dir: &Path, dry: bool) -> (Vec<String>, bool) {
    let ledger = read_ledger(dir);
    let mut said = Vec::new();
    let mut kept = Ledger { trees: Vec::new(), backup: ledger.backup.clone() };
    for tree in &ledger.trees {
        if dry {
            said.push(format!("would remove its trust entry for {tree}"));
            continue;
        }
        match helper(dir, "remove", tree) {
            Ok(()) => said.push(format!("removed its trust entry for {tree}")),
            Err(e) => {
                said.push(format!("kept its trust entry for {tree} in {}: {e}", claude_json().display()));
                kept.trees.push(tree.clone());
            }
        }
    }
    if !dry && !ledger.trees.is_empty() {
        let _ = match kept.trees.is_empty() {
            true => std::fs::remove_file(dir.join(LEDGER)),
            false => write_ledger(dir, &kept).map_err(|_| std::io::Error::other("ledger")),
        };
    }
    (said, !kept.trees.is_empty())
}

/// What a seat's screen says when it is stopped at Claude Code's folder-trust
/// dialog: which tree, and the fix. `None` for any other screen.
pub fn dialog_fix(screen: &str) -> Option<String> {
    if !screen.contains("Yes, I trust this folder") {
        return None;
    }
    let dir = screen
        .lines()
        .skip_while(|l| !l.contains("Accessing workspace:"))
        .skip(1)
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|d| format!(" for {d}"))
        .unwrap_or_default();
    Some(format!(
        "Claude Code's folder-trust dialog{dir}; fix: spawn it with `wsp spawn` inside `wsp sandbox`, which trusts the tree it makes before claude starts and `wsp sandbox rm` takes back — a tree it did not make is a person's to trust (Enter on \"Yes, I trust this folder\")"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wsp-local-trust-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn json_of(p: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    /// A sandbox seat's new tree is trusted with its external-imports
    /// acknowledgements, a backup is taken first, a tree the file already knows
    /// is left to its owner, and `clear` takes back exactly what was added.
    #[test]
    fn a_sandbox_trees_trust_is_added_backed_up_and_taken_back_and_nothing_else_is_touched() {
        let root = tmp("flow");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let file = home.join(".claude.json");
        let known_path = root.join("repo/.worktrees/known-1").display().to_string();
        std::fs::write(
            &file,
            format!(r#"{{"userID":"u","projects":{{"/theirs":{{"hasTrustDialogAccepted":true}},"{known_path}":{{"allowedTools":["x"]}}}}}}"#),
        )
        .unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", &home);

        let state_root = root.join("state");
        let sb = state_root.join("sandbox").join("wsp-t");
        let store = Store::at(sb.join("wsp"), sb.join("state"));
        std::fs::create_dir_all(&store.state).unwrap();
        let mk = |name: &str| {
            let t = root.join("repo/.worktrees").join(name);
            std::fs::create_dir_all(t.join(".git")).unwrap();
            t.display().to_string()
        };
        let (new, known) = (mk("new-1"), root.join("repo/.worktrees/known-1"));
        std::fs::create_dir_all(known.join(".git")).unwrap();
        // Not a tree wsp made: nothing is written.
        assert_eq!(trust(crate::cmd_sandbox::dir_of(&store), &root.join("elsewhere").display().to_string()).unwrap(), None);
        assert_eq!(json_of(&file)["projects"].as_object().unwrap().len(), 2);

        let said = trust(crate::cmd_sandbox::dir_of(&store), &new).unwrap().expect("a new tree is trusted");
        assert!(said.contains(&new), "{said}");
        let doc = json_of(&file);
        for k in ["hasTrustDialogAccepted", "hasClaudeMdExternalIncludesApproved", "hasClaudeMdExternalIncludesWarningShown"] {
            assert_eq!(doc["projects"][&new][k], true, "{k}");
        }
        assert_eq!(doc["userID"], "u");
        let backups: Vec<_> = std::fs::read_dir(state_root.join("trust-backups")).unwrap().flatten().collect();
        assert_eq!(backups.len(), 1, "one backup before the first write");
        assert!(!std::fs::read_to_string(backups[0].path()).unwrap().contains("new-1"), "the backup is of the file as it was");
        // A second seat in the same tree writes the same and backs up nothing more.
        trust(crate::cmd_sandbox::dir_of(&store), &new).unwrap().unwrap();
        assert_eq!(std::fs::read_dir(state_root.join("trust-backups")).unwrap().count(), 1);
        // An entry that was there before this sandbox is its owner's.
        assert_eq!(trust(crate::cmd_sandbox::dir_of(&store), &known.display().to_string()).unwrap(), None);
        assert!(json_of(&file)["projects"][known.display().to_string()].get("hasTrustDialogAccepted").is_none());

        let (lines, kept) = clear(&sb, true);
        assert!(!kept && lines.len() == 1 && lines[0].starts_with("would remove"), "{lines:?}");
        assert!(json_of(&file)["projects"].get(&new).is_some(), "-n removes nothing");
        let (lines, kept) = clear(&sb, false);
        assert!(!kept && lines.len() == 1, "{lines:?}");
        let doc = json_of(&file);
        assert!(doc["projects"].get(&new).is_none(), "{doc}");
        assert_eq!(doc["projects"]["/theirs"]["hasTrustDialogAccepted"], true);
        assert_eq!(doc["projects"][known.display().to_string()]["allowedTools"][0], "x");
        assert!(!sb.join(LEDGER).exists());
        // Outside a sandbox nothing is written at all.
        let live = Store::at(root.join("live/wsp"), root.join("live/state"));
        assert_eq!(trust(crate::cmd_sandbox::dir_of(&live), &new).unwrap(), None);
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_trust_dialog_on_a_screen_is_named_with_its_fix_and_no_other_screen_is() {
        let screen = " Accessing workspace:\n\n /Users/e/repo/.worktrees/x-1\n\n Quick safety check\n ❯ 1. Yes, I trust this folder\n   2. No, exit\n";
        let fix = dialog_fix(screen).expect("the dialog");
        assert!(fix.contains("/Users/e/repo/.worktrees/x-1") && fix.contains("wsp sandbox"), "{fix}");
        assert_eq!(dialog_fix("Do you want to proceed? 1. Yes 2. No"), None);
    }
}
