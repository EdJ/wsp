//! The environment variable a seat's occupant finds its seat id in.

/// The environment variable a seat's occupant finds its own handle in, for a
/// backend that has no name of its own for it.
///
/// **Not a thing wsp writes.** The module docs say why the arrow points the
/// other way: the backend puts this in the seat's environment, because the
/// backend is the only party that knows the seat's name at the moment the
/// environment is fixed. herdr has its own name for the same string and
/// `place_herdr` reads that instead; this is the convention for a supervisor or
/// a runtime arriving without one, so that not every new backend invents a
/// variable and every adapter has to learn it.
///
/// `WSP_SEAT_ID` rather than `WSP_SEAT` because **`WSP_SEAT` is taken, and by
/// the other sense of the word.** `executor/wsp` reads it as the ssh Host alias
/// of the seat *machine* — the one you sit at — and refuses to run without it,
/// and the README's install tells you to set it in every shell an agent might
/// run in. On an executor the two would be one variable in one environment with
/// a pane id on one side and a host alias on the other, and whichever won the
/// other would be silently wrong: the shim would ssh to `w0:p3`, or an agent
/// would claim a seat called `seat-mini`. Renaming the const was one line
/// because nothing read it yet; renaming the shim's would have been every
/// executor's shell profile.
pub const SEAT_ENV: &str = "WSP_SEAT_ID";
