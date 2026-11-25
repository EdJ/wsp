//! The governors' records, read: a seat resolved off its record, who answers for
//! a piece of work, whether a seat is vacant or stood down, whether a caller
//! governs. Moved out of `cmd_govern` (wsp-403) so the store and the engine can ask
//! these without reaching up into the seat commands; `cmd_govern` re-exports each.
//! The record's own writes (vacate, stand down, the reseat's count, claim and
//! failure) and the reads of the last seat moved here too (wsp-437), so the run's
//! repair and stand-down keep a seat's record without the seat commands.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::resolve::Index;
use crate::store::Store;
use crate::util;

/// A seat, resolved: which scope it governs and where it is sitting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    /// The scope the record is filed under — a project id or a worklist slug —
    /// and it is the scope *governed*, not necessarily the one that was asked
    /// about. [`seat_for`] walks, so a flag on a `data` task can resolve to the
    /// `wsp` seat and a flag on a member of tonight's list to the list's, and
    /// the reader wants to be told which.
    pub scope: String,
    /// The pane the agent was in when it took the seat, and the *exact* half of
    /// the record where the governor record's `workspace` key is the durable
    /// one — kept on the record, off this struct
    /// (`compound-096`; see [`room_of`]).
    ///
    /// No longer display-only, which is worklist-035 — [`governs`] carries the
    /// argument. It can still go stale, and what answers that is
    /// [`crate::cmd_agent::reconcile`] vacating a slot whose pane backend no
    /// longer lists, plus the re-take `wsp resume` and a custodian's own
    /// `wsp govern` both perform. Empty where the record was written for a room
    /// the process was not standing in (`wsp govern -w`).
    pub pane: String,
    pub since: String,
    /// The session the custodian is running under, learned from the backend
    /// after the fact by [`learn_seats`] — never written when the seat is
    /// taken, because at that instant there is a shell in the room and no
    /// agent. Empty until something has seen one.
    ///
    /// This field is `render-061`, and the reason it is on the *seat* rather
    /// than reachable through a binding is the finding that task was filed on:
    /// a binding is written per claim, and a custodian is the one kind of agent
    /// that deliberately holds no task. Checked on the machine on 2026-08-18
    /// with two governors up for a day — `bindings.json` was `{}`, and the two
    /// panes that had survived longest were the two that could not be resumed.
    pub session: String,
    /// Where that session was running, which is what a resume has to be
    /// standing in for the transcript to mean anything. Learned with the
    /// session and from the same reading.
    pub cwd: String,
    /// What kind of agent it is — `claude`, `opencode` — because what a resume
    /// *is* differs by kind and only the kind knows how to say it. Learned with
    /// the session, from the same reading and under the same rules; see
    /// [`learn_seats`]. Empty until something has seen one, which
    /// [`crate::cmd_spawn::kind_or_default`] reads as the default.
    pub kind: String,
    /// Nobody is in it yet, and wsp is filling it: the post a running list's
    /// members are answered from while its successor is on the way, as
    /// [`seat_for`] hands it back (`wsp-216`). What is said to it waits in its
    /// spool; there is no pane or room to name, and nobody to *be* it.
    pub vacant: bool,
}

/// One record, if it belongs to this machine.
///
/// A workspace id is backend's and means nothing on another host — the same
/// reason a claim and a mandate each carry one. A seat on another machine is
/// not a seat you can reach, so it reads as no seat rather than as a wrong one.
/// The seat on a scope, from the governors record.
///
/// The wake path's entry to the same resolution `--tell` takes: a scope names a
/// post, a post names a workspace and a pane, and only then is there anything
/// to say a sentence to.
pub fn seat_of_scope(scope: &str, governors: &BTreeMap<String, Value>) -> Option<Seat> {
    governors.get(scope).and_then(|rec| seat_of(scope, rec))
}

pub fn seat_of(scope: &str, rec: &Value) -> Option<Seat> {
    let host = rec.get("host").and_then(Value::as_str).unwrap_or("");
    if !host.is_empty() && host != util::hostname() {
        return None;
    }
    // A record naming no workspace names no seat. `govern` never writes one,
    // so this is a hand-edited or half-written file — and a seat you cannot
    // reach must read as absent rather than as somewhere.
    let workspace = rec.get("workspace").and_then(Value::as_str).unwrap_or_default();
    if workspace.is_empty() {
        return None;
    }
    Some(Seat {
        scope: scope.to_string(),
        pane: rec.get("pane").and_then(Value::as_str).unwrap_or_default().to_string(),
        since: rec.get("since").and_then(Value::as_str).unwrap_or_default().to_string(),
        session: str_at(rec, "session"),
        cwd: str_at(rec, "cwd"),
        kind: str_at(rec, "kind"),
        vacant: false,
    })
}

