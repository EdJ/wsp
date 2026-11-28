//! A merge or hand-off row asks its decisions first (`wsp-866`).
//!
//! trunk-green's one merge row (wsp-368) asked its merge point, a design clash,
//! a guard change and the push go one at a time as it met them: 44 of the list's
//! 116 minutes were the person's. Jesper, 2026-11-23: such a row starts with a
//! short first leg that lists every conflict and every person-shaped choice and
//! asks them in one batch before any resolving starts, and the push go is
//! recorded in the list's overview when the list is planned.
//!
//! Three places say it: `wsp worklist add` and `status planned` name the legs to
//! file and a missing `Push:` line ([`planning`]); the packet of a row worked as
//! one seat carries [`RULE`]; and `wsp worklist show` prints the `Push:` line
//! ([`push_line`]), which is where the row reads it.

use crate::model::Task;

/// The word a list's overview records the push go behind: `Push: go (…)`.
pub const PUSH_KEY: &str = "push:";

/// The tags that say a row is one, whatever its title.
const TAGS: &[&str] = &["merge", "hand-off", "handoff"];

/// Whether a row brings other history in: it is tagged so, or its title starts
/// with "Merge" and names where from (`origin/`, upstream) or into which trunk.
/// "Merge the two list renderers" is neither.
pub fn is(t: &Task) -> bool {
    if t.tags.iter().any(|tag| TAGS.iter().any(|k| tag.eq_ignore_ascii_case(k))) {
        return true;
    }
    let title = t.title.to_ascii_lowercase();
    let mut words = title.split(|c: char| !(c.is_ascii_alphanumeric() || c == '/' || c == '-' || c == '_'));
    if words.find(|w| !w.is_empty()) != Some("merge") {
        return false;
    }
    title.contains("origin/")
        || title.contains("upstream")
        || words.skip_while(|w| *w != "into").any(|w| ["master", "main", "trunk"].contains(&w))
}

/// What a list's overview says of the push: the rest of its `Push:` line.
pub fn push_line(overview: &str) -> Option<String> {
    overview.lines().find_map(|l| {
        let bare = l.trim().trim_start_matches(['-', '*', ' ']);
        let head = bare.get(..PUSH_KEY.len())?;
        let said = bare[PUSH_KEY.len()..].trim();
        (head.eq_ignore_ascii_case(PUSH_KEY) && !said.is_empty()).then(|| said.to_string())
    })
}

/// What the packet of a merge row worked as one seat says.
pub const RULE: &str = "## A merge row: decisions first
Before resolving anything:
1. Name the merge point and trial the merge without touching the tree (`git merge-tree --write-tree --name-only HEAD <ref>`).
2. List every conflict, with both sides' intent and your proposed resolution, and every choice that is the person's: the merge point, a design clash, a guard or test limit you would change, the push.
3. Ask them all in one batch, one `wsp ask <row> --person --wait --choices -` with a question for each. The push go is the `push` line of `wsp worklist show <list>`: where it is there, do not ask about the push.
Then merge, never rebase. A branch that holds the merge and contains master lands by fast-forward, so end as your order says and never block on a person for a `git merge --ff-only`; if master moves, `git merge master` again.";

/// The narrow packet order's last sentence, for a merge row: its conflicts are
/// in files no packet can name.
pub const ORDER: &str = "This is a merge row: ask its decisions first, as the packet says, and read the files its conflicts are in.";

/// The first leg's order. `packet: off`: a generated packet would forbid
/// reading the conflicts the leg is there to find.
const DECIDE: &str = "packet: off
Decide, and resolve nothing. Trial the merge without touching the tree (`git merge-tree --write-tree --name-only HEAD <ref>`). List every conflict with both sides' intent and a proposed resolution, and every choice that is the person's: the merge point, a design clash, a guard or test limit to change, the push (not where `wsp worklist show {list}` has a `push` line: that is the go). Ask them all in one `wsp ask {row} --person --wait --choices -`, a question for each. Leave the conflict list, the answers and the list's `push` line, where it has one, in your baton.";

