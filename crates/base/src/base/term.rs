//! The terminal, asked through `stty`.

use std::fs::File;
use std::process::Stdio;
use crate::calls::Command;

pub fn stty(args: &[&str]) {
    if let Ok(tty) = File::open("/dev/tty") {
        let _ = Command::new("stty")
            .args(args)
            .stdin(Stdio::from(tty))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
