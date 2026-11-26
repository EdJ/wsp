//! The one door between core wsp and a shared-work add-on (the hub's spoke).
//!
//! Core wsp runs alone: a person's rows, lists, seats and runs on their own
//! machine. Work shared with other people is an add-on that plugs in here. Core
//! asks it the few things only it can answer (may this shared row be claimed,
//! who drives this shared list, is this member running on somebody else's
//! machine, is this verb refused because another user holds the row, where does
//! a message for another user go) and never names the add-on's modules.
//!
//! [`SharedWork`] is that interface. Every method has a default that is the
//! standalone answer, so a wsp with nothing installed behaves exactly as a wsp
//! with no login always has: nothing is leased, every list is driven here,
//! nothing is held elsewhere, nothing is refused, no message leaves the
//! machine. The add-on answers the same way for a private row, and whenever it
//! is not logged in or is switched off ([`SharedWork::active`]).
//!
//! Like the other hooks the binary fills (`cmd_checkout::install_engine`,
//! `outputs::install`), the add-on is installed once by `main`, before any
//! verb runs ([`install`]); [`get`] hands back the installed one, or the
//! standalone default. The types that cross ([`Held`], [`Drives`], [`Member`],
//! [`Elsewhere`], [`Outgoing`], [`Incoming`], [`Lent`], [`Hand`]) are plain data
//! with no logic, and none is a type of the hub's wire, so the add-on can live
//! in a crate of its own that depends on this one and not the other way round,
//! and speak to its hub in whatever types it likes.
//!
//! A test that needs an add-on saying something in particular installs one on
//! its own thread for as long as a guard lives ([`scoped`]): tests run in
//! threads of one process, and a process-wide install would leak into the
//! tests beside it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use wsp_store_contract::{Principal, SeatId, Token};

use crate::model::Worklist;
use crate::store::Store;

/// A lease the add-on took for a row: its fencing token and when it runs out,
/// in epoch seconds. Written into the claim by [`SharedWork::stamp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    pub token: Token,
    pub expires: i64,
}

/// Whether the run advances a list on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drives {
    /// Advance it: a private list, a shared one this machine drives, or no
    /// add-on (the standalone case).
    Here,
    /// Another user's machine drives it, or is the one that may.
    Elsewhere(String),
    /// Not advanced this turn, in words: the hub did not answer, or the add-on
    /// is switched off.
    NotYet(String),
}

/// What the run does about one member of a list it drives, before starting it.
#[derive(Debug)]
pub enum Member {
    /// No lease to take: a private row, or no add-on.
    Free,
    /// Leased to this machine's user; the spawn's claim takes it again.
    Leased(Held),
    /// Another user's machine holds it: it runs there.
    Elsewhere(String),
    /// Not started this turn, in words.
    NotYet(String),
}

/// What the add-on knows to be another user's: rows held or kept on another
/// machine, and shared lists with the user who drives each.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Elsewhere {
    pub rows: BTreeSet<String>,
    /// List -> the user it was last read as driven by.
    pub lists: BTreeMap<String, String>,
}

/// A message leaving this machine for another user's: the address it is for
/// (`to`), who sends it (`from`, an address or `wsp`), the sending seat's pane
/// when a seat sends it, the words, and the id of the message it replies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub to: String,
    pub from: String,
    pub seat: Option<String>,
    pub text: String,
    pub re: Option<String>,
}

/// A message another user's machine sent here, as the add-on hands it to
/// `cmd_message::receive`: its id (the add-on's, which a reply names), the
/// address it is for, the user it was routed to (this machine's), the sending
/// user, the sender's address and seat, the words, and what it replies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    pub id: String,
    pub to: String,
    pub user: String,
    pub from_user: String,
    pub from: String,
    pub seat: Option<String>,
    pub text: String,
    pub re: Option<String>,
}

/// How a message from another machine is handed to a seat here: to `seat`, the
/// dressed text, from a principal, under an idempotency key; the order's id,
/// or why not.
pub type Hand<'a> = dyn Fn(&SeatId, &str, &Principal, &str) -> Result<String, String> + 'a;

/// Ending a claim the way `wsp despawn --lost` does: the row, and why.
pub type Release = fn(&Store, &str, &str) -> Result<(), String>;
/// Whether a seat is alive, the census read at most once into the cache given.
pub type SeatAlive = fn(&Store, &str, &mut Option<Option<Vec<String>>>) -> bool;
/// A tell kept for a seat's next idle (the held-tell spool): the seat, the text.
pub type Spool = fn(&Store, &str, &str);

