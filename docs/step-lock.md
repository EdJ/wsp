# A list's step holds its list (`wsp-663`)

## What went wrong

On 2026-11-17 `daemon.log` was full of
`wsp: state lock busy for 2s — carrying on without it`. Two
`wsp worklist advance --task compounding-724 --verb verified` processes ran
the same step side by side, and one of them spawned `compounding-709` a second
time. A commit of `tasks/compounding-690.md tasks/compounding-727.md` failed
in the same storm.

There were two causes:

1. **Nothing serialised a step.** The state lock (`Store::locked`) guards one
   file's read and write, which takes microseconds. A step can take minutes:
   it reads the group, decides what is owed, and spawns and tells. Two steps
   of one list could each read a member as unstarted between their lock-taking
   writes. `take_member` re-reads under the lock, so a duplicate start was
   usually caught, but only usually: once a lock is carried on past, it
   catches nothing.
2. **The fallback broke the lock for everyone.** After two seconds, `locked`
   carried on without the lock. As it left, it also removed the lock file,
   which belonged to the process that did hold it. The next process in then
   took the lock while that holder was still inside. One impatient process
   turned into many processes inside the lock at once.

## What a step does now

- `cycle::step` is the only way into a step, from `advance` and from the
  daemon's tick. It holds `<state>/stepping/<list>.lock` (`<pid> <epoch>`) for
  as long as it runs (`src/stepping.rs`). The lock is per list, so lists do
  not wait on each other.
- A second trigger for a list that is already being stepped does not wait and
  does not step. It writes `<list>.again` and returns. Before the holder lets
  go, it reads the mark and takes one more step. If many triggers arrive
  during one step, they leave one mark and get one follow-up step. No trigger
  is lost, and none piles up waiting.
- The last moment of a step is the one that could lose a mark. A trigger
  marks first and then tries the lock again. The holder lets go first and
  then looks for the mark. Whichever order the two happen in, one of them
  sees the other.
- A step's help to other lists, the verifier and barrier starts taken before
  members fan out under the seat cap, uses the same lock but leaves no mark.
  If the other list is busy, it is skipped: its holder is taking those starts
  itself. Because nothing waits on another list, two lists cannot deadlock.
- A lock is broken only when its pid is dead, or after 30 minutes (pid
  reuse). A step that panics gives its lock back on unwind.

## The global state lock's fallback: kept, but not for a step

`locked` still carries on after two seconds outside a step. A one-off verb
such as `wsp claim` or `wsp say` that never returns wedges an agent, and a
lost update there is something the next write or the daemon's repair puts
right. That trade is still the right one for the verbs.

It is the wrong trade for anything that spawns or tells, because a second
spawn or a second tell is never undone by a later write. So
`wsp_store::store::strictly` marks a scope where `locked` waits:

- it waits until the lock is free, or until its holder is broken as stale
  (30s, a dead holder);
- it says `state lock busy for Ns — a step waits for it` every ten seconds;
- it never says `carrying on without it`.

`cycle::step` and the whole of `cycle::advance` run inside `strictly`.

In both modes, a `locked` call removes the lock file only if it took the
lock. A call that carried on without it leaves the file to its holder.