/// The last leg's order.
const FINISH: &str = "packet: off
Merge as the baton's answers say, and never rebase. The push go is the `push` line of `wsp worklist show {list}`: where it is there, it is the person's answer, so do not ask about the push, even where the row says to ask first. Resolve, run the row's checks, and submit: a branch that holds the merge and contains master lands by fast-forward, so ask nobody for a `git merge --ff-only`; if master moves, `git merge master` again.";

/// What a planner is told of a merge row put in `list`: the two legs to file
/// where it has none, and the `Push:` line the list's overview lacks. Empty
/// when both are there, and for a row that is not one.
pub fn planning(t: &Task, list: &str, overview: &str) -> Vec<String> {
    let mut out = Vec::new();
    if !is(t) {
        return out;
    }
    let row = &t.id;
    if crate::legs::legs(t).is_empty() {
        let leg = |name: &str, order: &str| {
            let order = order.replace("{row}", row).replace("{list}", list);
            format!("wsp leg add {row} {name} --order - <<'EOF'\n{order}\nEOF")
        };
        out.push(format!(
            "wsp: {row} is a merge row: its decisions are asked before any resolving. Give it these two legs:\n{}\n{}",
            leg("decide", DECIDE),
            leg("finish", FINISH)
        ));
    }
    if push_line(overview).is_none() {
        out.push(format!(
            "wsp: {row} is a merge row and `{list}` records no push go: ask the person now and write a `Push: …` line \
             in its overview (`wsp worklist edit {list} --overview -`), so the row never asks mid-run"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(title: &str) -> Task {
        Task::new(title, "t-1")
    }

    #[test]
    fn a_merge_row_is_read_off_its_title_or_its_tag() {
        for title in [
            "Merge origin/master (Ed's 26f980b) into the trunk and push",
            "Merge Ed's origin/master (11 commits) into compound-react master; resolve feed.rs",
            "Merge upstream's release",
            "merge the read-api branch into master",
        ] {
            assert!(is(&row(title)), "{title}");
        }
        for title in ["Merge the two list renderers", "Merge two sections into one", "A merge row's decisions are asked mid-row", "Show the main page"] {
            assert!(!is(&row(title)), "{title}");
        }
        let mut tagged = row("Take over Ed's read-API work");
        tagged.tags = vec!["hand-off".into()];
        assert!(is(&tagged));
    }

    #[test]
    fn the_push_go_is_the_rest_of_a_push_line() {
        assert_eq!(push_line("Why.\n- Push: go (Jesper, 2026-11-23)\n").as_deref(), Some("go (Jesper, 2026-11-23)"));
        assert_eq!(push_line("push: no").as_deref(), Some("no"));
        assert_eq!(push_line("Push:\nThe merge is pushed later.\næøå ünï"), None);
    }

    #[test]
    fn planning_names_the_legs_and_the_push_line_until_each_is_there() {
        let mut t = row("Merge origin/master into the trunk");
        let said = planning(&t, "green", "Start now.");
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[0].contains("wsp leg add t-1 decide --order -") && said[0].contains("wsp leg add t-1 finish"), "{}", said[0]);
        assert!(said[0].contains("wsp ask t-1 --person") && said[0].contains("wsp worklist show green") && !said[0].contains('{'), "{}", said[0]);
        let finish = said[0].split_once("wsp leg add t-1 finish").unwrap().1;
        assert!(finish.contains("The push go is the `push` line of `wsp worklist show green`"), "{finish}");
        assert!(finish.contains("do not ask about the push"), "{finish}");
        assert!(said[1].contains("wsp worklist edit green --overview -"), "{}", said[1]);
        t.body = "## Legs\n- decide · claude sonnet medium · pending\n  order: x\n".into();
        assert!(planning(&t, "green", "Push: go").is_empty());
        assert!(planning(&row("Show the size"), "green", "").is_empty());
    }
}