/// What the daemon lends the add-on's background work from the layers above
/// it, which the add-on cannot reach itself.
#[derive(Clone, Copy)]
pub struct Lent {
    pub release: Release,
    pub alive: SeatAlive,
    pub spool: Spool,
}

/// What the shared side says of who this process is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhoAmI {
    /// No login (or no add-on): the caller is the literal `person`.
    NotLoggedIn,
    /// The principal the login names, and the server that said so.
    Known(Principal, String),
    /// A login is there and the hub refused it (revoked, or a token it never
    /// issued), in words.
    Refused(String),
    /// A login is there and the hub did not answer, in words.
    Unanswered(String),
}

/// What a shared-work add-on provides to core. Every default is the standalone
/// answer; see the module's doc.
pub trait SharedWork: Send + Sync {
    /// Whether the add-on is logged in and switched on, so new shared work may
    /// be taken and messages may leave the machine. False standalone.
    fn active(&self, _store: &Store) -> bool {
        false
    }

    /// The user this machine is logged in as on the shared side, if any: what
    /// `primary:<user>` and an allocation are read against.
    fn user(&self) -> Option<String> {
        None
    }

    /// Who the shared side says this process is (the principal its credential
    /// names) and the name of the server that said it, or why it cannot say.
    /// Never reads a seat's own token.
    fn whoami(&self) -> WhoAmI {
        WhoAmI::NotLoggedIn
    }

    /// Whether a new record may be filed into a shared workspace, asked by
    /// `record_workspace` once the flag or the environment named one.
    fn may_file_shared(&self, _store: &Store) -> Result<(), String> {
        Err("this wsp has no hub add-on, so it keeps everything private".into())
    }

    /// The lease a claim (or `wsp start`) on `row` needs, taken before anything
    /// is written: `None` where none is needed, `Err` for a refusal.
    fn before_claim(&self, _store: &Store, _row: &str) -> Result<Option<Held>, String> {
        Ok(None)
    }

    /// Write a lease [`SharedWork::before_claim`] took into the claim record.
    fn stamp(&self, _claim: &mut Value, _held: &Held) {}

    /// Whether a claim record carries the add-on's lease: such a claim's expiry
    /// is the add-on's to renew, never the daemon's.
    fn leased(&self, _claim: &Value) -> bool {
        false
    }

    /// A verb wrote `row` with a new status (`cmd_task::write_task`): a lease
    /// the add-on holds for it with no claim goes back if the work is over.
    fn row_moved(&self, _store: &Store, _row: &str) {}

    /// A claim on `row` is ending: its lease goes back, best effort.
    fn let_go(&self, _store: &Store, _row: &str, _claim: &Value) {}

    /// The other user `row` is allocated to, where it may not be taken here.
    fn allocated_elsewhere(&self, _store: &Store, _row: &str) -> Option<String> {
        None
    }

    /// The gate before the run advances `w`. The add-on says in `cycle.log`
    /// who drives a shared list when that changes.
    fn drive(&self, _store: &Store, _w: &Worklist) -> Drives {
        Drives::Here
    }

    /// Who `worklist show` says drives `w`; `None` for a list that is only
    /// ever this machine's.
    fn driven_by(&self, _store: &Store, _w: &Worklist) -> Option<String> {
        None
    }

    /// The driver leases `row` before starting it.
    fn lease_member(&self, _store: &Store, _row: &str) -> Member {
        Member::Free
    }

    /// A member lease the run took and did not use goes back at once.
    fn unused(&self, _store: &Store, _row: &str, _held: &Held) {}

    /// The user whose machine runs `row`, while it is not done.
    fn running_elsewhere(&self, _store: &Store, _row: &str) -> Option<String> {
        None
    }

    /// Whether the barrier may count `row` verified by a verdict its holder's
    /// machine sent.
    fn verdict_held(&self, _store: &Store, _row: &str) -> bool {
        false
    }

    /// Why `verb` (`done`, `review`, `submit`, `verified`, `land`) is refused on
    /// `row` here, if it is.
    fn refusal(&self, _store: &Store, _row: &str, _verb: &str) -> Option<String> {
        None
    }

    /// Bring `row` whole where only a stub of it is kept here (`wsp show
    /// --fetch`): `None` for a row there is nothing to fetch for.
    fn fetch(&self, _store: &Store, _row: &str) -> Option<Result<(), String>> {
        None
    }

    /// Lines `wsp show` prints about `row` under the label `hub`.
    fn show_lines(&self, _store: &Store, _row: &str) -> Vec<String> {
        Vec::new()
    }

