//! The board as styled lines.
//!
//! Shares the panel's [`Line`]/[`Style`] model, so a board can be painted to a
//! terminal or dropped into the storyboard, and the two cannot drift apart on
//! colour. Nothing here reads the store: [`frame`] is a function of the
//! [`Board`] it is given and the size of the pane.

use crate::model::{Priority, Status};
use crate::panel::{self, glyph, line, Line, Style};
use crate::util;

use super::{Board, Card, Cursor};

/// Rows above the columns: the title and its rule.
const HEAD: usize = 2;

/// The column headings and the rule under them.
const COLHEAD: usize = 2;

/// The dock at the foot: a rule, the selected card in full, its two lines of
/// detail, and the key hint.
///
/// Fixed, for the reason the panel's focus dock is fixed: a foot that shrank to
/// fit a short title would give a row back to the columns and take it away
/// again on every press of `j`, and the cards you are reading would step up and
/// down while you scrolled past them.
const DOCK: usize = 4;

/// Columns between one card and the next, drawn as ` │ `.
const GAP: usize = 3;

/// The mark on the selected card.
///
/// The panel highlights a selected row by inverting it, and that cannot work
/// here: a row of this frame holds four cards, and inverting the row would
/// light up all four. So the selection is a bar in the card's own gutter —
/// which is a column the card needs anyway, and which reads down the board
/// rather than across it.
const HERE: &str = "▌";

/// How wide each column is, left to right.
///
/// The remainder goes to the leftmost columns rather than the last, so the
/// widest column is the one holding the work that has not started — which is
/// where the longest titles are, because nobody has abbreviated them yet.
pub(super) fn widths(n: usize, w: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    let room = w.saturating_sub(GAP * (n - 1));
    let each = room / n;
    let mut spare = room - each * n;
    (0..n)
        .map(|_| {
            let extra = usize::from(spare > 0);
            spare = spare.saturating_sub(1);
            each + extra
        })
        .collect()
}

/// Which slice of a column is on screen, and how much is out of sight below it.
///
/// The cursor is the only thing that scrolls a column, and only its own: the
/// other columns sit at the top, because there is no reader in them. Returns
/// `(offset, shown, below)` — what is hidden above is the offset itself. The
/// two `⋯` markers cost a row each, so a column that admits to hiding something
/// draws one fewer card than one that does not; pretending otherwise would draw
/// a card into a row the window has already spent.
pub(super) fn window(len: usize, body: usize, row: usize) -> (usize, usize, usize) {
    if body == 0 {
        return (0, 0, len);
    }
    if len <= body {
        return (0, len, 0);
    }
    let mut inner = body;
    loop {
        // The cursor rides the bottom edge once the column has scrolled, and
        // the window never runs off the end of the cards.
        let off = row.saturating_sub(inner.saturating_sub(1)).min(len - inner);
        let below = len - off - inner;
        let want = inner + usize::from(off > 0) + usize::from(below > 0);
        // At one card there is nothing left to pay a marker with, and the card
        // the cursor is on is what the row is for. The counts stay honest and
        // the frame draws what fits — a pane three rows high is not a board.
        if want <= body || inner <= 1 {
            return (off, inner, below);
        }
        inner -= 1;
    }
}

/// The glyph in a card's second column.
///
/// The board's columns already say what the status is, so the mark is free to
/// answer the question they cannot: who is on it, and has it stopped. When
/// nobody is, it falls back to the task's own status — which is only ever news
/// in the `doing` column, where it is the ■ that separates blocked work from
/// work in hand.
fn mark(card: &Card) -> (Style, &'static str) {
    match card.agent {
        Some(state) => state.mark(),
        None => panel::status_mark(card.status),
    }
}