pub fn str_at(rec: &Value, key: &str) -> String {
    rec.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// The seat responsible for a piece of work: the running worklist it is in, or
/// the nearest seat above its project.
///
/// The escalation the task asked about, and it is a walk rather than a policy.
/// `robustness` has a seat and `wsp` has a seat: a hand raised in `robustness`
/// reaches the first, and the same hand raised while that seat is away reaches
/// the second. Nothing decides to escalate — the walk simply does not stop at a
/// level that has nobody in it, which is also why standing down needs no
/// hand-over. **The worklist step obeys that same rule and is not an exception
/// to it**: a list with no seat, or one whose agent has stood down, falls
/// through to the project chain rather than routing to nobody.
///
/// `list` is the front of the walk — the *running* worklist this task is a
/// member of, and `None` for the ordinary state where nothing is running. It
/// is a parameter rather than something read here because the rule is a map
/// lookup repeated over keys in priority order, and holds no store: one read of
/// `worklists/` ([`crate::worklist::Running`]) serves every question a command
/// asks, where a store read inside this would repeat it per raised hand.
///
/// **A list's post with a successor on the way is not a level with nobody in
/// it** (`wsp-216`, settled by the governor over it). The reconciler refills a
/// running list's post, so a member's ask made while it does waits on the
/// list's scope, as the run's own tells do (`wsp-202`), and comes back
/// [`Seat::vacant`]. It used to fall through to the project's seat, which a
/// self-run list does not answer to. Only where nobody is coming — the post
/// stood down, or its reseat failed and backed off ([`Vacancy::backed_off`]) —
/// does the walk go on up. A list with no record at all has no post to wait
/// on, which is a list run by hand from its project's seat; a list wsp runs
/// has one from the reconciler's first tick after `go`.
///
/// **Past the project levels, an allocated row's walk ends at its user's
/// primary** (`wsp-418`). `allocated_to` is the user the row is allocated to
/// (`Store::allocated_to`, wsp-414), `None` for an unallocated row, which
/// walks exactly as it did and ends at `None` — the person. For an allocated
/// row with no list or project seat answering, the answer is `primary:<user>`
/// ([`primary_of`]) when the primary is seated, carrying its occupant. On a
/// spoke the local primary *is* its user's primary (wsp-417), and the store
/// holds no login, so whether `<user>` is this spoke's own user — delivered
/// locally — or another's — routed through the hub — is the delivering
/// caller's to ask. A seat named `primary:<user>` in the map is that user's
/// primary first; otherwise the local [`PRIMARY`] slot is.
pub fn seat_for(
    governors: &BTreeMap<String, Value>,
    index: &Index,
    list: Option<&str>,
    project: Option<&str>,
    allocated_to: Option<&str>,
) -> Option<Seat> {
    seat_for_as(governors, index, list, project, allocated_to, None)
}

/// [`seat_for`] with the user logged in on this spoke, which the store does not
/// hold (`wsp-485`). `login` is `None` for a standalone wsp or a caller that
/// does not know, and the walk is then [`seat_for`]'s. With a login, an
/// allocated row whose user is *another's* ends at `primary:<user>` whether or
/// not a primary is seated here — the seat is theirs, on their wsp, and
/// delivery routes it through the hub ([`user_primary_seat_as`]).
pub fn seat_for_as(
    governors: &BTreeMap<String, Value>,
    index: &Index,
    list: Option<&str>,
    project: Option<&str>,
    allocated_to: Option<&str>,
    login: Option<&str>,
) -> Option<Seat> {
    if governors.is_empty() && !is_elsewhere(allocated_to, login) {
        return None;
    }
    if let Some(list) = list {
        if let Some(rec) = governors.get(list) {
            if let Some(seat) = seat_of(list, rec) {
                return Some(seat);
            }
            if being_filled(governors, list) {
                return Some(Seat {
                    scope: list.to_string(),
                    pane: String::new(),
                    since: String::new(),
                    session: String::new(),
                    cwd: String::new(),
                    kind: String::new(),
                    vacant: true,
                });
            }
        }
    }
    project
        .into_iter()
        .flat_map(|p| std::iter::once(p.to_string()).chain(index.ancestors(p)))
        .find_map(|s| governors.get(&s).and_then(|rec| seat_of(&s, rec)))
        .or_else(|| user_primary_seat_as(governors, allocated_to?, login))
}

/// Whether an allocation names a user other than the one logged in here.
fn is_elsewhere(allocated_to: Option<&str>, login: Option<&str>) -> bool {
    match (allocated_to.map(str::trim), login.map(str::trim)) {
        (Some(u), Some(me)) => !u.is_empty() && !me.is_empty() && u != me,
        _ => false,
    }
}

/// The seat `primary:<user>` names, if seated: a record under that key, else
/// the local [`PRIMARY`]'s occupant under the user's name — the step
/// [`seat_for`] takes past the project levels for an allocated row.
pub fn user_primary_seat(governors: &BTreeMap<String, Value>, user: &str) -> Option<Seat> {
    user_primary_seat_as(governors, user, None)
}

/// [`user_primary_seat`] knowing who is logged in here. For a user who is not
/// the login, the seat is `primary:<user>` whether or not a local primary is
/// seated — the local one is *this* user's, never theirs — with no pane, so
/// that what is said to it goes through the hub. For the login's own user, or
/// with no login, it is [`user_primary_seat`]'s answer.
pub fn user_primary_seat_as(governors: &BTreeMap<String, Value>, user: &str, login: Option<&str>) -> Option<Seat> {
    let user = user.trim();
    if user.is_empty() {
        return None;
    }
    if is_elsewhere(Some(user), login) {
        let key = primary_of(user);
        if let Some(seat) = governors.get(&key).and_then(|rec| seat_of(&key, rec)) {
            return Some(seat);
        }
        return Some(Seat {
            scope: key,
            pane: String::new(),
            since: String::new(),
            session: String::new(),
            cwd: String::new(),
            kind: String::new(),
            vacant: false,
        });
    }
    let key = primary_of(user);
    if let Some(seat) = governors.get(&key).and_then(|rec| seat_of(&key, rec)) {
        return Some(seat);
    }
    let seat = governors.get(PRIMARY).and_then(|rec| seat_of(PRIMARY, rec))?;
    Some(Seat { scope: key, ..seat })
}

/// Whether wsp is coming to fill this post, read off its record: nobody in it
/// on this machine, not stood down, and no reseat that failed and backed off.
///
/// The record's half of the question only. Whether the *list* is still one the
/// reconciler seats — running, held or parked, not done — is the caller's:
/// [`seat_for`] is handed running lists alone, and the cycle asks it of the
/// list in hand ([`crate::cycle::nobody_coming`]).
pub fn being_filled(governors: &BTreeMap<String, Value>, scope: &str) -> bool {
    let Some(rec) = governors.get(scope) else { return false };
    let host = str_at(rec, "host");
    if !host.is_empty() && host != util::hostname() {
        return false;
    }
    seat_of(scope, rec).is_none() && stood_at(governors, scope).is_none() && !Vacancy::of(rec).backed_off()
}

/// The same walk, started **one step past** a scope that cannot answer for
/// itself.
///
/// [`seat_for`] stops at the first seat it finds, which is the right answer to
/// *who answers for this work* and the wrong one to *who hears that this seat
/// has stopped*: the seat it reaches is the one the news is about. So this is
/// the chain with that scope's own step removed, terminating — like every other
/// address in this tree — at [`crate::cmd_watch::EVERYONE`] when nothing above
/// is filled. `worklist-041`.
///
/// **A list's seat answers to its home project's governor** — `wsp-263`, Ed,
/// 2026-10-09, reversing what this said before: that a list has nothing above
/// it because a list cutting across projects has as many project chains as it
/// has members. It has one *home* ([`crate::worklist::home`]): stated, or the
/// project its members share, or the one it was made in. So `home` is the
/// list's step past itself, and the walk goes on up that project's ancestors
/// exactly as a project scope's does. A list with no home still has nothing
/// above it, and escalates to everybody.
///
/// `home` is a parameter, `None` for a project scope, for the reason
/// [`seat_for`] takes `list`: this holds no store, and a caller that has the
/// lists and tasks in hand already ([`crate::worklist::homes`]) asks once.
/// [`seat_above_in`] is the walk with the reads done for it.
///
/// Members' own questions are unchanged: [`seat_for`] still asks the list's
/// seat first. This is only where the list's seat itself is answered for.
pub fn seat_above(governors: &BTreeMap<String, Value>, index: &Index, scope: &str, home: Option<&str>) -> Option<Seat> {
    if governors.is_empty() {
        return None;
    }
    let walk = match home {
        Some(h) => std::iter::once(h.to_string()).chain(index.ancestors(h)).collect(),
        None => index.ancestors(scope),
    };
    walk.into_iter().find_map(|s| governors.get(&s).and_then(|rec| seat_of(&s, rec)))
}

/// The scope of the primary interface (`wsp-234`, built by `wsp-279`): the one
/// standing seat that is no project's, where the person talks to the fleet.
///
/// **A slot in `governors.json` like any other, and not a project or a list.**
/// That is what makes it a seat and not a session the window owns (compound-506):
/// it is on the census, `wsp govern` lists it, `--tell` reaches it, it outlives
/// the window, and `reconcile` vacates it when its pane goes the way it vacates
/// any slot. What it is *not* follows from the key being neither a project id nor
/// a worklist slug: [`seat_for`] walks lists and projects, so no row's question
/// ever routes to it and no seat's `answers_to` names it — the person is still
/// the governor of last resort, and this is only where the person reads it.
///
/// So the name is reserved in the one key space (`Store::scope_taken`, `project
/// add`), and the readers that would otherwise treat it as a governor ask
/// [`is_primary`]: `wip`'s role, the brief, the work order, the ask's routing, and
/// the reconciler, which never refills it (README, *Does the daemon refill it
/// unprompted?*).
pub const PRIMARY: &str = "primary";

/// Whether a scope is the primary interface's — one spelling, so a reader that
/// special-cases it cannot drift from the constant.
pub fn is_primary(scope: &str) -> bool {
    scope == PRIMARY
}

/// The prefix of a user's primary, `primary:<user>` (`wsp-417`). Reserved in the
/// scope key space beside [`PRIMARY`]: no project, list or slug may start with it.
pub const USER_PRIMARY_PREFIX: &str = "primary:";

/// The name a user's primary goes by: `primary:<user>`.
///
/// On a spoke the local primary *is* its user's primary, so this is an address
/// and not a second slot in `governors.json`; [`is_primary`] stays true for the
/// bare `primary` only, which is what its readers meant by it.
pub fn primary_of(user: &str) -> String {
    format!("{USER_PRIMARY_PREFIX}{}", user.trim())
}

/// The user a `primary:<user>` names, or `None` for any other scope — the bare
/// `primary` included, and an empty user.
pub fn user_of_primary(scope: &str) -> Option<&str> {
    let user = scope.strip_prefix(USER_PRIMARY_PREFIX)?.trim();
    (!user.is_empty()).then_some(user)
}

/// Whether a scope is in the reserved `primary` space: the bare word, or
/// anything under `primary:`. What the key space refuses a project or a list.
pub fn is_primary_key(scope: &str) -> bool {
    is_primary(scope) || scope.starts_with(USER_PRIMARY_PREFIX)
}

/// When a person stood this seat down, if they did.
///
/// **The marker that makes `--clear` mean something to the reconciler, and it is
/// one field because `vacate` used to carry two meanings at once.** `--clear`
/// and `reconcile` both emptied the record, and the record they left was
/// *identical*, so the reconciler could not tell a governor that had died — which
/// is the one it is there to replace — from a governor a person had deliberately
/// stood down. It read the second as the first and refilled it. On 2026-10-05
/// that put `cpd-275` back into a `tooling` seat Ed had closed by hand twenty
/// minutes earlier, two ticks after the close.
///
/// **Absent is the ordinary case and is not a default to fill in.** A record
/// without the key is a seat nobody has ever stood down, which is every record
/// written before this and every seat taken since — so a store full of them is
/// unaffected and the reconciler keeps doing its job.
///
/// A date and not a boolean, because the roster has to say *which* — a seat
/// nobody is in is either a governor that died or a position somebody decided to
/// leave empty, and those are different sentences to a person reading it.
pub fn stood_at(governors: &BTreeMap<String, Value>, scope: &str) -> Option<i64> {
    let at = str_at(governors.get(scope)?, STOOD_DOWN);
    let at = util::epoch_of(&at);
    (at > 0).then_some(at)
}

/// The key a stand-down is recorded under. `STOOD_DOWN` is spelled here rather
/// than inlined because three files now read or write it and a typo in a record
/// is silent — the reconciler would simply never see the decision.
pub const STOOD_DOWN: &str = "stood_down";

/// One ISO timestamp off a record, as an epoch — and `None` for a key that is
/// absent, empty or unparseable, which are the same answer.
///
/// **Shared because three markers now carry dates and a reader that accepted a
/// different subset of those three would be a reader whose idea of "claimed" is
/// not the reconciler's.** A hand-edited `0` and a hand-edited `"soon"` both mean
/// nothing here, and both mean the claim is not held.
pub fn at_of(rec: &Value, key: &str) -> Option<i64> {
    let at = util::epoch_of(&str_at(rec, key));
    (at > 0).then_some(at)
}

/// How long this seat has been standing empty, in ticks, and whether a reseat
/// is already under way.
///
/// **Two fields and not one, because they answer two different questions.** The
/// count is the reconciler's evidence — a seat that reads `Empty` on one pass is
/// a pane between states, and one that reads it on three is a dead governor on a
/// running list. The claim is the guard: `wsp-148` requires the record to be
/// written *before* the spawn so a repeated tick cannot seat a second successor,
/// and a record that only counted would say `2` again on the tick after the
/// spawn as readily as on the tick before it.
///
/// The count is on the record and not in [`crate::repair::Pass`] because that
/// struct is in memory: the daemon `exec`s itself when an install lands
/// underneath it (`daemon::reload`), which throws the count away at exactly the
/// moment a person is most likely to be installing — mid-run, with a governor
/// dead. A counter that resets would never reach two ticks.
///
/// The claim is dated and read back through [`SEAT_CLAIMED_FOR`] rather than
/// being a bare boolean, because a bare one survives a process that was killed
/// mid-reseat — and the failure this row was pointed at by name is a rotate that
/// left a successor nobody could reach behind. A claim is taken back when the
/// reseat finishes, and an abandoned one is only honoured for this long.
/// When a reseat of this seat failed, if one did.
///
/// **A second field beside the claim, and the reason the claim is not simply
/// given back on failure.** Giving it back made the very next pass try again,
/// which is right for a failure that was a moment's bad luck and catastrophic
/// for one that is structural: on 2026-10-05 `tokenhub-spec-sync` was reseated
/// once a minute for ever, each attempt opening a compound agent whose renderer
/// never came up, and the seat empty at the end of every one. The claim is the
/// bound on that — [`SEAT_CLAIMED_FOR`] between attempts — so a failure has to
/// *keep* it and say why, and this is the why.
///
/// `wsp watch --status` reads it too, because the alternative was a sentence
/// claiming a governor was on its way for twenty minutes after the process that
/// was going to seat it had already said it could not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Vacancy {
    /// Consecutive passes that have read this seat `Empty` or `Gone`.
    pub unseated: u64,
    /// When a reseat was claimed, if one is in flight.
    pub reseating: Option<i64>,
    /// When the reseat under way last failed, if it did.
    pub failed: Option<i64>,
}

