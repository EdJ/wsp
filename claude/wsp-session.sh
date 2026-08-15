#!/bin/sh
# wsp ↔ Claude Code: hand a session its brief on the way in.
#
# Installed *beside* herdr's own hook rather than into it — that file says in
# its header that it is overwritten on every herdr update, and it is right to
# say so.
#
# Called as `wsp-session.sh start`, with the hook payload on stdin. Everything
# here is best-effort: a hook that fails a session, or delays one, is a hook
# that gets deleted within a week. Every path exits 0.

set -eu

action="${1:-}"
payload="$(cat 2>/dev/null || true)"

# A subagent shares its parent's pane, and therefore its parent's claim and its
# parent's brief. It needs neither, and if it ever grows a SessionEnd it must
# not release work the session above it is still doing. The key is present and
# null for a top-level session, so the test is for a non-empty string.
if printf '%s' "$payload" | grep -qE '"agent_id"[[:space:]]*:[[:space:]]*"'; then
  exit 0
fi

command -v wsp >/dev/null 2>&1 || exit 0

case "$action" in
  start)
    # stdout from a SessionStart hook is added to the session's context. This
    # is the whole integration: an agent opens knowing its project, the task it
    # holds, what is settled, what is next, and who else is in the tree.
    wsp brief 2>/dev/null || true
    ;;
  end)
    # Deliberately nothing. A claim outlives the pane that made it — that is
    # the design, and `/clear` ends a session without ending the work. Auto
    # releasing here would drop a claim mid-task every time you cleared the
    # screen, and leave `wsp wip` unable to tell you that a task is underway
    # with nobody on it, which is exactly the signal it exists to give.
    ;;
esac

exit 0
