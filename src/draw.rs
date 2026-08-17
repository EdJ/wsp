//! The renderer: one spec, drawn to a terminal or to a block of text.
//!
//! Part 4 of the partition Ed adopted on 2026-08-17 (decision on t-260816-083).
//! Parts 1–3 are the store, `place.rs` and `arrange.rs`; this is the one that
//! had no file, because until now it existed only as a command — `wsp panel
//! storyboard` — that happens to know how to draw.
//!
//! `arrange.rs` says what panes there are. It does not say what is *in* them: a
//! [`Body::Rendered`] pane carries a [`Content`], which is a name and a target
//! and deliberately not a command, and turning that into something a person can
//! see is this file's whole job. One sentence:
//!
//! > a renderer takes a spec and a view, and puts cells somewhere.
//!
//! # Why two targets, and why the pair is the point
//!
//! A live herdr pane and a block of text for tests and storyboards. Neither is
//! interesting alone — the terminal is what we already ship, and a text block is
//! a debugging convenience. The pair is the falsifiable half of the whole
//! partition, and it falsifies in one direction: **if a renderer can draw a view
//! with nothing running, then the view really does carry what a renderer needs.**
//! If a target has to reach past the view to the store or to herdr, the seam is
//! in the wrong place, and the text block is what says so, on the day it is
//! written, rather than the third target discovering it a year later.
//!
//! The third target — a canvas node, a phone — is the reason the seam is worth
//! having and is not built here. What is built is the shape it would arrive
//! into: [`Target`] has two methods and neither of them mentions a terminal.
//!
//! # Who composites
//!
//! The one asymmetry, stated up front because it is the thing a reader expects
//! to be wrong. Both targets are handed the same [`Spec`] and both walk the same
//! [`layout`]; what differs is how many cells they are usually given.
//!
//! - Standing in herdr, wsp's rendered panes are *separate processes* — `wsp
//!   view` in one pane, `wsp panel` in another. Each is handed a spec of one
//!   pane, filling its own terminal, and the multiplexer composited the rest.
//! - A storyboard has no multiplexer, so [`Block`] is handed the whole spec and
//!   does the compositing itself.
//!
//! That is one code path with a different number of cells in it, which is why
//! [`Ansi`] positions by [`Rect`] rather than assuming the origin: a spec of one
//! pane at `{0,0,w,h}` produces exactly the escapes `panel::to_ansi` produced
//! before this file existed, and a spec of three produces three regions in one
//! terminal. The second case has no caller today. It is not speculative
//! generality — it is the same arithmetic, and refusing to do it would have
//! meant writing the positioning twice.
//!
//! # What a filled pane can be drawn as, and the boundary that is
//!
//! `arrange.rs` splits panes into [`Body::Rendered`] (wsp draws it from a
//! snapshot) and [`Body::Filled`] (an agent or a command lives there). **Only
//! rendered panes can be drawn to a text block.** A filled pane holds a live
//! process whose output is not a function of anything wsp holds; in a storyboard
//! it is a labelled box and nothing more.
//!
//! This is the boundary of what a storyboard can prove, and it is written here
//! rather than discovered by somebody who expected agent output in a frame. It
//! also decides the two targets' opposite behaviour, which looks like an
//! inconsistency and is the rule applied twice:
//!
//! - [`Block`] draws a stub for a filled pane, **because nothing else will**.
//!   A page with a hole in it misreports the layout.
//! - [`Ansi`] draws nothing at all for one, **because something else already
//!   has**. Painting over a live process is the one thing a renderer must never
//!   do, and a target that treated the two cases alike would do exactly that.
//!
//! [`Body::Done`] is drawn by neither: a pane whose occupant has finished is a
//! pane on its way out, and the spec is telling the reconciler, not us.
//!
//! # Focus is not an input
//!
//! Ed's decision of 2026-08-17: never rely on focus to make a mechanism work.
//! Nothing below reads it, and that is enforced by there being nowhere to read
//! it from — [`Cell`] carries a slot, a label, a rect and a body. A renderer
//! draws a pane whether or not a person is looking at it, which is what makes
//! `Spec::focus` a statement about the destination rather than a precondition
//! for anything here.
//!
//! It holds by construction rather than by discipline, and the seam holds the
//! same line one layer down: `focused` does not reach the view layer either.
//! herdr's two focus facts are carried on `live::Live`, beside the rows rather
//! than inside them, because their only reader is the panel's event loop —
//! picking the refresh interval, and deciding what a click means. A scheduling
//! input travelling inside a view is one every renderer is handed and none of
//! them can use.
//!
//! # The seam under this port, and why it is not the port's business
//!
//! This file names no herdr type, because [`Paint`] is a trait and the view
//! stays behind it. That was true before t-260817-012 and did not mean much:
//! all three views carried the runner's structs — `panel::Snapshot` held
//! `Vec<herdr::Workspace>` and `Vec<herdr::Pane>`, `detail::Ctx` and
//! `kanban::Ctx` a pane list each — so the cost landed on whoever implemented
//! the trait, and a canvas renderer for a phone would have had to link wsp's
//! herdr socket client to compile.
//!
//! It does not now. `crate::live` is where the socket call sits and
//! `live::AgentRef` is what a view carries, so a target below this port is
//! handed wsp's own types and nothing else. The rule that keeps it that way is
//! falsifiable rather than argued: no `herdr::` in `panel/rows.rs`,
//! `detail/render.rs`, `kanban/mod.rs` or `story.rs`.
//!
//! # What is deliberately absent
//!
//! - **A view type.** [`Paint`] is a trait so that this file names none of the
//!   three. A port that imported `Snapshot` would have tied the renderer to one
//!   surface's answer, and — while the views still carried herdr — would have
//!   made that dependency unavoidable rather than visible.
//! - **Input.** A renderer draws; keys are `panel::keys` and `kanban::keys`,
//!   and the state they mutate is the painter's — see [`Paint::paint`] taking
//!   `&mut self`, which is the only concession this port makes to the fact that
//!   a frame is not a function of the view alone.
//! - **Migrating `story.rs` and the three run loops.** The same call is made
//!   here that `arrange.rs` made about its own: an adapter written before its
//!   callers are known is a third opinion about what they need. What has moved
//!   is the one piece that could not drift safely — `panel::to_ansi` is now the
//!   one-cell case of [`Ansi`] rather than a second loop that agrees with it.