    /// The rows and lists that are another user's.
    fn elsewhere(&self, _store: &Store) -> Elsewhere {
        Elsewhere::default()
    }

    /// Send a message to another machine: the id it was given and the user it
    /// was routed to, or why not.
    fn send(&self, _store: &Store, _msg: Outgoing) -> Result<(String, String), String> {
        Err("this wsp has no hub to carry a message off this machine".into())
    }

    /// What `wsp doctor` says of the shared side. `alive` says whether a seat
    /// is alive, as the doctor's census read it; `now` is epoch seconds.
    fn health(
        &self,
        _store: &Store,
        _now: i64,
        _alive: &mut dyn FnMut(&str) -> bool,
        _problems: &mut Vec<String>,
        _warnings: &mut Vec<String>,
        _notes: &mut Vec<String>,
    ) {
    }

    /// The daemon's background work for the add-on, started once with what the
    /// daemon lends it. `None` when there is nothing to run.
    fn start(&self, _store: &Store, _lent: Lent) -> Option<std::thread::JoinHandle<()>> {
        None
    }
}

/// The add-on with nothing in it: every answer the standalone one.
pub struct Standalone;

impl SharedWork for Standalone {}

static INSTALLED: OnceLock<Arc<dyn SharedWork>> = OnceLock::new();

/// Install the add-on for this process. The first install stands; `main` makes
/// it before any verb runs.
pub fn install(addon: Arc<dyn SharedWork>) {
    let _ = INSTALLED.set(addon);
}

/// The add-on: one a test put on this thread ([`scoped`]), else the installed
/// one, else [`Standalone`].
pub fn get() -> Arc<dyn SharedWork> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(a) = SCOPED.with(|s| s.borrow().clone()) {
        return a;
    }
    match INSTALLED.get() {
        Some(a) => a.clone(),
        None => standalone(),
    }
}

fn standalone() -> Arc<dyn SharedWork> {
    static STANDALONE: OnceLock<Arc<dyn SharedWork>> = OnceLock::new();
    STANDALONE.get_or_init(|| Arc::new(Standalone)).clone()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static SCOPED: std::cell::RefCell<Option<Arc<dyn SharedWork>>> = const { std::cell::RefCell::new(None) };
}

/// While the guard lives, [`get`] on this thread answers `addon`; dropped, the
/// one before it is back.
#[cfg(any(test, feature = "test-support"))]
pub fn scoped(addon: Arc<dyn SharedWork>) -> Scoped {
    let was = SCOPED.with(|s| s.borrow_mut().replace(addon));
    Scoped(was)
}

#[cfg(any(test, feature = "test-support"))]
pub struct Scoped(Option<Arc<dyn SharedWork>>);

#[cfg(any(test, feature = "test-support"))]
impl Drop for Scoped {
    fn drop(&mut self) {
        let was = self.0.take();
        SCOPED.with(|s| *s.borrow_mut() = was);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Refuses;

    impl SharedWork for Refuses {
        fn refusal(&self, _: &Store, row: &str, verb: &str) -> Option<String> {
            Some(format!("{verb} on {row} is refused"))
        }
    }

    #[test]
    fn addon_nothing_installed_on_this_thread_answers_as_standalone_and_a_scoped_one_is_put_back() {
        let (_env, store) = crate::testkit::isolated_store("addon-scoped");
        let before = get().refusal(&store, "t-1", "done");
        {
            let _a = scoped(Arc::new(Refuses));
            assert_eq!(get().refusal(&store, "t-1", "done").as_deref(), Some("done on t-1 is refused"));
            {
                let _b = scoped(Arc::new(Standalone));
                assert_eq!(get().refusal(&store, "t-1", "done"), None, "the inner one answers");
            }
            assert!(get().refusal(&store, "t-1", "done").is_some(), "the outer one is back");
        }
        assert_eq!(get().refusal(&store, "t-1", "done"), before);
        // The standalone answers.
        let s = Standalone;
        assert!(!s.active(&store));
        assert_eq!(s.before_claim(&store, "t-1"), Ok(None));
        assert!(!s.leased(&serde_json::json!({ "hub_token": 3 })));
        assert!(matches!(s.lease_member(&store, "t-1"), Member::Free));
        assert!(s.send(&store, Outgoing { to: "primary:ed".into(), from: "wsp".into(), seat: None, text: "hi".into(), re: None }).is_err());
        assert!(s.may_file_shared(&store).is_err());
        assert_eq!(s.user(), None);
        assert_eq!(s.whoami(), WhoAmI::NotLoggedIn);
    }
}