/// How long a claim on a reseat is honoured before the next pass takes it back.
///
/// Twenty minutes, and it is a bound on a *dead* process rather than a wait: a
/// reseat that is running holds this for as long as the spawn takes, and one
/// that was killed holding it holds it for ever. Both are answered by a number
/// that is long past any real spawn and short enough that a run does not sit
/// unseated for the rest of the night.
pub const SEAT_CLAIMED_FOR: i64 = 20 * 60;

/// Read the three markers off a record.
pub fn vacancy(governors: &BTreeMap<String, Value>, scope: &str) -> Vacancy {
    governors.get(scope).map(Vacancy::of).unwrap_or_default()
}

impl Vacancy {
    /// The markers as one record carries them. `None` is a record with none of
    /// them, which is every record written before `wsp-148`.
    pub fn of(rec: &Value) -> Vacancy {
        Vacancy {
            unseated: rec.get("unseated").and_then(Value::as_u64).unwrap_or(0),
            reseating: at_of(rec, "reseating"),
            failed: at_of(rec, "reseating_failed"),
        }
    }

    /// The last reseat failed and nothing has been tried since: the claim
    /// [`reseat_failed`] kept is the one that failed, so nobody is coming
    /// until it goes stale and a pass takes it again. A claim taken after the
    /// failure is a retry in flight, and somebody is coming again.
    pub fn backed_off(&self) -> bool {
        self.failed.is_some_and(|failed| self.reseating.is_none_or(|claimed| failed >= claimed))
    }