#![allow(dead_code)]

use crate::arrange::{Anchor, Body, Content, Dir, Filler, Rect, Slot, Spec};
use crate::panel::{ansi_of, to_html_spans, Line, Style, ANSI_INV, ANSI_OFF};

/// A drawn pane: styled spans in rows, measured in columns.
///
/// The panel's own `Line`, unchanged and not re-declared. It is already the
/// thing this port needs — style held beside the text rather than baked into
/// it, so width is a char count and the same rows go to a terminal or a page
/// (`panel/render.rs:36`). A renderer that invented a second cell model would
/// have had to keep it in step with that one, for ever, for nothing.
pub type Frame = Vec<Line>;

/// One pane of a spec, placed.
///
/// No focus flag, and no handle on the runtime. What a renderer is allowed to
/// know about a pane is: which slot it is, what it is called, where it is, and
/// what is in it.
#[derive(Debug, Clone)]
pub struct Cell {
    pub slot: Slot,
    pub label: String,
    pub rect: Rect,
    pub body: Body,
}

/// Cells, plus what the layout could not do.
///
/// Notes rather than errors, for `arrange::Plan`'s reason: a layout that drops
/// a pane silently is indistinguishable from a spec that never asked for one.
#[derive(Debug, Clone, Default)]
pub struct Sheet {
    pub cells: Vec<Cell>,
    pub notes: Vec<String>,
}

impl Sheet {
    /// The cell a slot ended up in, by name — generations are the reconciler's
    /// business and a renderer only ever draws the current one.
    pub fn cell(&self, name: &str) -> Option<&Cell> {
        self.cells.iter().find(|c| c.slot.name == name)
    }
}

/// A rectangle nobody has claimed yet, or one a slot holds.
struct Region {
    owner: Option<Slot>,
    rect: Rect,
}

