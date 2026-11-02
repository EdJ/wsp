//! The attention harness as an agent kind (wsp-330).
//!
//! `~/claude/ai/attention`'s `tiered-nudge` arm ran packets at about 0.4 of the
//! tokens Claude Code spent on the same packet (compounding-034). This is how a
//! row asks for it: `wsp spawn <task> --agent --kind attention`.
//!
//! # A kind that is not in a pane's catalogue
//!
//! herdr starts only the agents in its own catalogue, and `attention` is not
//! one, so this kind runs on the backends that exec the kind's program —
//! compound (the default) and `--headless` — and `--herdr` is refused before a
//! seat is opened ([`crate::cmd_spawn`]).
//!
//! The program is not `attention` itself but `wsp attend <task>`
//! ([`Kind::program`]). The harness is a loop that answers once and exits; what
//! an unattended attempt needs *after* it exits — the row's checks, the commit,
//! the land, the review or the block, and the attempt's tokens and dollars on
//! the log — is wsp's, and a wrapper is the one place that sees both ends of it.
//! See [`crate::cmd_attend`].
//!
//! # What the kind says it cannot do
//!
//! - **Be told anything.** The order goes in once, as the packet. A sentence
//!   typed at the seat reaches a process that is not reading it, so
//!   [`Kind::tell`] refuses, and `queue_is_the_agents` is false: nothing holds
//!   a wake for it.
//! - **Be brief by hook or file.** The packet is the whole brief, inlined by the
//!   wrapper; the wsp brief tells an agent to run verbs the harness denies it.
//! - **Take an effort.** The harness has no such knob.
//!
//! # What an attempt is read off
//!
//! The wrapper gives each attempt a log directory under
//! `<state>/attention/<session>/` and teaches the claim that name as the
//! session ([`crate::cmd_agent::learn_sessions`]), so [`Kind::ran`] at release
//! is a read of two files the harness already writes: `conversation.json`
//! (model, dollars at list price) and the `*.log.json` (one entry per call,
//! tokens in and out). A call's `in` is its whole prompt, cached or not, so
//! `Spend::input` is an overstatement of fresh input and `cache` is zero; the
//! price is the harness's own and is the figure to compare.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent_commands::{Address, Kind, Ran, Spawn, Spend};
use crate::place::{Delivery, Place, Refusal, Result, Seat};

pub struct Attention;

/// Where the harness's models are named: `api/haiku`, `claude/haiku`,
/// `zen/<model>`, `scripted`. Its own prefixes, checked here because a typo
/// would otherwise start an unattended run that fails on its first call.
const MODEL_PREFIXES: &[&str] = &["api/", "claude/", "zen/", "scripted"];

impl Kind for Attention {
    /// `attend <task>`, and the model when one was asked for. The order is not
    /// here: the wrapper builds the message from the row's packet.
    fn args(&self, spawn: &Spawn) -> Vec<String> {
        let mut argv = vec!["attend".to_string(), spawn.name.to_string()];
        if let Some(model) = spawn.model {
            argv.push("--model".into());
            argv.push(model.to_string());
        }
        argv
    }

    fn program(&self) -> Option<&'static str> {
        Some("wsp")
    }

    fn tier(&self, model: Option<&str>, effort: Option<&str>) -> std::result::Result<(), String> {
        if effort.is_some() {
            return Err("the attention harness has no effort — leave `--effort` off".into());
        }
        match model {
            Some(m) if !MODEL_PREFIXES.iter().any(|p| m.starts_with(p)) => Err(format!(
                "no model `{m}` — the attention harness names models `api/…`, `claude/…`, `zen/…` or `scripted`"
            )),
            _ => Ok(()),
        }
    }

    fn address(&self, _place: &dyn Place, _spawn: &Spawn) -> Option<Address> {
        None
    }

    fn tell(&self, _place: &dyn Place, seat: &Seat, _text: &str) -> Result<Delivery> {
        Err(Refusal::Backend(format!(
            "{seat} is an attention run: it takes its packet once and nothing can be told to it. \
             `wsp despawn` it and respawn the row to change the order"
        )))
    }

    /// The order is taken from the row, not from argv — but it must count as
    /// delivered, or `spawn` would type it at a seat nobody is reading.
    fn order_in_args(&self) -> bool {
        true
    }

    fn queue_is_the_agents(&self) -> bool {
        false
    }

    fn running(&self, _spawn: &Spawn) -> Option<bool> {
        None
    }

    fn ran(&self, session: &str, _cwd: &str, _since: i64) -> Option<Ran> {
        read_attempt(&attempt_dir(session))
    }
}