    /// Whether the claim, if there is one, has gone stale.
    pub fn claim_stale(&self, at: i64) -> bool {
        self.reseating.is_some_and(|since| since + SEAT_CLAIMED_FOR <= at)
    }

    /// Whether a reseat may be claimed now — nothing is claimed, or what is was
    /// claimed long enough ago that whatever took it is gone.
    pub fn claim_free(&self, at: i64) -> bool {
        self.reseating.is_none_or(|_| self.claim_stale(at))
    }

    /// Whether this seat has been empty long enough to replace. `n` ticks, which
    /// is the whole of the delay this row buys and the reason it is not zero: a
    /// pane reads `Empty` for a moment every time an agent is cleared and
    /// restarted, and a successor seated into that window is two governors for
    /// one run.
    pub fn overdue(&self, n: u64) -> bool {
        self.unseated >= n
    }
}

/// The last agent to hold this slot, filled or not — the reader `wsp resume`
/// asks and the only one entitled to look in `last`.
///
/// A live occupant is preferred over a remembered one, and both are answered
/// as a [`Seat`] because they are the same fact at different ages: this is who
/// was sitting here, where, and under which session. `elsewhere` is not
/// filtered out here the way [`seat_of`] filters it — a resume names the host
/// it is going to, and refusing to *read* a seat on another machine would make
/// `wsp resume` unable to say "that seat is on mb2" at all.
pub fn last_seat(governors: &BTreeMap<String, Value>, scope: &str) -> Option<Seat> {
    let rec = governors.get(scope)?;
    let of = |r: &Value| {
        let workspace = str_at(r, "workspace");
        (!workspace.is_empty()).then(|| Seat {
            scope: scope.to_string(),
            pane: str_at(r, "pane"),
            since: str_at(r, "since"),
            session: str_at(r, "session"),
            cwd: str_at(r, "cwd"),
            kind: str_at(r, "kind"),
            vacant: false,
        })
    };
    of(rec).or_else(|| rec.get("last").and_then(of))
}

