//! Keys off the terminal, parsed rather than guessed at.
//!
//! What stood here read one byte after `ESC`, one more after `[`, and matched
//! `A`–`D`. Everything longer than that ran past the end of the read and came
//! back as typing: `ESC [ 5 ~` for page-up left a `~` in the buffer, which the
//! next read delivered as a keystroke, which a prompt then put in a task
//! title. `ESC [ 1 ; 5 A` — ctrl-up, which xterm sends and every multiplexer
//! forwards — left `;5A` behind, three of them.
//!
//! So: a state machine over bytes, following the grammar rather than a fixed
//! length. A sequence is consumed whole whether or not anything here knows
//! what it means, and the ones with no meaning are dropped in silence. That is
//! the whole of the fix — the parse is worth more than the four keys currently
//! read out of it, because [`Csi`] is what a mouse report is, and giving those
//! meaning is the next task along.
//!
//! Bare `ESC` is the one thing bytes cannot settle: it is a key, and it is
//! also the start of every sequence. `stty min 0 time 1` makes a read with
//! nothing waiting return empty after ~100ms, and [`Keys::idle`] is that
//! silence — an `ESC` with nothing behind it was the key.

/// A key as typed, not as interpreted. `j` used to arrive already meaning
/// "down", which is unanswerable once a prompt needs a literal `j` — so the
/// meaning is decided by the reducer, which knows the mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Backspace,
    /// The far ends of the list, for anyone who does not think in `g`/`G`.
    Home,
    End,
    Char(char),
    /// A press, in cells, 0-based from the top-left of the pane. The terminal
    /// counts from 1; the correction happens here so nothing downstream has to
    /// remember which convention it is holding.
    Click { x: usize, y: usize },
    /// The wheel, which reports as buttons 64 and 65 rather than as motion.
    Wheel { up: bool },
    /// Ctrl-C, which raw mode delivers as a byte rather than a signal.
    Interrupt,
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
/// Enough for anything a keyboard sends; a longer one is malformed, and
/// dropping its tail beats growing a buffer on whatever arrives.
const MAX_PARAMS: usize = 16;

/// One control sequence, as the grammar carries it, before anything decides
/// what it means: `ESC [` · private marker · parameters · intermediates ·
/// final byte.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Csi {
    /// `<`, `=`, `>` or `?` when the sequence is a private one. This is what
    /// tells an SGR mouse report from a cursor key.
    pub(crate) private: Option<u8>,
    /// `;`-separated, an omitted one reading as 0 — which is what the standard
    /// means by a default.
    pub(crate) params: Vec<u16>,
    /// The letter at the end, which is mostly what a sequence *is*.
    pub(crate) final_byte: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Ground,
    /// `ESC` seen. What follows decides whether it was a key or a prefix.
    Esc,
    /// Inside `ESC [` … final byte.
    Csi,
    /// Inside `ESC O` — the same arrows, from a terminal in application cursor
    /// mode, which is a mode herdr's panes can be left in.
    Ss3,
    /// The bytes after `ESC [ M`, which sit *outside* the grammar: an X10
    /// mouse report puts three raw bytes after its final, and they are the one
    /// thing a parser that only followed the rules would still leak.
    X10(u8),
    /// Inside a string sequence — `OSC`, `DCS`, `APC`, `PM` — which runs to
    /// `BEL` or `ESC \` and carries a reply to something we never asked.
    Str,
    /// `ESC` inside a string sequence: `\` ends it, anything else does not.
    StrEsc,
}

/// The terminal's byte stream, one key at a time.
pub(crate) struct Keys {
    state: State,
    seq: Csi,
    /// The parameter under construction. `Some` also means "a parameter has
    /// been started", so `ESC [ 1 ; A` keeps its empty second slot.
    pending: Option<u16>,
}

impl Keys {
    pub(crate) fn new() -> Keys {
        Keys { state: State::Ground, seq: Csi::default(), pending: None }
    }

