//! A paragraph a verb is handed (wsp-438): typed on the line, a stream, or a
//! file named by `--from`, checked for the one thing that says it is not one.
//!
//! Moved down out of `cmd_agent` so `wsp ask`, `wsp answer` and the other
//! message verbs (`cmd_message`) take prose without reaching up into the seats;
//! `cmd_agent` re-exports each, so `wsp tell` and its paths are unchanged.

use crate::base::args::Args;
use crate::util;

/// The same sentence, for the verbs that are not `tell`.
///
/// `wsp ask` and `wsp answer` take prose after an id exactly as this does, and
/// they must take it *identically*: the `-` form, the empty check and the
/// backtick refusal are one behaviour, and a second copy of them is a second
/// set of edge cases. `render-080` is why the refusal exists at all — three
/// phrases were executed by the shell instead of delivered, and the message
/// arrived fluent with the load-bearing nouns missing.
pub fn prose(args: &Args, skip: usize) -> Result<String, i32> {
    from_source(args, &args.text(skip))
}

/// The paragraph, out of the three places a paragraph comes from: typed on the
/// line, a stream, or a file named by `--from`.
///
/// **The file form is `worklist-036` beyond the row it was found on.** `flag`
/// lost the message because it had no `--from` and the parser bound the path to
/// an option nothing read — and `flag` was not alone in the first half of that:
/// `tell`, `ask`, `answer` and `govern --tell` all took `-` and none of them
/// took a file, while the house rule printed in every brief is *give a wsp verb
/// its prose through a file*, because a shell evaluates backticks inside double
/// quotes. Four verbs told agents to use a spelling four verbs did not have.
/// The dropped-word guard in `main` now says so out loud instead of losing it
/// (see [`crate::base::args::Args::dropped`]); this is the other half, which is that the
/// spelling should simply work, and it works the same way on all of them
/// because they are one act and share this function.
///
/// Two sources is refused rather than resolved: `--from` beside a typed
/// sentence is a caller who does not know which one is being sent, and picking
/// either is the same silent loss in a smaller hat.
pub fn from_source(args: &Args, typed: &str) -> Result<String, i32> {
    let Some(path) = args.get("from") else {
        return match typed.trim() == "-" {
            true => piped_message(),
            false => typed_message(typed),
        };
    };
    if !typed.trim().is_empty() && typed.trim() != "-" {
        eprintln!("wsp: a sentence and --from are two messages — send one");
        return Err(2);
    }
    // A lone `--from`, or `--from -`, is the stream: a dash is not a value, it
    // is the conventional name for stdin, and `prose_source` reads it the same
    // way for `note` and its neighbours.
    if matches!(path.as_str(), "true" | "-") {
        return piped_message();
    }
    match crate::util::read_source(&path) {
        Ok(text) if text.trim().is_empty() => {
            eprintln!("wsp: {path} is empty — nothing sent");
            Err(2)
        }
        Ok(text) => Ok(text.trim().to_string()),
        Err(e) => {
            eprintln!("wsp: cannot read {path}: {e}");
            Err(1)
        }
    }
}

/// A message given on the command line, checked for the one thing that says it
/// is not one. See [`crate::util::terminal_output`] — the payload here is prose
/// about the CLI going through a shell, which is the exact shape that comes
/// back carrying a command's output instead of the command's name.
pub fn typed_message(raw: &str) -> Result<String, i32> {
    let text = raw.trim();
    if text.is_empty() {
        eprintln!("wsp: nothing to say");
        return Err(2);
    }
    if let Some(why) = util::terminal_output(text) {
        eprintln!("wsp: {why}");
        eprintln!("     a backtick inside double quotes runs a command. `-` reads the message from a stream, where a shell never sees it");
        return Err(2);
    }
    Ok(text.to_string())
}

/// The message on stdin. Shared with `govern --tell -`, because the two verbs
/// are one act — a sentence typed into an agent's composer — and a stream form
/// that behaved differently on one of them is a stream form nobody trusts.
pub fn piped_message() -> Result<String, i32> {
    if util::stdin_is_tty() {
        // The same failure `wsp note` names: a command that stops and silently
        // swallows keys is worse than one that refuses.
        eprintln!("wsp: nothing is piped in — `-` reads the message from a stream");
        return Err(2);
    }
    match crate::util::read_source("-") {
        Ok(raw) if raw.trim().is_empty() => {
            eprintln!("wsp: nothing on stdin — nothing sent");
            Err(2)
        }
        Ok(raw) => Ok(raw.trim().to_string()),
        Err(e) => {
            eprintln!("wsp: cannot read stdin: {e}");
            Err(1)
        }
    }
}