/// The seat this scope's slot is for — filled, or remembered by a vacated
/// record — and `None` where **nobody is in it**.
///
/// **One definition of that question, and `wsp-148` is why there is one.**
/// Two readers disagreed about it: this verb's `--reseat` guard asked
/// [`seat_of_scope`] and read a record with a workspace and an *empty* `pane`
/// as occupied, while the reconciler asked the same record and read it as
/// unseated. So the daemon would count a vacancy twice, claim the record, launch
/// this verb — and be told *"the seat has somebody in it - nothing to reseat"*,
/// leaving the claim on a post nobody was ever going to fill for the next
/// twenty minutes. Found live on 2026-10-05, on `tooling`, whose record reads
/// `workspace: compound, pane: ""`.
///
/// **The empty pane is the whole of it.** [`seat_of`] stops at the workspace,
/// because a workspace names a seat and a pane names an *occupant*; two records
/// in this store write the first and not the second, and this is the reader that
/// has to notice. Live first, the way back second — [`last_seat`] is not an
/// alternative here, it is the same seat at a different age.
pub fn seat_held(scope: &str, governors: &BTreeMap<String, Value>) -> Option<Seat> {
    let seat = seat_of_scope(scope, governors).or_else(|| last_seat(governors, scope))?;
    (!seat.pane.is_empty()).then_some(seat)
}