/// One card, fitted to its column.
fn card_line(card: &Card, w: usize, here: bool, project: Option<&str>) -> Line {
    let mut l = Line::default();
    l.push(if here { Style::Accent } else { Style::Plain }, if here { HERE } else { " " });

    // A column that lines up down the board: `normal` is a space rather than
    // nothing, or every title on the rows around a `!` would shift a column.
    let (pst, pmark) = match card.priority {
        Priority::High => (Style::Warn, glyph::HIGH),
        Priority::Low => (Style::Dim, glyph::LOW),
        Priority::Normal => (Style::Dim, " "),
    };
    l.push(pst, pmark);
    let (st, g) = mark(card);
    l.push(st, g);
    l.push(Style::Plain, " ");

    // On the right: the project, when the board spans more than one, and the
    // work decomposed underneath. Both are things the column cannot say.
    //
    // The project only where there is room for it. A column twenty wide spends
    // four on the gutter and eleven on a project id, and what is left is not a
    // title — it is a truncation with a project name after it, and the title is
    // the one thing a card must carry. The dock says which project whatever the
    // width, so nothing is lost but the glance.
    let mut right = Line::default();
    if card.under.open > 0 {
        right.push(Style::Dim, format!("{}", card.under.open));
    }
    if let Some(p) = project.filter(|_| w >= 32) {
        if !right.spans.is_empty() {
            right.push(Style::Plain, " ");
        }
        right.push(Style::Dim, util::truncate(p, 10));
    }
    let flag = usize::from(card.prose) * 2;
    let tail = if right.spans.is_empty() { 0 } else { right.width() + 1 };
    let avail = w.saturating_sub(l.width() + tail + flag).max(4);

    let ink = match (card.agent, card.status) {
        (Some(crate::panel::AgentState::Asking), _) => Style::Warn,
        (_, Status::Blocked) => Style::Warn,
        (_, Status::Parked) => Style::Dim,
        (_, Status::Done) => Style::Dim,
        (Some(_), _) => Style::Plain,
        _ => Style::Muted,
    };
    l.push(ink, util::truncate(&card.title, avail));
    if card.prose {
        l.push(Style::Plain, " ");
        l.push(Style::Dim, glyph::NOTES);
    }
    if !right.spans.is_empty() {
        l.pad(w.saturating_sub(l.width() + right.width()).max(1));
        l.spans.extend(right.spans);
    }
    l.fit(w);
    l
}

/// Lay the columns side by side.
///
/// Every column is padded to the same number of rows before it gets here, so
/// this is a transpose and nothing more — which is the point: the arithmetic
/// that decides what is in a column lives where the column is built, and this
/// only decides where it is drawn.
fn beside(cols: &[Vec<Line>], widths: &[usize], rule: bool) -> Vec<Line> {
    let rows = cols.iter().map(|c| c.len()).max().unwrap_or(0);
    (0..rows)
        .map(|y| {
            let mut l = Line::default();
            for (i, col) in cols.iter().enumerate() {
                if i > 0 {
                    l.push(Style::Dim, if rule { "─┼─" } else { " │ " });
                }
                let mut cell = col.get(y).cloned().unwrap_or_default();
                cell.fit(widths[i]);
                l.spans.extend(cell.spans);
            }
            l
        })
        .collect()
}

/// The top line: what the board is of, who is running, and how much work there
/// is.
///
/// The strip is the panel's, mark for mark: one column per agent, what wants
/// you first, in the same glyphs and the same colours. A board answers what the
/// work is and which pile it is in; this answers how much attention is on it
/// and how much of that has stopped — and it is drawn from the whole census
/// whatever the board is scoped to, because a count that went quiet when you
/// opened a board on a quiet project is a count you learn to distrust.
fn header(board: &Board, w: usize) -> Line {
    // The right-hand side first, so the strip knows what room it has. The
    // counts are never the thing that gets clipped: a truncated strip must not
    // also be the only thing saying how many there are.
    let (open, blocked) = board.totals();
    let mut right = Line::default();
    right.push(Style::Dim, format!("{open} open"));
    if blocked > 0 {
        right.push(Style::Plain, "  ");
        right.push(Style::Warn, format!("{}{blocked}", glyph::BLOCKED));
    }
    // Only when it is on, and in the same word the panel uses for the same
    // state. Off is the resting state — a header that said so every frame would
    // train the eye to skip the line.
    if board.show_done {
        right.push(Style::Plain, "  ");
        right.push(Style::Accent, "+done");
    }

    let mut l = Line::default();
    l.push(Style::Bold, "wsp");
    l.push(Style::Dim, " · ");
    l.push(Style::Bold, board.scope.label());
    if !board.agents.is_empty() {
        let total = board.agents.len();
        // Three of gap, the count and its space, and a column between the strip
        // and the figures on the right.
        let room = w
            .saturating_sub(l.width() + 3 + total.to_string().chars().count() + 1)
            .saturating_sub(right.width() + 1);
        let (shown, clipped) =
            if total > room { (room.saturating_sub(1), true) } else { (total, false) };
        l.push(Style::Plain, "   ");
        for a in board.agents.iter().take(shown) {
            let (st, mark) = a.state.mark();
            l.push(st, mark);
        }
        if clipped {
            l.push(Style::Dim, glyph::MORE);
        }
        l.push(Style::Dim, format!(" {total}"));
    }

    l.pad(w.saturating_sub(l.width() + right.width()).max(1));
    l.spans.extend(right.spans);
    l
}