    /// One byte from the terminal. Emits nothing until a sequence is whole,
    /// and can emit two: `ESC` followed by an ordinary character is a real
    /// `ESC` and then that character, in that order, which is what typing fast
    /// after pressing it looks like.
    pub(crate) fn feed(&mut self, b: u8, out: &mut Vec<Key>) {
        match self.state {
            State::Ground if b == ESC => self.state = State::Esc,
            State::Ground => ground(b, out),
            State::Esc => match b {
                b'[' => {
                    self.seq = Csi::default();
                    self.pending = None;
                    self.state = State::Csi;
                }
                b'O' => self.state = State::Ss3,
                b'P' | b']' | b'^' | b'_' => self.state = State::Str,
                // Two escapes: the first one was the key.
                ESC => out.push(Key::Esc),
                _ => {
                    out.push(Key::Esc);
                    self.state = State::Ground;
                    ground(b, out);
                }
            },
            State::Csi => self.csi(b, out),
            State::Ss3 => {
                self.state = State::Ground;
                if let Some(k) = key_of(&Csi { final_byte: b, ..Csi::default() }) {
                    out.push(k);
                }
            }
            State::X10(n) => self.state = if n > 1 { State::X10(n - 1) } else { State::Ground },
            State::Str => match b {
                BEL => self.state = State::Ground,
                ESC => self.state = State::StrEsc,
                _ => {}
            },
            State::StrEsc => match b {
                b'\\' => self.state = State::Ground,
                ESC => {}
                _ => self.state = State::Str,
            },
        }
    }

    /// The read came back with nothing. An `ESC` waiting on this was the key.
    ///
    /// A half-finished sequence is abandoned rather than held, because a
    /// terminal writes one in a single go: if 100ms of silence has landed in
    /// the middle of it, what is in hand is not a sequence, and keeping the
    /// state would eat the next thing typed as parameters.
    pub(crate) fn idle(&mut self, out: &mut Vec<Key>) {
        if let State::Esc = self.state {
            out.push(Key::Esc);
        }
        self.state = State::Ground;
    }

    fn csi(&mut self, b: u8, out: &mut Vec<Key>) {
        match b {
            // A private marker counts in the first position only; later it is
            // part of a sequence nothing here was going to read anyway.
            0x3c..=0x3f => {
                if self.seq.private.is_none() && self.seq.params.is_empty() && self.pending.is_none()
                {
                    self.seq.private = Some(b);
                }
            }
            b'0'..=b'9' => {
                let v = self.pending.unwrap_or(0);
                self.pending = Some(v.saturating_mul(10).saturating_add((b - b'0') as u16));
            }
            // `:` separates sub-parameters rather than parameters. Nothing
            // here reads one, and flattening them keeps the count honest.
            b';' | b':' => {
                if self.seq.params.len() < MAX_PARAMS {
                    self.seq.params.push(self.pending.unwrap_or(0));
                }
                self.pending = Some(0);
            }
            // Intermediates are part of the grammar and part of nothing we
            // read, so they are consumed and forgotten.
            0x20..=0x2f => {}
            // `ESC [ M` with nothing in front of it is an X10 mouse report.
            b'M' if self.seq.private.is_none()
                && self.seq.params.is_empty()
                && self.pending.is_none() =>
            {
                self.state = State::X10(3);
            }
            0x40..=0x7e => {
                if let Some(v) = self.pending.take() {
                    if self.seq.params.len() < MAX_PARAMS {
                        self.seq.params.push(v);
                    }
                }
                self.seq.final_byte = b;
                self.state = State::Ground;
                if let Some(k) = key_of(&self.seq) {
                    out.push(k);
                }
            }
            // `CAN` and `SUB` abandon a sequence by definition; an `ESC`
            // starts a new one. Anything else at this point is not a sequence.
            0x18 | 0x1a => self.state = State::Ground,
            ESC => self.state = State::Esc,
            _ => self.state = State::Ground,
        }
    }
}

/// A byte with no sequence around it.
fn ground(b: u8, out: &mut Vec<Key>) {
    let k = match b {
        3 => Key::Interrupt,
        b'\r' | b'\n' => Key::Enter,
        0x7f | 0x08 => Key::Backspace,
        c if c.is_ascii_graphic() || c == b' ' => Key::Char(c as char),
        _ => return,
    };
    out.push(k);
}