/// The host a slot was last held from — a live occupancy's, or a vacated
/// record's memory of one. Empty where nothing has ever sat here.
pub fn host_of(governors: &BTreeMap<String, Value>, project: &str) -> String {
    let Some(rec) = governors.get(project) else { return String::new() };
    match str_at(rec, "host") {
        h if !h.is_empty() => h,
        _ => rec.get("last").map(|l| str_at(l, "host")).unwrap_or_default(),
    }
}

/// Why the last reseat of this seat failed, as [`reseat_failed`] recorded it.
pub fn reseat_why(governors: &BTreeMap<String, Value>, scope: &str) -> Option<String> {
    governors
        .get(scope)
        .and_then(|r| r.get("reseat_why"))
        .and_then(Value::as_str)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
}

/// The occupancy a vacated record keeps, out of the record it is replacing.
///
/// Everything a resume is keyed on and nothing a live reader looks at. A
/// record that was already vacated hands its own `last` back rather than
/// wrapping it again — standing an empty slot down twice must not bury the
/// thread one level deeper each time, and `reconcile --reap` runs on every
/// daemon start.
fn stood_down(rec: &Value) -> Value {
    if let Some(last) = rec.get("last") {
        return last.clone();
    }
    json!({
        "workspace": str_at(rec, "workspace"),
        "pane": str_at(rec, "pane"),
        "host": str_at(rec, "host"),
        "session": str_at(rec, "session"),
        "cwd": str_at(rec, "cwd"),
        "kind": str_at(rec, "kind"),
        // The tier, and **this is the field that made the omission matter.**
        // `wsp-148` reads it back off `last` to seat a successor, because a
        // governor whose pane died is vacated before the reconciler looks — so
        // without these two the one record a reseat has to read is the one that
        // did not carry them, and a run's most expensive seat was replaced at the
        // runtime default every time it was replaced.
        "model": str_at(rec, "model"),
        "effort": str_at(rec, "effort"),
        "since": str_at(rec, "since"),
    })
}

/// Empty a slot without taking it off the project.
///
/// The half of "it outlives its occupant" that a person can see. Every earlier
/// version of standing down deleted the record, so a night that ended took the
/// position down with the agent and the project woke up with no governor and no
/// sign there had ever been one. What is dropped is the occupancy — workspace,
/// pane, host — and what is kept is the post, which is the thing another agent
/// can be put into tomorrow.
///
/// Also what `reconcile --reap` does to a slot whose workspace has
/// closed: the agent is gone, the position is not.
///
/// **What it drops is the occupancy and what it keeps is the way back.** The
/// record it leaves carries `last`: the workspace, host, session and cwd of
/// whoever was just in it, which is exactly what `wsp resume` needs and exactly
/// what nothing else must read. Under its own key rather than left in place,
/// because every reader of a governor record asks *who is in this seat now* —
/// [`seat_of`], [`governs`], [`Slot::elsewhere`] — and a vacated record that
/// still answered `workspace` or `host` at the top level would tell all three
/// that somebody is sitting here. The nesting is the whole of the guarantee:
/// one key nobody had reason to look in before, so a slot cannot be brought
/// back to life by a field left behind.
/// kept and what is dropped, and then the `stood_down` sentence rides along if it
/// was there — because this function is `reconcile`'s as well as `--clear`'s, and
/// a reconciler that dropped the sentence would undo a person's decision on its
/// next visit. **The one thing that clears a stand-down is filling the seat**
/// ([`take`]), which is the point: the decision is about an empty seat, so an
/// empty seat cannot revoke it and an occupied one has already answered it.
pub fn vacate(store: &Store, project: &str) -> bool {
    let governors = store.governors();
    let Some(rec) = governors.get(project) else { return false };
    // Already empty. Said as false so a caller can report what it actually
    // changed rather than what it looked at.
    if seat_of(project, rec).is_none() && rec.get("workspace").is_none() {
        return false;
    }
    let mut left = json!({ "vacated": util::now_iso(), "last": stood_down(rec) });
    if let Some(at) = str_at(rec, STOOD_DOWN).into() {
        if let Some(o) = left.as_object_mut() {
            o.insert(STOOD_DOWN.into(), json!(at));
        }
    }
    store.set_governor(project, left);
    store.log_event("governor-vacated", json!({ "project": project }));
    true
}