/// The agents with nothing in their hands, named.
///
/// Everyone else is already on the board: an agent holding a card *is* the mark
/// on that card, and the column it sits in is what the agent is doing. An agent
/// holding nothing is on no card by definition, so without this line the board
/// shows you every piece of work and none of the people free to take one —
/// which is the pair of facts you opened it to put together.
///
/// One line, and it earns its row only when there is somebody on it. The height
/// changes when an agent goes free, which is rare and is news; it does not
/// change as the cursor moves, which is what would make the cards jump.
fn spare_rail(board: &Board, w: usize) -> Option<Line> {
    let spare = board.spare();
    if spare.is_empty() {
        return None;
    }
    let mut l = Line::default();
    let (st, mark) = crate::panel::AgentState::Spare.mark();
    l.push(st, mark);
    l.push(Style::Muted, format!(" spare {}", spare.len()));
    l.push(Style::Dim, "  ·  ");
    let mut left = 0;
    for (i, a) in spare.iter().enumerate() {
        // Everything after the first has to fit whole, or the line ends in half
        // a name — which reads as a name rather than as a truncation.
        let mut want = Line::default();
        if i > 0 {
            want.push(Style::Dim, "   ");
        }
        want.push(Style::Muted, util::truncate(&a.name, 32));
        want.push(Style::Dim, format!(" · {}", a.pane));
        if l.width() + want.width() > w.saturating_sub(10) && i > 0 {
            left = spare.len() - i;
            break;
        }
        l.spans.extend(want.spans);
    }
    if left > 0 {
        l.push(Style::Dim, format!("   +{left}"));
    }
    l.fit(w);
    Some(l)
}

/// The card under the cursor, said in full.
///
/// A card is one line of a column twenty-odd wide, and a title averages
/// sixty-four characters — so the board names most work by its first fifth.
/// This is where the rest of it goes, along with the two facts a card has no
/// room for: what to type at a shell to mean it, and who is holding it.
fn dock(board: &Board, cur: &Cursor, w: usize) -> Vec<Line> {
    let Some(card) = board.card_at(cur) else {
        let lane = board.columns.get(cur.col).map(|c| c.lane.label()).unwrap_or("this column");
        return vec![line(Style::Dim, format!("nothing in {lane}")), Line::default()];
    };

    let mut facts = Line::default();
    facts.push(Style::Dim, card.id.clone());
    facts.push(Style::Plain, "  ");
    let (st, _) = panel::status_mark(card.status);
    facts.push(st, card.status.as_str());
    if card.priority != Priority::Normal {
        facts.push(Style::Plain, "  ");
        facts.push(
            if card.priority == Priority::High { Style::Warn } else { Style::Dim },
            card.priority.as_str(),
        );
    }
    if let Some(p) = &card.project {
        facts.push(Style::Plain, "  ");
        facts.push(Style::Muted, p.clone());
    }
    if card.under.open > 0 {
        facts.push(Style::Plain, "  ");
        facts.push(Style::Dim, format!("{} under it", card.under.open));
    }
    // Who has it, and for how long. The difference between an agent working and
    // an agent stuck is the second half of that sentence.
    if let Some(state) = card.agent {
        facts.push(Style::Plain, "  ");
        let (ink, _) = state.mark();
        facts.push(ink, panel::agent_word(state));
        if let Some(pane) = &card.pane {
            facts.push(Style::Dim, format!(" · {pane}"));
        }
        if let Some(h) = &card.held {
            facts.push(Style::Dim, format!(" · {h}"));
        }
    }
    facts.fit(w);

    vec![line(Style::Plain, util::truncate(&card.title, w)), facts]
}

