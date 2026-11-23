//! `wsp-hub serve --config <file>`: the hub (wsp-460), a separate minimal
//! service: objects, refs and leases, and the API's connection, hello and
//! subscribe. It runs no agents and builds nothing. See the library's
//! documentation and `config` for the file.

use std::path::PathBuf;
use std::process::ExitCode;

use wsp_hub::{serve, Config};

const USAGE: &str = "usage: wsp-hub serve --config <file>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["serve", "--config", file] => PathBuf::from(file),
        ["--help"] | ["-h"] => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let cfg = match Config::load(&config) {
        Ok(cfg) => cfg,
        Err(why) => {
            eprintln!("wsp-hub: {why}");
            return ExitCode::from(2);
        }
    };
    let running = match serve::start(&cfg) {
        Ok(r) => r,
        Err(why) => {
            eprintln!("wsp-hub: {why}");
            return ExitCode::FAILURE;
        }
    };
    let addrs: Vec<String> = running.addrs().iter().map(ToString::to_string).collect();
    // One line a supervisor or a test can wait for; the token is a file, never printed.
    println!("wsp-hub: listening on {}; bootstrap token in {}", addrs.join(" "), running.token_path().display());
    // Serves until it is killed: the store is transactional and every object is
    // renamed into place, so an end at any moment leaves it whole.
    loop {
        std::thread::park();
    }
}