/// Record that a person stood this seat down, and that it is to stay down.
///
/// **Written after the vacate rather than inside it, because `vacate` is also
/// what `reconcile` calls** and the two must not converge again. `vacate`'s own
/// record is what a dead governor leaves; this is the extra sentence on it that
/// says somebody meant it.
pub fn mark_stood_down(store: &Store, scope: &str) -> bool {
    store.edit_governor(scope, |rec| {
        let Some(o) = rec.as_object_mut() else { return false };
        o.insert(STOOD_DOWN.into(), json!(util::now_iso()));
        true
    })
}

/// One more pass has found this seat empty, and how this seat now reads.
///
/// **The whole record and not just the count, and the reason is that the caller
/// decides on one and writes the other.** The reconciler asks "is this seat
/// overdue?" — [`Vacancy::overdue`] — and this is the only reading that can
/// answer it, so returning the pair rather than the number means the predicate
/// that fires and the marker that recorded it are the same read of the same
/// value. Returning the count alone would put `EMPTY_TICKS` and the record's own
/// `unseated` field on either side of a write that a concurrent `learn_seats`
/// can interleave with.
///
/// Under the lock and re-read inside it, so two passes racing cannot both decide
/// they are the first.
pub fn count_unseated(store: &Store, scope: &str) -> Vacancy {
    let mut now = Vacancy::default();
    store.edit_governor(scope, |rec| {
        let Some(o) = rec.as_object_mut() else { return false };
        o.insert(
            "unseated".into(),
            json!(o.get("unseated").and_then(Value::as_u64).unwrap_or(0) + 1),
        );
        now = Vacancy::of(rec);
        true
    });
    now
}

/// Forget that this seat was ever empty — a pass that found somebody in it.
///
/// **Clears the count and never the claim.** A live seat is the one reading that
/// proves the reseat worked, and it is the only evidence there will ever be that
/// it did; clearing a claim on it would be clearing the receipt.
pub fn forget_unseated(store: &Store, scope: &str) {
    store.edit_governor(scope, |rec| {
        let Some(o) = rec.as_object_mut() else { return false };
        if o.remove("unseated").is_none() {
            return false;
        }
        true
    });
}

/// Take the claim that stops a second pass seating a second successor.
///
/// **The record before the spawn, which is the whole of the ordering.** It is
/// written by whoever is about to open a seat and started, so a pass that finds
/// it does nothing at all — not a second seat, and not a second sentence to the
/// spool. The alternative, deciding on a field that is only written once the
/// successor is up, leaves the window that `wsp-114` was filed about open for the
/// whole length of a spawn.
///
/// A stale claim is taken back rather than refused, and refusing it is the
/// obvious thing to write: an abandoned claim would then hold a dead governor's
/// post empty until a person noticed it, which is the failure this row exists to
/// end.
pub fn claim_seat(store: &Store, scope: &str) -> bool {
    let at = util::epoch_secs();
    store.edit_governor(scope, |rec| {
        if !Vacancy::of(rec).claim_free(at) {
            return false;
        }
        let Some(o) = rec.as_object_mut() else { return false };
        o.insert("reseating".into(), json!(util::now_iso()));
        true
    })
}

/// A reseat was attempted under a claim and did not produce a seat.
///
/// **The claim is kept and the failure is recorded against it, and both halves
/// are load-bearing.** The claim is what stops the next pass trying again
/// immediately, which is the whole difference between a momentary failure being
/// retried in a minute and a structural one being retried every minute for
/// ever — `tokenhub-spec-sync` on 2026-10-05, each attempt opening a compound
/// agent whose renderer never came up. The record of the failure is what lets
/// [`crate::wake`] say `retrying` rather than `reseating` for the twenty minutes
/// in between.
///
/// **Kept for a refusal as well as a broken agent**, and for the same reason: a
/// refusal the reconciler keeps making is a refusal it will keep making, and a
/// verb that released the claim on its way out turned one into a spawn a minute.
///
/// **With the reason, on the seat and in `cycle.log`, from this one call**
/// (`wsp-177`). Before, the seat said `retrying` and the reason was only in the
/// verb's undated stderr, which ended "cpd-313 has the order in its composer"
/// about a pane `put_back` had already ended. A reader of `--status` could not
/// tell a renderer that never came up from a refusal, and `cycle.log` had no
/// dated line saying the attempt was over. One function writes both so neither
/// can be forgotten by the next way out somebody adds.
pub fn reseat_failed(store: &Store, scope: &str, why: &str) {
    let mut claimed = None;
    store.edit_governor(scope, |rec| {
        let Some(o) = rec.as_object_mut() else { return false };
        o.insert("reseating_failed".into(), json!(util::now_iso()));
        o.insert("reseat_why".into(), json!(why));
        // A claim that was never taken is not invented here: this records that
        // an attempt happened, and the reconciler is what decides whether a
        // *next* one may.
        claimed = Vacancy::of(rec).reseating;
        true
    });
    let next = claimed
        .map(|at| format!("; the next attempt is after {}", util::local_hm(at + SEAT_CLAIMED_FOR)))
        .unwrap_or_default();
    crate::store::log_line(store, &format!("{scope}: the reseat failed — {why}{next}"));
}

