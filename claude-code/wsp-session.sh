#!/bin/sh
# wsp ↔ Claude Code: hand a session its brief on the way in, and let a session
# wsp is hosting say what it is doing.
#
# Installed *beside* herdr's own hook rather than into it — that file says in
# its header that it is overwritten on every herdr update, and it is right to
# say so.
#
# Called as `wsp-session.sh <hook>`, with the hook payload on stdin, where
# <hook> is Claude Code's own event name. `start` and `end` are the two older
# spellings and still mean SessionStart and SessionEnd, because they are what is
# already wired into somebody's settings.json.
#
# Everything here is best-effort: a hook that fails a session, or delays one, is
# a hook that gets deleted within a week. Every path exits 0.

set -eu

action="${1:-}"

# The reporting hooks fire several times a turn, in every Claude Code on this
# machine, most of which are in a herdr pane and are none of wsp's business. So
# the cheapest possible answer for them comes first: no seat variable, nothing
# to report, and not even a `cat` of the payload is paid for.
#
# SessionStart is exempt because the brief below is owed to every session,
# seated or not.
case "$action" in
  start|SessionStart) ;;
  *) [ -n "${WSP_SEAT_ID:-}" ] || exit 0 ;;
esac

payload="$(cat 2>/dev/null || true)"

# A subagent shares its parent's pane, and therefore its parent's claim and its
# parent's brief. It needs neither, and it must not report either: a subagent's
# Stop would tell the supervisor the seat is idle while the session above it is
# in the middle of a turn. The key is present and null for a top-level session,
# so the test is for a non-empty string.
if printf '%s' "$payload" | grep -qE '"agent_id"[[:space:]]*:[[:space:]]*"'; then
  exit 0
fi

command -v wsp >/dev/null 2>&1 || exit 0

# What the agent is doing, in its own words, recorded against the seat it is
# standing in. Silent, and silent by construction rather than by redirection —
# `wsp report` prints nothing and exits 0 outside a seat — but stdout from a
# SessionStart hook lands in the session's context, so nothing is left to trust.
report() {
  printf '%s' "$payload" | wsp report "$1" >/dev/null 2>&1 || true
}

case "$action" in
  start|SessionStart)
    # stdout from a SessionStart hook is added to the session's context. This
    # is the whole integration: an agent opens knowing its project, the task it
    # holds, what is settled, what is next, and who else is in the tree.
    #
    # `--session` is the payload mode, and this hook is what it was added for.
    # Every request in a session re-reads the whole context, so a token here is
    # paid by every request that follows — which is exactly why the context an
    # agent would otherwise go and *fetch* belongs here rather than in its
    # fourth request. Measured on t-260816-096: 14,450 tokens of `wsp show`
    # arriving over requests 4–16 and then carried for another ~86 anyway.
    #
    # Plain `wsp brief` stays the smaller thing an agent types mid-session.
    # Do not swap this for `--session` there, and do not make it the default.
    wsp brief --session 2>/dev/null || true
    # And, for a supervised agent, the announcement that closes its launch
    # window. Under herdr this is what a spinner glyph in a terminal title is
    # read for; here the agent simply says it.
    report SessionStart
    ;;
  end|SessionEnd)
    # No release, deliberately. A claim outlives the pane that made it — that is
    # the design, and `/clear` ends a session without ending the work. Auto
    # releasing here would drop a claim mid-task every time you cleared the
    # screen, and leave `wsp wip` unable to tell you that a task is underway
    # with nobody on it, which is exactly the signal it exists to give.
    #
    # What is new is the report, and it is not the same thing: it says the agent
    # in this seat has stopped, which is a fact about the process. The claim is
    # a fact about the work and is `wsp despawn`'s to end.
    report SessionEnd
    ;;
  *)
    # UserPromptSubmit, Stop, StopFailure, PermissionRequest, Elicitation — the
    # hook's own name, passed through. `wsp report` knows which of them mean
    # something and ignores the rest, so a hook wired here in error costs a
    # process and changes nothing.
    report "$action"
    ;;
esac

exit 0