/// Where a spec's panes end up, given an area to put them in.
///
/// Pure, and the arithmetic is herdr's rather than ours — **the target keeps
/// `ratio` and the new pane gets the remainder** (`arrange::Dir`, learned twice:
/// `panel/install.rs:155` and `panel/verbs.rs:862`). Getting this backwards
/// offline would make every storyboard a picture of a layout we do not ship.
///
/// `into` is the screen we are pretending to have, and it starts unowned. That
/// is what [`Anchor::Widest`] means with no runtime to ask: `arrange` defines it
/// as *the widest pane that is not one of ours*, which offline is the ground
/// nobody has split off yet. Once the ground is used up, `Widest` falls back to
/// the widest pane we placed, and says so — because that is a real divergence
/// from the live path, where a foreign pane would still be there.
pub fn layout(spec: &Spec, into: Rect) -> Sheet {
    let mut out = Sheet::default();
    let mut regions: Vec<Region> = vec![Region { owner: None, rect: into }];

    for want in &spec.panes {
        // A pane on its way out is not a pane to draw. The spec is telling the
        // reconciler to take it down; there is nothing here for us.
        if let Body::Done = want.body {
            continue;
        }

        let at = match &want.at {
            // The root pane of a tab this spec is making. It does not split
            // anything — `tab.create` hands you one pane filling the tab — so
            // it takes the ground whole.
            Anchor::Root => match regions.iter().position(|r| r.owner.is_none()) {
                Some(i) => {
                    regions[i].owner = Some(want.slot.clone());
                    out.cells.push(Cell {
                        slot: want.slot.clone(),
                        label: want.label.clone(),
                        rect: regions[i].rect,
                        body: want.body.clone(),
                    });
                    continue;
                }
                None => {
                    out.notes.push(format!("{} wants the root and it is taken", want.slot));
                    continue;
                }
            },
            Anchor::Widest => {
                let free = widest(&regions, true);
                match free.or_else(|| {
                    let any = widest(&regions, false);
                    if any.is_some() {
                        out.notes.push(format!(
                            "nothing unclaimed to split {} off — taking the widest pane we placed, \
                             which is not what Widest means against a live runtime",
                            want.slot
                        ));
                    }
                    any
                }) {
                    Some(i) => i,
                    None => {
                        out.notes.push(format!("nothing to split {} off", want.slot));
                        continue;
                    }
                }
            }
            Anchor::Slot(s) => {
                match regions.iter().position(|r| r.owner.as_ref().is_some_and(|o| o.same_name(s)))
                {
                    Some(i) => i,
                    None => {
                        out.notes.push(format!("{} hangs off {s}, which is not open", want.slot));
                        continue;
                    }
                }
            }
        };

        match split(regions[at].rect, want.dir, want.ratio) {
            Some((kept, rest)) => {
                regions[at].rect = kept;
                regions.push(Region { owner: Some(want.slot.clone()), rect: rest });
                out.cells.push(Cell {
                    slot: want.slot.clone(),
                    label: want.label.clone(),
                    rect: rest,
                    body: want.body.clone(),
                });
            }
            None => out.notes.push(format!("no room to split {} off", want.slot)),
        }
    }

    out
}

/// The widest region, optionally only the unclaimed ones.
fn widest(regions: &[Region], free_only: bool) -> Option<usize> {
    regions
        .iter()
        .enumerate()
        .filter(|(_, r)| !free_only || r.owner.is_none())
        .max_by_key(|(_, r)| r.rect.w)
        .map(|(i, _)| i)
}

/// The target keeps `ratio`; the new pane gets the remainder.
///
/// Both halves are floored at one cell, so a ratio of 0.98 in a narrow column
/// still leaves something to draw in rather than a pane of width zero — the
/// failure `panel/install.rs:149` measured as seven usable characters, one step
/// further on.
fn split(r: Rect, dir: Dir, ratio: f64) -> Option<(Rect, Rect)> {
    let ratio = ratio.clamp(0.0, 1.0);
    match dir {
        Dir::Right => {
            if r.w < 2 {
                return None;
            }
            let keep = ((r.w as f64 * ratio).round() as u32).clamp(1, r.w - 1);
            Some((
                Rect { w: keep, ..r },
                Rect { x: r.x + keep, y: r.y, w: r.w - keep, h: r.h },
            ))
        }
        Dir::Down => {
            if r.h < 2 {
                return None;
            }
            let keep = ((r.h as f64 * ratio).round() as u32).clamp(1, r.h - 1);
            Some((
                Rect { h: keep, ..r },
                Rect { x: r.x, y: r.y + keep, w: r.w, h: r.h - keep },
            ))
        }
    }
}