pub(crate) fn frame(board: &Board, cur: &Cursor, w: usize, h: usize, note: &str) -> Vec<Line> {
    let n = board.columns.len();
    let ws = widths(n, w);
    let rail = spare_rail(board, w);
    let body = h.saturating_sub(HEAD + COLHEAD + DOCK + usize::from(rail.is_some())).max(1);

    let mut out: Vec<Line> = vec![header(board, w), line(Style::Dim, "─".repeat(w))];

    // The headings, and the rule beneath them ruled through at every seam, so
    // the columns read as columns before a single card is drawn.
    let heads: Vec<Vec<Line>> = board
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut l = Line::default();
            l.push(Style::Plain, " ");
            // The column the cursor is in, said in the ink the panel uses for
            // what is live. Without it a board with three empty columns gives
            // no clue where a verb would land.
            l.push(if i == cur.col { Style::Accent } else { Style::Muted }, c.lane.label());
            l.push(Style::Plain, " ");
            l.push(Style::Dim, c.cards.len().to_string());
            if c.dropped > 0 {
                l.push(Style::Dim, format!(" of {}", c.cards.len() + c.dropped));
            }
            vec![l]
        })
        .collect();
    out.extend(beside(&heads, &ws, false));
    let rules: Vec<Vec<Line>> =
        ws.iter().map(|w| vec![line(Style::Dim, "─".repeat(*w))]).collect();
    out.extend(beside(&rules, &ws, true));

    let cols: Vec<Vec<Line>> = board
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let row = if i == cur.col { cur.row } else { 0 };
            let (off, shown, below) = window(c.cards.len(), body, row);
            let mut lines: Vec<Line> = Vec::new();
            if off > 0 {
                lines.push(more(off, ws[i], true));
            }
            for (j, card) in c.cards.iter().enumerate().skip(off).take(shown) {
                let project = board
                    .mixed
                    .then(|| card.project.as_deref().unwrap_or("inbox"))
                    .filter(|p| Some(*p) != scope_id(board));
                lines.push(card_line(card, ws[i], i == cur.col && j == cur.row, project));
            }
            if below > 0 {
                lines.push(more(below, ws[i], false));
            }
            // The markers are what the window would like to say; the pane is
            // what it has. On anything taller than three rows these agree.
            lines.truncate(body);
            while lines.len() < body {
                lines.push(Line::default());
            }
            lines
        })
        .collect();
    out.extend(beside(&cols, &ws, false));

    out.push(line(Style::Dim, "─".repeat(w)));
    // Above the card's own detail, so the foot reads as "who is free, then what
    // you are looking at" — the order you would ask them in.
    out.extend(rail);
    out.extend(dock(board, cur, w));
    out.push(if note.is_empty() {
        line(
            Style::Dim,
            "hjkl move · s v d o set · < > shift · ! priority · ↵ open it · E edit · A done · q close",
        )
    } else {
        line(Style::Accent, util::truncate(note, w))
    });
    out.truncate(h);
    out
}

/// What the scope names, when it names a project — the one project a card need
/// not repeat.
fn scope_id(board: &Board) -> Option<&str> {
    match &board.scope {
        super::Scope::Project(p) => Some(p.as_str()),
        _ => None,
    }
}