/// The kind a reseat of this scope starts — the seat's own, read live or from
/// under `last`, or [`DEFAULT_KIND`].
///
/// **Under `last` as well, through [`cmd_govern::last_seat`]**, for
/// [`cmd_govern::tier_of`]'s reason: a governor whose pane died is vacated
/// before the reconciler looks, so the kind is nearly always there and a reader
/// of the top level alone defaulted it — right by luck for `claude`, and an
/// opencode run moved onto claude for everyone else.
pub fn seat_kind(governors: &BTreeMap<String, Value>, scope: &str) -> String {
    crate::agent_kind::kind_or_default(&last_seat(governors, scope).map(|s| s.kind).unwrap_or_default())
}

#[cfg(test)]
mod user_primary_tests {
    use super::*;

    #[test]
    fn a_user_primary_round_trips_and_is_not_the_bare_primary() {
        assert_eq!(primary_of("ed"), "primary:ed");
        assert_eq!(user_of_primary("primary:ed"), Some("ed"));
        assert_eq!(user_of_primary(&primary_of("jesper")), Some("jesper"));
        assert_eq!(user_of_primary("primary"), None);
        assert_eq!(user_of_primary("primary:"), None);
        assert_eq!(user_of_primary("wsp"), None);
    }

    #[test]
    fn is_primary_stays_the_bare_word_and_the_key_space_reserves_both() {
        assert!(is_primary("primary"));
        assert!(!is_primary("primary:ed"), "its 18 readers meant the bare primary");
        assert!(is_primary_key("primary") && is_primary_key("primary:ed"));
        assert!(!is_primary_key("primaryx") && !is_primary_key("wsp"));
    }

    /// `wsp-485`: on jesper's spoke an ed-allocated row's walk ends at
    /// `primary:ed` with no primary seated here, and with one seated — which is
    /// jesper's, never ed's, so it lends the seat nothing. An unallocated row,
    /// ed's own spoke and a standalone one walk as they did.
    #[test]
    fn another_users_primary_is_reached_with_no_local_primary_seated() {
        let index = crate::resolve::Index::new(vec![crate::model::Project::new("core")]);
        let seated = |pane: &str| serde_json::json!({ "workspace": "w1", "pane": pane });
        let mut g: BTreeMap<String, Value> = BTreeMap::new();
        g.insert("core".into(), serde_json::json!({ "workspace": "", "pane": "" }));
        let walk = |g: &BTreeMap<String, Value>, to: Option<&str>, login: Option<&str>| {
            seat_for_as(g, &index, None, Some("core"), to, login)
        };

        let s = walk(&g, Some("ed"), Some("jesper")).expect("ed's, with nobody seated here");
        assert_eq!((s.scope.as_str(), s.pane.as_str(), s.vacant), ("primary:ed", "", false));
        assert_eq!(walk(&g, Some("ed"), None), None, "standalone: today's walk");
        assert_eq!(walk(&g, Some("ed"), Some("ed")), None, "ed's own spoke with no primary: today's");
        assert_eq!(walk(&g, None, Some("jesper")), None, "unallocated: the person");
        assert_eq!(walk(&BTreeMap::new(), Some("ed"), Some("jesper")).map(|s| s.scope), Some("primary:ed".into()));

        g.insert(PRIMARY.into(), seated("pr:1"));
        let s = walk(&g, Some("ed"), Some("jesper")).unwrap();
        assert_eq!((s.scope.as_str(), s.pane.as_str()), ("primary:ed", ""), "jesper's primary is not ed's");
        let own = walk(&g, Some("jesper"), Some("jesper")).unwrap();
        assert_eq!((own.scope.as_str(), own.pane.as_str()), ("primary:jesper", "pr:1"), "its own user: today's");
        assert_eq!(walk(&g, Some("ed"), None).unwrap().pane, "pr:1", "standalone: today's");
        assert_eq!(walk(&g, None, Some("jesper")), None);

        // A record under the user's own key is theirs, seated.
        g.insert("primary:ed".into(), seated("ed:1"));
        assert_eq!(walk(&g, Some("ed"), Some("jesper")).unwrap().pane, "ed:1");
    }
}