/// What the view layer owes a renderer: content in, cells out.
///
/// A trait rather than a function taking a view, so that this port names none
/// of the three view types — see the finding in the module docs, which only
/// stays visible because it is not compiled in here.
///
/// `&mut self` is the one concession, and it is worth its line. A frame is not
/// a pure function of the view: it is a function of the view *and a cursor* —
/// which row is selected, how far the tree has scrolled, whether the tag picker
/// is up. That state is per-slot and belongs to the painter, because the
/// painter is the only thing that knows how tall the pane is
/// (`panel::place(ui, view, w, h)` keeps the scroll offset, and a driver that
/// skipped it would leave the tree deciding its offset from the cursor alone,
/// every key, for ever — `story.rs:421`).
pub trait Paint {
    /// Draw one rendered pane at this size.
    ///
    /// `slot` is passed rather than being implied by the content because two
    /// panes can hold the same view pointed at different things, and their
    /// cursors are not the same cursor.
    fn paint(&mut self, slot: &Slot, content: &Content, size: Rect) -> Frame;
}

/// Where drawn cells go.
///
/// Two methods, and the split between them is 084's rendered/filled line
/// arriving intact at the last layer that could have ignored it.
pub trait Target {
    /// A pane wsp drew.
    fn rendered(&mut self, cell: &Cell, frame: &Frame);
    /// A pane somebody else's process is in. `what` is the occupant, in as many
    /// words as we honestly have — see [`describe`].
    fn filled(&mut self, cell: &Cell, what: &str);
}

/// What a filled pane's occupant is called.
///
/// An agent is named by kind and name and never by its `args`, for
/// `arrange::Filler`'s reason: flags decide what a *new* agent starts with, and
/// a picture of a running one that spells out its preamble is a picture that
/// changes when `cmd_spawn::TRIM` does.
pub fn describe(f: &Filler) -> String {
    match f {
        Filler::Agent(a) => {
            let kind = if a.kind.is_empty() { "agent" } else { &a.kind };
            if a.name.is_empty() {
                kind.to_string()
            } else {
                format!("{kind} on {}", a.name)
            }
        }
        Filler::Command(argv) => argv.join(" "),
    }
}

/// Draw a spec.
///
/// The whole port in nine lines, which is the point: everything above is what
/// had to be decided for this to be nine lines. Returns the layout's notes, so
/// a caller can say why a pane it asked for is not in the picture.
pub fn draw(spec: &Spec, into: Rect, paint: &mut dyn Paint, to: &mut dyn Target) -> Vec<String> {
    let sheet = layout(spec, into);
    for cell in &sheet.cells {
        match &cell.body {
            Body::Rendered(content) => {
                let frame = paint.paint(&cell.slot, content, cell.rect);
                to.rendered(cell, &frame);
            }
            Body::Filled(f) => to.filled(cell, &describe(f)),
            // `layout` already dropped these. Matched rather than caught by a
            // wildcard so that a fourth body cannot be added and silently not
            // drawn.
            Body::Done => {}
        }
    }
    sheet.notes
}

// ---- the terminal ------------------------------------------------------

/// A live pane: escapes, positioned.
///
/// The output `panel::to_ansi` used to build, generalised by one addition —
/// each row is placed at the cell's own column instead of column 1. A spec of
/// one pane at the origin produces the same bytes it always did, which is why
/// `to_ansi` is now this rather than a second implementation that agrees.
#[derive(Debug, Default)]
pub struct Ansi {
    out: String,
}

impl Ansi {
    /// Starting with a cleared screen. What a repaint does; a partial update
    /// would construct this with [`Ansi::default`] instead.
    pub fn fresh() -> Ansi {
        Ansi { out: String::from("\x1b[H\x1b[2J") }
    }

    pub fn finish(self) -> String {
        self.out
    }
}

impl Target for Ansi {
    fn rendered(&mut self, cell: &Cell, frame: &Frame) {
        let (w, h) = (cell.rect.w as usize, cell.rect.h as usize);
        for (i, l) in frame.iter().take(h).enumerate() {
            self.out.push_str(&format!("\x1b[{};{}H", cell.rect.y as usize + i + 1, cell.rect.x + 1));
            let mut l = l.clone();
            l.fit(w);
            for s in &l.spans {
                // Inverse is re-asserted per span rather than wrapped around
                // the row: every span ends with a reset, and a reset clears
                // inverse too, so a single opening `INV` only ever highlighted
                // a selected row up to its first styled run.
                if l.selected {
                    self.out.push_str(ANSI_INV);
                }
                self.out.push_str(ansi_of(s.style));
                self.out.push_str(&s.text);
                self.out.push_str(ANSI_OFF);
            }
        }
    }

