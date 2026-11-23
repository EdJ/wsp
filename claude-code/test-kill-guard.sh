#!/bin/sh
# Tests for the kill guard in wsp-session.sh (wsp-691): each command is sent as
# Claude Code's PreToolUse payload for the Bash tool, and the hook must refuse
# it (exit 2, the reason on stderr) or let it run (exit 0, silent).
#
#   sh claude-code/test-kill-guard.sh
#
# Nothing here is run: the hook only reads the command. Run by
# `cargo test the_kill_guard_shell_tests_pass` too.

set -u

here="$(cd "$(dirname "$0")" && pwd)"
hook="$here/wsp-session.sh"
fails=0
count=0

# The command as a JSON string: backslashes, quotes, tabs and newlines escaped.
json() {
  printf '%s' "$1" | awk '
    { gsub(/\\/, "\\\\"); gsub(/"/, "\\\""); gsub(/\t/, "\\t"); out = out (NR > 1 ? "\\n" : "") $0 }
    END { printf "%s", out }'
}

run() {
  printf '{"session_id":"t","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"%s","description":"x"}}' "$(json "$1")" |
    env -u WSP_SEAT_ID sh "$hook" PreToolUse 2>"$err"
}

err="$(mktemp "${TMPDIR:-/tmp}/kill-guard.XXXXXX")"
trap 'rm -f "$err"' EXIT

# refuse <command> <text the reason must hold>
refuse() {
  count=$((count + 1))
  run "$1"
  code=$?
  if [ "$code" -ne 2 ]; then
    fails=$((fails + 1)); printf 'FAIL refused? exit %s for: %s\n' "$code" "$1"
  elif ! grep -qF -- "$2" "$err"; then
    fails=$((fails + 1)); printf 'FAIL reason for: %s\n  wanted: %s\n  got: %s\n' "$1" "$2" "$(cat "$err")"
  elif ! grep -qF 'Kill by pid' "$err" || ! grep -qF 'lsof' "$err"; then
    fails=$((fails + 1)); printf 'FAIL no pid advice for: %s\n' "$1"
  fi
}

allow() {
  count=$((count + 1))
  run "$1"
  code=$?
  if [ "$code" -ne 0 ] || [ -s "$err" ]; then
    fails=$((fails + 1)); printf 'FAIL allowed? exit %s for: %s\n  said: %s\n' "$code" "$1" "$(cat "$err")"
  fi
}

after='Put every option before the pattern'

# The 11-19 command, and the forms of it.
refuse 'pkill -f "scripts/check.mjs" -P 0' "$after"
refuse 'pkill -f "scripts/check.mjs" -P 0' '`-P` after `scripts/check.mjs`'
refuse "pkill -f 'scripts/check.mjs' -P0" "$after"
refuse 'cd ~/claude/wsp && pkill -f scripts/check.mjs -9' "$after"
refuse 'sudo pkill -f "vite --port 5199" -u ed' "$after"
refuse 'FOO=1 timeout 5 pkill -f long-pattern -TERM' "$after"
refuse 'pkill -f long-pattern -- -P' 'under 4 characters'
refuse 'killall myserver -9' "$after"
refuse 'killall -9 myserver -u ed' "$after"
refuse 'pgrep -f "scripts/check.mjs" -P 0 | xargs kill' "$after"
refuse 'pgrep -f "scripts/check.mjs" -P 0 | xargs -n1 kill -9' "$after"
refuse 'kill $(pgrep -f "scripts/check.mjs" -P 0)' "$after"
refuse 'kill `pgrep -f scripts/check.mjs -P 0`' "$after"
refuse 'sh -c '\''pkill -f "scripts/check.mjs" -P 0'\''' "$after"
refuse 'bash -lc "pkill -f scripts/check.mjs -P 0"' "$after"
refuse 'eval pkill -f scripts/check.mjs -P 0' "$after"
refuse 'true; pkill -f "scripts/check.mjs" -P 0 2>/dev/null || true' "$after"
refuse 'for i in 1 2; do
  pkill -f "scripts/check.mjs" -P 0
done' "$after"

# killall of a bare common name, and pkill of one.
for name in claude node wsp compound-sup compound-render cargo; do
  refuse "killall $name" "stops every $name on the machine"
done
refuse 'killall -9 node' 'stops every node'
refuse '/usr/bin/killall -KILL claude' 'stops every claude'
refuse 'pkill node' 'stops every node'
refuse 'pkill -f claude' 'stops every claude'
refuse 'pgrep node | xargs kill' 'stops every node'

# A short pkill pattern.
refuse 'pkill -f ab' 'under 4 characters'
refuse 'pkill -9 -f "yes"' 'under 4 characters'
refuse 'pkill vi' 'under 4 characters'
refuse 'kill $(pgrep -f ts)' 'under 4 characters'

# Nothing narrowing it at all.
refuse 'pkill -u ed' 'no pattern'
refuse 'pkill -P 1' 'no pattern'
refuse 'killall -u ed' 'no pattern'

# What still runs: narrow, well-formed kills.
allow 'pkill -f "vite --port 5199"'
allow 'kill $(cat pidfile)'
allow 'kill $(cat "$WSP_SCRATCH/vite.pid") 2>/dev/null || true'
allow 'kill -TERM 4123 4124'
allow 'pkill -P 4123'
allow 'pkill -F /tmp/vite.pid'
allow 'pkill -9 -P 4123 -f "vite --port 5199"'
allow 'pkill -f "scripts/check.mjs" 2>/dev/null'
allow 'pkill -x yes'
allow 'pgrep -f "vite --port 5199" | xargs kill'
allow 'lsof -t -i :5199 | xargs kill'
allow 'killall my-dev-server'
allow 'killall -9 my-dev-server'
allow 'pgrep -f "scripts/check.mjs" -P 0'
allow 'while kill -0 4123; do sleep 5; done'

# Commands that only mention a kill.
allow 'echo killall node'
allow 'git commit -m "refuse pkill -f x -P 0 and killall node"'
allow 'grep -n "pkill\|killall" claude-code/wsp-session.sh'
allow 'man pkill'
allow 'cargo test kill_guard'
allow 'ls -la # pkill -f x -P 0'

# A payload the guard cannot read is let through.
count=$((count + 1))
printf 'not json at all, but pkill' | sh "$hook" PreToolUse 2>"$err" || { fails=$((fails + 1)); echo 'FAIL unreadable payload refused'; }
count=$((count + 1))
printf '' | sh "$hook" PreToolUse 2>"$err" || { fails=$((fails + 1)); echo 'FAIL empty payload refused'; }

if [ "$fails" -ne 0 ]; then
  printf '%s of %s kill-guard cases failed\n' "$fails" "$count"
  exit 1
fi
printf 'kill guard: %s cases pass\n' "$count"