/// What a finished sequence means here, if anything.
///
/// A modifier arrives as a second parameter — `ESC [ 1 ; 5 A` is ctrl-up — and
/// is read for identity, not for the shift state: the key is still the arrow,
/// and nothing in the panel wants to know it was held with ctrl. Everything
/// unanswered returns `None`, which is a sequence consumed and dropped rather
/// than one spilled into whatever is reading keys.
fn key_of(seq: &Csi) -> Option<Key> {
    // An SGR mouse report: `ESC [ < button ; col ; row M` for a press, `m` for
    // the release. This is what the private marker was being carried for.
    //
    // Only the press is answered. A release carries the same coordinates and
    // answering both would do everything twice — and the release of a click
    // that started somewhere else is not an event about this row at all.
    if seq.private == Some(b'<') {
        let b = seq.params.first().copied().unwrap_or(0);
        let x = seq.params.get(1).copied().unwrap_or(1).saturating_sub(1) as usize;
        let y = seq.params.get(2).copied().unwrap_or(1).saturating_sub(1) as usize;
        if seq.final_byte != b'M' {
            return None;
        }
        // Bit 6 marks the wheel; the low two bits then say which way. Modifier
        // bits (4 shift, 8 alt, 16 ctrl) are masked off — the panel has nothing
        // that a shift-click should mean, and letting them through would make
        // a click with a stray modifier silently do nothing.
        return match b & 0b1100_0011 {
            64 => Some(Key::Wheel { up: true }),
            65 => Some(Key::Wheel { up: false }),
            0 => Some(Key::Click { x, y }),
            _ => None,
        };
    }
    // Every other private sequence is a terminal reply to something we never
    // asked. Parsed so it cannot leak; not answered.
    if seq.private.is_some() {
        return None;
    }
    match seq.final_byte {
        b'A' => Some(Key::Up),
        b'B' => Some(Key::Down),
        b'C' => Some(Key::Right),
        b'D' => Some(Key::Left),
        b'H' => Some(Key::Home),
        b'F' => Some(Key::End),
        // The numbered keys, which differ by terminal on which number is which
        // — `1`/`7` and `4`/`8` are both live in the wild. The rest of them
        // (insert, delete, the function keys, bracketed paste) mean nothing
        // here and are dropped whole.
        b'~' => match seq.params.first().copied().unwrap_or(0) {
            1 | 7 => Some(Key::Home),
            4 | 8 => Some(Key::End),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<Key> {
        let mut k = Keys::new();
        let mut out = Vec::new();
        for b in bytes {
            k.feed(*b, &mut out);
        }
        out
    }

    /// What a sequence leaves behind matters as much as what it yields: the
    /// bug this replaces was a tail arriving as typing.
    fn then_typing(bytes: &[u8]) -> Vec<Key> {
        let mut k = Keys::new();
        let mut out = Vec::new();
        for b in bytes {
            k.feed(*b, &mut out);
        }
        k.feed(b'x', &mut out);
        out
    }

    #[test]
    fn ordinary_bytes() {
        assert_eq!(keys(b"a Z"), vec![Key::Char('a'), Key::Char(' '), Key::Char('Z')]);
        assert_eq!(keys(b"\r"), vec![Key::Enter]);
        assert_eq!(keys(&[0x7f]), vec![Key::Backspace]);
        assert_eq!(keys(&[3]), vec![Key::Interrupt]);
    }

    #[test]
    fn arrows_both_ways() {
        let arrows = vec![Key::Up, Key::Down, Key::Right, Key::Left];
        assert_eq!(keys(b"\x1b[A\x1b[B\x1b[C\x1b[D"), arrows);
        assert_eq!(keys(b"\x1bOA\x1bOD"), vec![Key::Up, Key::Left]);
    }

    #[test]
    fn modifiers_still_mean_the_arrow() {
        assert_eq!(then_typing(b"\x1b[1;5A"), vec![Key::Up, Key::Char('x')]);
        assert_eq!(then_typing(b"\x1b[1;2;3D"), vec![Key::Left, Key::Char('x')]);
    }

    #[test]
    fn home_and_end_by_either_spelling() {
        assert_eq!(keys(b"\x1b[H\x1b[F"), vec![Key::Home, Key::End]);
        let ends = vec![Key::Home, Key::End, Key::Home, Key::End];
        assert_eq!(keys(b"\x1b[1~\x1b[4~\x1b[7~\x1b[8~"), ends);
    }

    /// The regression this module exists for: page-up used to eat the `[`, drop
    /// the `5`, and hand `~` to whatever was reading.
    #[test]
    fn unknown_sequences_are_eaten_whole() {
        assert_eq!(then_typing(b"\x1b[5~"), vec![Key::Char('x')]);
        assert_eq!(then_typing(b"\x1b[6~"), vec![Key::Char('x')]);
        assert_eq!(then_typing(b"\x1b[200~"), vec![Key::Char('x')]);
        assert_eq!(then_typing(b"\x1b[?1;2c"), vec![Key::Char('x')]);
    }

    #[test]
    fn a_press_is_a_click_and_a_release_is_nothing() {
        // ESC [ < 0 ; 13 ; 4 M — button 0 down at column 13, row 4.
        assert_eq!(keys(b"\x1b[<0;13;4M"), vec![Key::Click { x: 12, y: 3 }]);
        // The release carries the same place and must not do it twice.
        assert_eq!(keys(b"\x1b[<0;13;4m"), vec![]);
    }

    #[test]
    fn the_wheel_reports_as_a_button() {
        assert_eq!(keys(b"\x1b[<64;1;1M"), vec![Key::Wheel { up: true }]);
        assert_eq!(keys(b"\x1b[<65;1;1M"), vec![Key::Wheel { up: false }]);
    }

    /// A click with a stray modifier held should still be a click on that row.
    #[test]
    fn modifier_bits_do_not_swallow_a_click() {
        for b in [4u8, 8, 16, 28] {
            let seq = format!("\x1b[<{};5;5M", b);
            assert_eq!(
                keys(seq.as_bytes()),
                vec![Key::Click { x: 4, y: 4 }],
                "button byte {b} should still read as button 0"
            );
        }
    }

    /// The right and middle buttons mean nothing here, and must not be read as
    /// the left one.
    #[test]
    fn other_buttons_are_dropped() {
        assert_eq!(then_typing(b"\x1b[<1;5;5M"), vec![Key::Char('x')]);
        assert_eq!(then_typing(b"\x1b[<2;5;5M"), vec![Key::Char('x')]);
    }

    /// The X10 form still leaves nothing. Its three bytes sit *outside* the
    /// grammar, after the final, so it is the one report that has to be
    /// consumed by counting rather than by parsing — and it is not answered,
    /// because no terminal sends it once SGR has been asked for.
    #[test]
    fn the_x10_report_is_still_eaten_whole() {
        assert_eq!(then_typing(b"\x1b[M \x21\x22"), vec![Key::Char('x')]);
    }

    /// SGR reports used to be dropped; now the press is a click. What must not
    /// change is that nothing of the sequence survives into the text stream.
    #[test]
    fn an_sgr_report_leaves_no_tail_behind() {
        assert_eq!(
            then_typing(b"\x1b[<0;12;34M\x1b[<0;12;34m"),
            vec![Key::Click { x: 11, y: 33 }, Key::Char('x')]
        );
    }

    /// A terminal reply to a query nobody here made — colour, cursor position,
    /// a device attribute — is a string sequence, and runs to its terminator.
    #[test]
    fn string_sequences_run_to_their_terminator() {
        assert_eq!(then_typing(b"\x1b]11;rgb:1c1c/1c1c/1c1c\x07"), vec![Key::Char('x')]);
        assert_eq!(then_typing(b"\x1bP+q544e\x1b\\"), vec![Key::Char('x')]);
    }

    #[test]
    fn esc_is_a_key_when_nothing_follows() {
        let mut k = Keys::new();
        let mut out = Vec::new();
        k.feed(ESC, &mut out);
        assert!(out.is_empty(), "esc waits to see what it was");
        k.idle(&mut out);
        assert_eq!(out, vec![Key::Esc]);
    }

    #[test]
    fn esc_then_typing_delivers_both_in_order() {
        assert_eq!(keys(b"\x1bq"), vec![Key::Esc, Key::Char('q')]);
        assert_eq!(keys(b"\x1b\x1b"), vec![Key::Esc]);
    }

    /// Silence in the middle of a sequence means it was never one. The state
    /// goes, so the next keystroke is a keystroke and not a parameter.
    #[test]
    fn a_torn_sequence_is_abandoned() {
        let mut k = Keys::new();
        let mut out = Vec::new();
        for b in b"\x1b[1;" {
            k.feed(*b, &mut out);
        }
        k.idle(&mut out);
        assert!(out.is_empty());
        k.feed(b'q', &mut out);
        assert_eq!(out, vec![Key::Char('q')]);
    }

    /// A parameter list long enough to be malformed must not grow a buffer,
    /// and must not stop the sequence being consumed.
    #[test]
    fn absurd_parameter_lists_are_bounded() {
        let mut s = String::from("\x1b[");
        for _ in 0..200 {
            s.push_str("99999999;");
        }
        s.push('A');
        assert_eq!(then_typing(s.as_bytes()), vec![Key::Up, Key::Char('x')]);
    }
}