    /// Nothing. Something is already running in that pane and drawing over it
    /// is the one thing a renderer must never do — see the module docs.
    fn filled(&mut self, _cell: &Cell, _what: &str) {}
}

// ---- the text block ----------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Ch {
    c: char,
    style: Style,
    sel: bool,
}

impl Default for Ch {
    fn default() -> Ch {
        Ch { c: ' ', style: Style::Plain, sel: false }
    }
}

/// A whole layout in one block of text.
///
/// The target with no runtime under it, and therefore the one that proves
/// something. It composites: cells are written into a grid at their own rects,
/// so a three-pane workspace comes out as a picture of a three-pane workspace
/// rather than three pictures.
///
/// Two outputs from the one grid, and neither is an approximation of the other.
/// [`Block::text`] is what a test asserts against; [`Block::html`] is the
/// storyboard's, built through `panel::to_html_spans` so that the class table
/// has one definition and a colour cannot drift between the page and the
/// terminal.
#[derive(Debug, Clone)]
pub struct Block {
    grid: Vec<Vec<Ch>>,
    w: usize,
}

impl Block {
    pub fn new(size: Rect) -> Block {
        let (w, h) = (size.w as usize, size.h as usize);
        Block { grid: vec![vec![Ch::default(); w]; h], w }
    }

    /// Put one styled char down, if it is on the block at all.
    fn put(&mut self, x: usize, y: usize, ch: Ch) {
        if y < self.grid.len() && x < self.w {
            self.grid[y][x] = ch;
        }
    }

    fn write(&mut self, x: usize, y: usize, style: Style, text: &str) {
        for (i, c) in text.chars().enumerate() {
            self.put(x + i, y, Ch { c, style, sel: false });
        }
    }

    /// Trailing blanks trimmed. A test that asserts on a frame should not have
    /// to know how wide the pane it was drawn in happened to be.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for row in &self.grid {
            let line: String = row.iter().map(|c| c.c).collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    /// The same grid as styled spans, in the storyboard's shape.
    ///
    /// Runs are regrouped into a `Line` and handed to the panel's own span
    /// writer rather than emitting the markup here — the one place the two
    /// outputs could drift is the style-to-class table, so neither of them owns
    /// a copy of it.
    pub fn html(&self) -> String {
        let mut out = String::from("<pre class=\"wsp\">");
        for row in &self.grid {
            let mut i = 0;
            while i < row.len() {
                let mut j = i + 1;
                while j < row.len() && row[j].style == row[i].style && row[j].sel == row[i].sel {
                    j += 1;
                }
                let mut l = Line::default();
                l.push(row[i].style, row[i..j].iter().map(|c| c.c).collect::<String>());
                out.push_str(if row[i].sel { "<span class=\"sel\">" } else { "<span>" });
                out.push_str(&to_html_spans(&l));
                out.push_str("</span>");
                i = j;
            }
            out.push('\n');
        }
        out.push_str("</pre>");
        out
    }
}

impl Target for Block {
    fn rendered(&mut self, cell: &Cell, frame: &Frame) {
        let (x0, y0) = (cell.rect.x as usize, cell.rect.y as usize);
        let (w, h) = (cell.rect.w as usize, cell.rect.h as usize);
        for (i, l) in frame.iter().take(h).enumerate() {
            let mut l = l.clone();
            l.fit(w);
            let mut x = 0;
            for s in &l.spans {
                for c in s.text.chars() {
                    self.put(x0 + x, y0 + i, Ch { c, style: s.style, sel: l.selected });
                    x += 1;
                }
            }
        }
    }