/// `<state>/attention`, where every attempt's directory is.
pub fn root() -> PathBuf {
    match std::env::var_os("WSP_STATE") {
        Some(v) => PathBuf::from(v),
        None => crate::store::Store::default_state(),
    }
    .join("attention")
}

/// One attempt's directory. A session name is one path component, so a name
/// that carries a separator reads nothing rather than somewhere else.
pub fn attempt_dir(session: &str) -> PathBuf {
    match session.contains('/') || session.contains("..") {
        true => root().join("_refused"),
        false => root().join(session),
    }
}

/// What an attempt consumed, off the directory the harness logged into.
/// `None` when it logged nothing, which is an attempt that never reached a
/// call and is not a zero.
fn read_attempt(dir: &Path) -> Option<Ran> {
    let conversation: Value = serde_json::from_slice(&std::fs::read(dir.join("conversation.json")).ok()?).ok()?;
    let log = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.to_string_lossy().ends_with(".log.json"))
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let calls: Vec<&Value> =
        log.as_ref().and_then(Value::as_array).map(|a| a.iter().filter(|e| e.get("in").is_some()).collect()).unwrap_or_default();
    let tokens = |key: &str| -> Vec<u64> { calls.iter().filter_map(|e| e.get(key).and_then(Value::as_u64)).collect() };
    let (ins, outs) = (tokens("in"), tokens("out"));
    let micros = conversation.get("cost").and_then(Value::as_f64).map(|d| (d * 1_000_000.0).round() as u64);
    let model = conversation.get("model").and_then(Value::as_str).map(|m| m.trim_start_matches("claude-").to_string());
    Some(Ran {
        models: model.into_iter().collect(),
        efforts: Vec::new(),
        turns: calls.len(),
        peak: ins.iter().copied().max().unwrap_or(0),
        spend: Spend { cache: 0, input: ins.iter().sum(), output: outs.iter().sum(), micros },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-attention-{}-{}", std::process::id(), files.len()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in files {
            std::fs::write(dir.join(name), body).unwrap();
        }
        dir
    }

    #[test]
    fn an_attempt_is_read_as_turns_tokens_and_the_harnesss_own_price() {
        let dir = attempt(&[
            ("conversation.json", r#"{"arm":"tiered-nudge","model":"claude-haiku-5-5","cost":0.0125,"messages":[]}"#),
            (
                "chat-tiered-nudge-0.log.json",
                r#"[{"turn":0,"in":1600,"out":190},{"turn":0,"tool":"bash","out":"x"},{"turn":1,"in":3100,"out":230}]"#,
            ),
        ]);
        let ran = read_attempt(&dir).unwrap();
        assert_eq!(ran.turns, 2, "a tool result is not a call");
        assert_eq!(ran.peak, 3100);
        assert_eq!(ran.models, vec!["haiku-5-5".to_string()]);
        assert_eq!((ran.spend.input, ran.spend.output, ran.spend.micros), (4700, 420, Some(12_500)));
    }

    #[test]
    fn an_attempt_that_logged_nothing_is_unmeasured_not_zero() {
        assert!(read_attempt(&attempt(&[])).is_none());
    }

    #[test]
    fn a_session_name_cannot_point_outside_the_attempts_directory() {
        assert_eq!(attempt_dir("../../etc").file_name().unwrap(), "_refused");
        assert_eq!(attempt_dir("wsp-330-17").file_name().unwrap(), "wsp-330-17");
    }

    #[test]
    fn the_harness_takes_its_own_model_names_and_no_effort() {
        assert!(Attention.tier(Some("api/haiku"), None).is_ok());
        assert!(Attention.tier(None, None).is_ok());
        assert!(Attention.tier(Some("opus"), None).unwrap_err().contains("api/"));
        assert!(Attention.tier(None, Some("high")).is_err());
    }
}
