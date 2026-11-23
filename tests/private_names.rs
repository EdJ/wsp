//! The privacy lint (wsp-452): no tracked file names a person's machine or its private address.
//!
//! The rule: never name a person's own machines or their addresses in code, comments, docs, commit
//! messages, test fixtures or measurement files. This test reads every tracked file and fails on
//!
//! - **a private address literal**: a full dotted quad in `192.168.0.0/16` or in the tailnet's
//!   `100.64.0.0/10`. A range written as a prefix (`100.64.0.0/10`) is a definition, not an
//!   address, and passes. A test that needs an address in a range builds it from octets.
//! - **a hostname the person lists** in an untracked local file, `~/.config/wsp/private-names`
//!   (or the file `$WSP_PRIVATE_NAMES` names): one name per line, `#` starts a comment, matched
//!   case-insensitively as a substring. The names themselves never enter the repo; with no such
//!   file only the address check runs.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Every full dotted quad in `line` that sits in a private range and is not the head of a prefix.
fn private_addrs(line: &str) -> Vec<String> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() || (i > 0 && (b[i - 1].is_ascii_digit() || b[i - 1] == b'.')) {
            i += 1;
            continue;
        }
        // Read digits and dots from here.
        let mut j = i;
        while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
            j += 1;
        }
        let token = line[i..j].trim_end_matches('.');
        let octets: Vec<&str> = token.split('.').collect();
        let nums: Vec<Option<u8>> = octets.iter().map(|o| o.parse::<u8>().ok()).collect();
        let at_end = i + token.len();
        let is_prefix = b.get(at_end) == Some(&b'/');
        if octets.len() == 4 && nums.iter().all(|n| n.is_some()) && !is_prefix {
            let (a, c) = (nums[0].unwrap(), nums[1].unwrap());
            if (a == 192 && c == 168) || (a == 100 && (64..=127).contains(&c)) {
                out.push(token.to_string());
            }
        }
        i = j;
    }
    out
}

fn names_from(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect()
}

fn private_names() -> Vec<String> {
    let path = match std::env::var_os("WSP_PRIVATE_NAMES") {
        Some(p) => PathBuf::from(p),
        None => match std::env::var_os("HOME") {
            Some(h) => Path::new(&h).join(".config/wsp/private-names"),
            None => return Vec::new(),
        },
    };
    std::fs::read_to_string(path).map(|t| names_from(&t)).unwrap_or_default()
}

/// What is wrong in `text`: one `line N: why` per hit.
fn findings(text: &str, names: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        for a in private_addrs(line) {
            out.push(format!("line {}: private address {a}", n + 1));
        }
        let low = line.to_lowercase();
        if names.iter().any(|name| low.contains(name.as_str())) {
            out.push(format!("line {}: a name from the private-names file", n + 1));
        }
    }
    out
}

#[test]
fn no_tracked_file_names_a_private_address_or_a_listed_machine() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ls = Command::new("git").arg("-C").arg(root).args(["ls-files", "-z"]).output().expect("git ls-files");
    assert!(ls.status.success(), "git ls-files failed");
    let names = private_names();
    let mut bad = Vec::new();
    for f in ls.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()) {
        let rel = String::from_utf8_lossy(f).to_string();
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else { continue };
        for hit in findings(&text, &names) {
            bad.push(format!("{rel}: {hit}"));
        }
    }
    assert!(bad.is_empty(), "private names in tracked files (wsp-452):\n{}", bad.join("\n"));
}

#[test]
fn the_lint_fails_on_a_planted_literal_and_passes_on_a_range_and_on_clean_text() {
    let lan = format!("ssh to 192.{}.0.7 now", 168);
    assert_eq!(private_addrs(&lan), vec![format!("192.{}.0.7", 168)]);
    for second in [64, 100, 127] {
        let tail = format!("host 100.{second}.1.2:22");
        assert_eq!(private_addrs(&tail), vec![format!("100.{second}.1.2")], "{tail}");
    }
    // Outside the ranges, a prefix, a version-like string and a longer dotted run pass.
    for ok in ["8.8.8.8", "100.63.255.255", "100.128.0.1", "10.0.0.1", "100.64.0.0/10", "1.2.3", "v1.100.64.0.9"] {
        assert!(private_addrs(ok).is_empty(), "{ok}");
    }
    assert!(findings("nothing here\n127.0.0.1\n", &[]).is_empty());
}

#[test]
fn a_listed_name_fails_the_lint_whatever_its_case_and_a_comment_line_lists_nothing() {
    let names = names_from("# the person's machines\n\nSomeBox  # the workstation\n");
    assert_eq!(names, vec!["somebox".to_string()]);
    assert_eq!(findings("on SOMEBOX today", &names), vec!["line 1: a name from the private-names file".to_string()]);
    assert!(findings("on another box", &names).is_empty());
}