    /// A labelled box, because nothing else is going to draw here.
    ///
    /// The limit of what a storyboard can prove, drawn as a limit rather than
    /// left as a gap: a reader who sees a box knows there is a live process
    /// there and that its output is not a function of anything wsp holds. A
    /// reader who sees blank cells thinks the layout is wrong.
    fn filled(&mut self, cell: &Cell, what: &str) {
        let (x, y) = (cell.rect.x as usize, cell.rect.y as usize);
        let (w, h) = (cell.rect.w as usize, cell.rect.h as usize);
        if w < 2 || h < 2 {
            return;
        }
        let bar = "─".repeat(w - 2);
        self.write(x, y, Style::Dim, &format!("┌{bar}┐"));
        for i in 1..h - 1 {
            self.write(x, y + i, Style::Dim, "│");
            self.write(x + w - 1, y + i, Style::Dim, "│");
        }
        self.write(x, y + h - 1, Style::Dim, &format!("└{bar}┘"));

        let inner = w - 4;
        let mid = y + h / 2;
        if h >= 4 {
            let label = if cell.label.is_empty() { cell.slot.name.clone() } else { cell.label.clone() };
            self.write(x + 2, mid.saturating_sub(1), Style::Muted, &crate::util::truncate(&label, inner));
        }
        self.write(x + 2, mid, Style::Dim, &crate::util::truncate(what, inner));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arrange::{Slot, Want};
    use crate::panel::line;

    const SCREEN: Rect = Rect { x: 0, y: 0, w: 80, h: 24 };

    fn rendered(name: &str, view: &str) -> Want {
        Want::new(Slot::new(name), Body::Rendered(Content::new(view))).labelled(name)
    }

    fn filled(name: &str, argv: &[&str]) -> Want {
        let cmd = Filler::Command(argv.iter().map(|s| s.to_string()).collect());
        Want::new(Slot::new(name), Body::Filled(cmd)).labelled(name)
    }

    /// A painter over hand-built data and nothing else.
    ///
    /// This is the falsification, and it is a compile-time one: the whole
    /// renderer runs against a struct with no store in it, no socket, and no
    /// clock. A target that needed any of the three could not be written this
    /// way, and the test would not exist to be read.
    struct Fixture {
        rows: Vec<String>,
        asked: Vec<(String, Rect)>,
    }

    impl Paint for Fixture {
        fn paint(&mut self, slot: &Slot, content: &Content, size: Rect) -> Frame {
            self.asked.push((slot.name.clone(), size));
            let mut out = vec![line(Style::Bold, format!("{} {}", content.view, slot.name))];
            for r in &self.rows {
                out.push(line(Style::Plain, r.clone()));
            }
            out
        }
    }

    fn fixture() -> Fixture {
        Fixture { rows: vec!["one".into(), "two".into()], asked: Vec::new() }
    }

    #[test]
    fn a_spec_of_one_root_pane_takes_the_whole_area() {
        let spec = Spec::new(vec![rendered("panel", "panel").at(Anchor::Root, Dir::Right, 0.5)]);
        let sheet = layout(&spec, SCREEN);
        assert_eq!(sheet.cells.len(), 1);
        assert_eq!(sheet.cells[0].rect, SCREEN);
    }

    #[test]
    fn the_target_keeps_the_ratio_and_the_new_pane_gets_the_remainder() {
        // The trap `panel/install.rs:155` learned by putting the sidebar on the
        // wrong side at the wrong width. A layout that got this backwards would
        // draw storyboards of a product we do not ship.
        let spec = Spec::new(vec![rendered("panel", "panel").at(Anchor::Widest, Dir::Right, 0.75)]);
        let sheet = layout(&spec, SCREEN);
        let panel = sheet.cell("panel").expect("placed");
        assert_eq!(panel.rect, Rect { x: 60, y: 0, w: 20, h: 24 });
    }

    #[test]
    fn a_pane_hanging_off_a_slot_splits_that_slot_and_not_the_ground() {
        let spec = Spec::new(vec![
            rendered("panel", "panel").at(Anchor::Root, Dir::Right, 0.5),
            rendered("view", "detail").at(Anchor::Slot(Slot::new("panel")), Dir::Down, 0.5),
        ]);
        let sheet = layout(&spec, SCREEN);
        // The whole screen was the panel's; the view comes out of it and not
        // out of some other region.
        assert_eq!(sheet.cell("view").unwrap().rect, Rect { x: 0, y: 12, w: 80, h: 12 });
    }

    #[test]
    fn widest_offline_means_the_ground_nobody_has_split_off_yet() {
        // Two panes anchored Widest in a row. The second must come out of what
        // is left of the ground, not out of the first — which is what "the
        // widest pane that is not one of ours" says against a live runtime.
        let spec = Spec::new(vec![
            rendered("panel", "panel").at(Anchor::Widest, Dir::Right, 0.75),
            rendered("view", "detail").at(Anchor::Widest, Dir::Right, 0.5),
        ]);
        let sheet = layout(&spec, SCREEN);
        assert_eq!(sheet.cell("panel").unwrap().rect.w, 20);
        assert_eq!(sheet.cell("view").unwrap().rect, Rect { x: 30, y: 0, w: 30, h: 24 });
        assert!(sheet.notes.is_empty(), "{:?}", sheet.notes);
    }

    #[test]
    fn taking_the_widest_pane_of_our_own_is_said_rather_than_done_quietly() {
        let spec = Spec::new(vec![
            rendered("panel", "panel").at(Anchor::Root, Dir::Right, 0.5),
            rendered("view", "detail").at(Anchor::Widest, Dir::Right, 0.5),
        ]);
        let sheet = layout(&spec, SCREEN);
        assert_eq!(sheet.cells.len(), 2);
        assert!(
            sheet.notes.iter().any(|n| n.contains("not what Widest means")),
            "{:?}",
            sheet.notes
        );
    }

    #[test]
    fn a_pane_the_spec_has_finished_with_is_not_drawn_by_anybody() {
        let spec = Spec::new(vec![
            rendered("panel", "panel").at(Anchor::Root, Dir::Right, 0.5),
            Want::new(Slot::new("editor"), Body::Done).labelled("editor"),
        ]);
        let mut paint = fixture();
        let mut block = Block::new(SCREEN);
        draw(&spec, SCREEN, &mut paint, &mut block);
        assert_eq!(paint.asked.len(), 1);
        assert!(!block.text().contains("editor"), "{}", block.text());
    }

    #[test]
    fn a_filled_pane_is_a_labelled_box_on_a_page_and_nothing_at_all_on_a_terminal() {
        // The two halves of one rule: a page has to say a live process is
        // there, and a terminal must not paint over it.
        let spec = Spec::new(vec![
            rendered("panel", "panel").at(Anchor::Root, Dir::Right, 0.5),
            filled("agent", &["claude"]).at(Anchor::Slot(Slot::new("panel")), Dir::Right, 0.4),
        ]);

        let mut block = Block::new(SCREEN);
        draw(&spec, SCREEN, &mut fixture(), &mut block);
        let page = block.text();
        assert!(page.contains("agent"), "{page}");
        assert!(page.contains("claude"), "{page}");
        assert!(page.contains('┌'), "{page}");

        let mut term = Ansi::fresh();
        draw(&spec, SCREEN, &mut fixture(), &mut term);
        let out = term.finish();
        assert!(!out.contains("claude"), "{out:?}");
        assert!(out.contains("panel panel"), "{out:?}");
    }

    #[test]
    fn the_painter_is_asked_for_the_same_cells_whichever_target_is_drawing() {
        // The pair is the point. If the two targets could ask the view for
        // different things, a storyboard would stop being evidence about the
        // thing we ship.
        let spec = Spec::new(vec![
            rendered("panel", "panel").at(Anchor::Widest, Dir::Right, 0.6),
            rendered("view", "detail").at(Anchor::Widest, Dir::Right, 0.5),
        ]);
        let mut to_page = fixture();
        draw(&spec, SCREEN, &mut to_page, &mut Block::new(SCREEN));
        let mut to_term = fixture();
        draw(&spec, SCREEN, &mut to_term, &mut Ansi::fresh());
        assert_eq!(to_page.asked, to_term.asked);
        assert_eq!(to_page.asked.len(), 2);
    }

    #[test]
    fn a_pane_is_drawn_whether_or_not_anything_is_focused() {
        // Ed, 2026-08-17: never rely on focus to implement automation. There is
        // nowhere in a `Cell` to read it from, so this asserts the consequence:
        // the same spec draws the same picture under either focus line.
        let panes = vec![
            rendered("panel", "panel").at(Anchor::Root, Dir::Right, 0.5),
            rendered("view", "detail").at(Anchor::Slot(Slot::new("panel")), Dir::Right, 0.5),
        ];
        let mut a = Spec::new(panes.clone());
        a.focus = Some(Slot::new("panel"));
        let mut b = Spec::new(panes);
        b.focus = None;

        let mut pa = Block::new(SCREEN);
        draw(&a, SCREEN, &mut fixture(), &mut pa);
        let mut pb = Block::new(SCREEN);
        draw(&b, SCREEN, &mut fixture(), &mut pb);
        assert_eq!(pa.text(), pb.text());
        assert!(pa.text().contains("detail view"), "{}", pa.text());
    }

    #[test]
    fn a_whole_workspace_is_drawn_with_nothing_running() {
        // What the storyboard is for, and the claim the partition rests on:
        // three panes, two views, one agent, no store and no socket.
        let spec = Spec::new(vec![
            rendered("panel", "panel").at(Anchor::Widest, Dir::Right, 0.7),
            rendered("view", "detail").at(Anchor::Slot(Slot::new("panel")), Dir::Down, 0.5),
            filled("agent", &["claude"]).at(Anchor::Widest, Dir::Right, 0.5),
        ]);
        let mut block = Block::new(SCREEN);
        let notes = draw(&spec, SCREEN, &mut fixture(), &mut block);
        assert!(notes.is_empty(), "{notes:?}");
        let page = block.text();
        assert!(page.contains("panel panel"), "{page}");
        assert!(page.contains("detail view"), "{page}");
        assert!(page.contains("claude"), "{page}");
    }

    #[test]
    fn a_cell_at_the_origin_writes_the_escapes_the_panel_always_wrote() {
        // `panel::to_ansi` is this, with one cell. The assertion is the row
        // positioning, because that is the only thing generalising it changed.
        let mut term = Ansi::fresh();
        let cell = Cell {
            slot: Slot::new("panel"),
            label: "panel".into(),
            rect: SCREEN,
            body: Body::Rendered(Content::new("panel")),
        };
        term.rendered(&cell, &vec![line(Style::Plain, "hello")]);
        let out = term.finish();
        assert!(out.starts_with("\x1b[H\x1b[2J\x1b[1;1H"), "{out:?}");
    }

    #[test]
    fn a_cell_away_from_the_origin_writes_at_its_own_column() {
        let mut term = Ansi::default();
        let cell = Cell {
            slot: Slot::new("view"),
            label: "view".into(),
            rect: Rect { x: 40, y: 5, w: 20, h: 3 },
            body: Body::Rendered(Content::new("detail")),
        };
        term.rendered(&cell, &vec![line(Style::Plain, "hello")]);
        assert!(term.finish().starts_with("\x1b[6;41H"));
    }

    #[test]
    fn a_pane_too_narrow_to_split_is_reported_rather_than_drawn_at_zero_width() {
        let spec = Spec::new(vec![
            rendered("panel", "panel").at(Anchor::Root, Dir::Right, 0.5),
            rendered("view", "detail").at(Anchor::Slot(Slot::new("panel")), Dir::Right, 0.5),
        ]);
        let sheet = layout(&spec, Rect { x: 0, y: 0, w: 1, h: 10 });
        assert_eq!(sheet.cells.len(), 1);
        assert!(sheet.notes.iter().any(|n| n.contains("no room")), "{:?}", sheet.notes);
    }

    #[test]
    fn the_page_carries_the_selected_row_and_the_classes_the_panel_uses() {
        // The storyboard's half of the pair. Not a golden-file test — what is
        // worth pinning is that the block goes out through the panel's own
        // class table, and that a selected row survives compositing as a run
        // rather than as a wrapper round the whole line, which is what lets a
        // second pane sit beside it on the same row.
        let mut block = Block::new(Rect { x: 0, y: 0, w: 12, h: 2 });
        let cell = Cell {
            slot: Slot::new("panel"),
            label: "panel".into(),
            rect: Rect { x: 0, y: 0, w: 12, h: 2 },
            body: Body::Rendered(Content::new("panel")),
        };
        let mut sel = line(Style::Accent, "a<b");
        sel.selected = true;
        block.rendered(&cell, &vec![sel, line(Style::Dim, "two")]);

        let html = block.html();
        assert!(html.contains("<span class=\"sel\">"), "{html}");
        assert!(html.contains("class=\"a\">a&lt;b</span>"), "{html}");
        assert!(html.contains("class=\"d\">two</span>"), "{html}");
    }

    #[test]
    fn an_agent_is_named_by_what_it_is_and_never_by_its_flags() {
        // `arrange::Filler` refuses to compare `args` so that a reconcile does
        // not kill a session over a changed preamble. A picture that spelled
        // them out would reintroduce the same coupling in the storyboard.
        let a = crate::place::Agent {
            kind: "claude".into(),
            name: "t-260816-092".into(),
            args: vec!["--dangerously-skip-permissions".into()],
        };
        let said = describe(&Filler::Agent(a));
        assert_eq!(said, "claude on t-260816-092");
    }
}