fn more(n: usize, w: usize, above: bool) -> Line {
    let mut l = Line::default();
    l.push(Style::Plain, " ");
    l.push(Style::Dim, glyph::MORE);
    l.push(Style::Plain, " ");
    l.push(Style::Muted, format!("{n} {}", if above { "above" } else { "more" }));
    l.fit(w);
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The columns and the gaps have to add up to the pane exactly, or the rule
    /// under the headings stops short of the edge and the board looks broken at
    /// every width but the one it was written at.
    #[test]
    fn the_columns_fill_the_pane_at_every_width() {
        for n in 1..=4 {
            for w in 20..200 {
                let ws = widths(n, w);
                let total: usize = ws.iter().sum::<usize>() + GAP * (n - 1);
                assert_eq!(total, w, "{n} columns at {w}");
                // And no column is more than one wider than another, or the
                // board leans.
                let (lo, hi) = (ws.iter().min().unwrap(), ws.iter().max().unwrap());
                assert!(hi - lo <= 1, "{ws:?} at {w}");
            }
        }
    }

    /// The window has to hold the cursor, and it has to be honest about what it
    /// is not showing — including the row each of those admissions costs.
    #[test]
    fn the_window_holds_the_cursor_and_pays_for_what_it_hides() {
        for len in 0..40usize {
            for body in 1..12usize {
                for row in 0..len.max(1) {
                    let (off, shown, below) = window(len, body, row);
                    assert!(shown <= body, "len {len} body {body} row {row} showed {shown}");
                    // Both markers and a card need three rows. Below that the
                    // card wins and the frame trims the rest.
                    let drawn = shown + usize::from(off > 0) + usize::from(below > 0);
                    assert!(
                        drawn <= body.max(3),
                        "len {len} body {body} row {row} drew {drawn}",
                    );
                    assert_eq!(off + shown + below, len, "len {len} body {body} row {row}");
                    if len > 0 && row < len {
                        assert!(
                            row >= off && row < off + shown,
                            "cursor {row} is outside {off}..{}", off + shown,
                        );
                    }
                }
            }
        }
    }

    fn board_with(agents: Vec<crate::kanban::Agent>) -> Board {
        Board {
            scope: crate::kanban::Scope::Project("wsp".into()),
            columns: crate::kanban::Lane::ALL
                .iter()
                .map(|l| crate::kanban::Column { lane: *l, cards: Vec::new(), dropped: 0 })
                .collect(),
            agents,
            mixed: false,
            show_done: true,
        }
    }

    fn agent(state: crate::panel::AgentState, name: &str, pane: &str) -> crate::kanban::Agent {
        crate::kanban::Agent {
            state,
            name: name.into(),
            pane: pane.into(),
            task: None,
            held: None,
        }
    }

    /// The rail takes a row from the columns, and only when somebody is on it.
    /// A board where everyone is busy must not spend a row saying so — and a
    /// board where somebody is free must not make you count marks in the header
    /// to find out who.
    #[test]
    fn the_spare_rail_costs_a_row_only_when_there_is_somebody_on_it() {
        use crate::panel::AgentState;
        let busy = board_with(vec![agent(AgentState::Working, "Trance Video", "w1:p1")]);
        assert!(spare_rail(&busy, 120).is_none());
        let cards_when_busy = frame(&busy, &Cursor::default(), 120, 30, "").len();

        let free = board_with(vec![
            agent(AgentState::Working, "Trance Video", "w1:p1"),
            agent(AgentState::Spare, "Verb UI", "w2:p1"),
        ]);
        let rail = spare_rail(&free, 120).expect("somebody is free and the board should say so");
        let text = rail.text();
        assert!(text.contains("spare 1"), "{text}");
        assert!(text.contains("Verb UI"), "{text}");
        assert!(text.contains("w2:p1"), "the name alone will not let you reach it: {text}");
        assert!(!text.contains("Trance Video"), "a working agent is not capacity: {text}");

        // The frame is the same height either way — the rail comes out of the
        // columns, not out of the pane.
        let with_rail = frame(&free, &Cursor::default(), 120, 30, "");
        assert_eq!(with_rail.len(), cards_when_busy);
        assert_eq!(with_rail.len(), 30);
    }

    /// Every agent is a mark, and the count is never what gets cut. A strip too
    /// long for the header says so with a `⋯` rather than quietly reporting
    /// fewer agents than there are.
    #[test]
    fn the_strip_clips_before_the_count_does() {
        use crate::panel::AgentState;
        let many: Vec<crate::kanban::Agent> = (0..40)
            .map(|i| agent(AgentState::Working, &format!("agent {i}"), &format!("w{i}:p1")))
            .collect();
        let b = board_with(many);
        for w in 30..140 {
            let text = header(&b, w).text();
            assert!(text.contains(" 40"), "the count went missing at {w}: {text}");
            assert!(text.contains("0 open"), "the work went missing at {w}: {text}");
            assert!(
                text.chars().count() <= w,
                "the header outgrew the pane at {w}: {} chars",
                text.chars().count(),
            );
        }
    }

    /// A column shorter than the pane never scrolls and never claims to.
    #[test]
    fn a_short_column_sits_still() {
        assert_eq!(window(3, 10, 2), (0, 3, 0));
        assert_eq!(window(0, 10, 0), (0, 0, 0));
    }
}
