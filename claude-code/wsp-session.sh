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
# a hook that gets deleted within a week. Every path exits 0, but one: the kill
# guard below (PreToolUse) exits 2 to refuse a command that would signal half
# the machine.

set -eu

action="${1:-}"

# PreToolUse on Bash: the kill guard (wsp-691), and the one path here that can
# say no. On macOS `pkill`, `pgrep` and `killall` use BSD getopt, which stops at
# the first non-option, so every word after the first pattern is one more
# pattern: on 2026-11-19 a verifier's `pkill -f "scripts/check.mjs" -P 0` meant
# "check.mjs, or -P, or 0", matched nearly every process on the machine and
# ended every Claude session on it within ten seconds.
#
# Refused, with exit 2 and the reason on stderr (which Claude Code shows the
# agent instead of running the command):
#   - `pkill`/`killall`, or a `pgrep` in a command that also runs `kill` (`pgrep
#     … | xargs kill`, `kill $(pgrep …)`), with any option after the first
#     pattern;
#   - one of them naming a bare common name: claude, node, wsp, compound-sup,
#     compound-render, cargo;
#   - a `pkill` (or killing `pgrep`) pattern shorter than 4 characters, unless
#     `-x` makes it exact;
#   - a `pkill` or `killall` with no pattern and nothing that narrows it to a
#     pidfile, a parent other than 0 or 1, a process group or a tty (`pkill -u
#     me` is every process you own).
# Looked through: `VAR=x`, sudo, env, nice, nohup, timeout, xargs, command,
# exec, `sh -c '…'` and `eval`. Everything else, `kill $(cat pidfile)` and
# `pkill -f "vite --port 5199"` included, runs.
#
# Every session is guarded, seated or not: the machine is everybody's. A
# command with no `kill` in it costs one `cat` and a `case`. Anything that
# fails to read is allowed: the guard is for the slip, not for an adversary.
# Tested by claude-code/test-kill-guard.sh.
if [ "$action" = PreToolUse ]; then
  payload="$(cat 2>/dev/null || true)"
  case "$payload" in *kill*) ;; *) exit 0 ;; esac
  why="$(printf '%s' "$payload" | awk '
function base(p) { sub(/.*\//, "", p); return p }

function hexval(h,   i, v, c) {
  v = 0
  for (i = 1; i <= length(h); i++) {
    c = index("0123456789abcdef", tolower(substr(h, i, 1)))
    if (c == 0) return 63
    v = v * 16 + c - 1
  }
  return v
}

# The Bash tool input'"'"'s `command`, as the JSON string unescaped.
function json_command(s,   i, rest, out, c, n, v) {
  i = index(s, "\"tool_input\"")
  if (i == 0) return ""
  rest = substr(s, i)
  if (!match(rest, /"command"[ \t\r\n]*:[ \t\r\n]*"/)) return ""
  rest = substr(rest, RSTART + RLENGTH)
  out = ""; n = length(rest)
  for (i = 1; i <= n; i++) {
    c = substr(rest, i, 1)
    if (c == "\"") break
    if (c == "\\") {
      i++; c = substr(rest, i, 1)
      if (c == "n") c = "\n"
      else if (c == "t") c = "\t"
      else if (c == "r") c = "\r"
      else if (c == "b" || c == "f") c = " "
      else if (c == "u") { v = hexval(substr(rest, i + 1, 4)); i += 4; c = (v > 0 && v < 128) ? sprintf("%c", v) : "?" }
    }
    out = out c
  }
  return out
}

# Shell words and separators: typ "w" a word, "s" a separator (; & | ( ) `
# newline, and $( ), "r" a redirection, whose target word is not an argument.
function tokenize(s, tok, typ,   n, i, c, q, w, have, nx, L) {
  n = 0; w = ""; have = 0; q = ""; L = length(s)
  for (i = 1; i <= L; i++) {
    c = substr(s, i, 1)
    if (q == "\047") { if (c == "\047") q = ""; else w = w c; continue }
    if (q == "\"") {
      if (c == "\"") { q = ""; continue }
      if (c == "\\") {
        nx = substr(s, i + 1, 1)
        if (nx == "\\" || nx == "\"" || nx == "$" || nx == "`") { w = w nx; i++; continue }
        if (nx == "\n") { i++; continue }
      }
      w = w c; continue
    }
    if (c == "\\") { nx = substr(s, i + 1, 1); i++; if (nx != "\n") { w = w nx; have = 1 }; continue }
    if (c == "\047" || c == "\"") { q = c; have = 1; continue }
    if (c == " " || c == "\t" || c == "\r") { if (have) { tok[++n] = w; typ[n] = "w"; w = ""; have = 0 }; continue }
    if (c == "#" && !have) { while (i <= L && substr(s, i, 1) != "\n") i++; i--; continue }
    if (c == "<" || c == ">") {
      if (have && w ~ /^[0-9]+$/) { w = ""; have = 0 }
      if (have) { tok[++n] = w; typ[n] = "w"; w = ""; have = 0 }
      tok[++n] = c; typ[n] = "r"; continue
    }
    if (c == ";" || c == "&" || c == "|" || c == "(" || c == ")" || c == "`" || c == "\n" || (c == "$" && substr(s, i + 1, 1) == "(")) {
      if (have) { tok[++n] = w; typ[n] = "w"; w = ""; have = 0 }
      if (c == "$") i++
      tok[++n] = c; typ[n] = "s"; continue
    }
    w = w c; have = 1
  }
  if (have) { tok[++n] = w; typ[n] = "w" }
  return n
}

function common(a) {
  return a == "claude" || a == "node" || a == "wsp" || a == "compound-sup" || a == "compound-render" || a == "cargo"
}

function pidwise() {
  return "Kill by pid instead: `kill $(cat <pidfile>)` for what you started (`echo $! > <pidfile>`), or the pids `lsof -t -i :<port>` or `lsof -t +D <dir>` gives."
}

function after(name, pat, opt) {
  return "wsp refused this command: `" name "` stops reading options at its first pattern on macOS (BSD getopt), so `" opt "` after `" pat "` is read as one more pattern, which matches nearly every process on the machine. On 2026-11-19 `pkill -f \"scripts/check.mjs\" -P 0` ended every Claude session on it. Put every option before the pattern (`" name " -P <ppid> -f \"<pattern>\"`). " pidwise()
}

function bare(name, a) {
  return "wsp refused this command: `" name " " a "` stops every " a " on the machine, every other seat'"'"'s included. " pidwise()
}

function short(name, a) {
  return "wsp refused this command: the `" name "` pattern `" a "` is under 4 characters and matches far more than your process. Name it by something only it has (`pkill -f \"vite --port 5199\"`), or: " pidwise()
}

function unnarrowed(name) {
  return "wsp refused this command: `" name "` with no pattern, a pidfile, a parent, a process group or a tty matches every process it is allowed to signal. " pidwise()
}

# Whether option `o` of wrapper `b` takes the next word as its value.
function valued(b, o) {
  if (b == "sudo" || b == "doas") return o ~ /^-[ugCchpU]$/
  if (b == "env") return o ~ /^-[uPS]$/
  if (b == "nice") return o == "-n"
  if (b == "timeout" || b == "gtimeout") return o ~ /^-[sk]$/
  if (b == "xargs") return o ~ /^-[IJLnPsEdRS]$/
  return 0
}

# pkill or pgrep from word i on: "" or why it is refused.
function pk(w, nw, i, name,   j, a, dd, seen, k, ch, letters, exact, narrow, nxt) {
  dd = 0; seen = ""; exact = 0; narrow = 0
  for (j = i + 1; j <= nw; j++) {
    a = w[j]
    if (!dd && a == "--") { dd = 1; continue }
    if (!dd && a ~ /^-./) {
      if (seen != "") return after(name, seen, a)
      if (a ~ /^--/) {
        if (a ~ /^--(parent|pidfile|pgroup|terminal)/) narrow = 1
        if (a !~ /=/ && a ~ /^--(signal|parent|pgroup|session|terminal|euid|uid|group|pidfile|ns|nslist|older|newer|runstates|cgroup)$/) j++
        if (a == "--exact") exact = 1
        continue
      }
      if (a ~ /^-[0-9]+$/ || (length(a) > 2 && a ~ /^-[A-Z][A-Z0-9]+$/)) continue
      letters = substr(a, 2)
      for (k = 1; k <= length(letters); k++) {
        ch = substr(letters, k, 1)
        if (ch == "x") exact = 1
        if (index("FGMNPUgtucsJ", ch)) {
          nxt = (k == length(letters)) ? w[j + 1] : substr(letters, k + 1)
          if (ch == "F" || ch == "g" || ch == "t" || (ch == "P" && nxt != "0" && nxt != "1")) narrow = 1
          if (k == length(letters)) j++
          break
        }
      }
      continue
    }
    if (seen == "") seen = a
    if (common(a)) return bare(name, a)
    if (length(a) < 4 && !exact) return short(name, a)
  }
  if (seen == "" && !narrow && name == "pkill") return unnarrowed(name)
  return ""
}

function ka(w, nw, i,   j, a, dd, seen, k, ch, letters) {
  dd = 0; seen = ""
  for (j = i + 1; j <= nw; j++) {
    a = w[j]
    if (!dd && a == "--") { dd = 1; continue }
    if (!dd && a ~ /^-./) {
      if (seen != "") return after("killall", seen, a)
      if (a ~ /^--/) { if (a !~ /=/ && a ~ /^--(signal|user|older-than|younger-than|ns|context)$/) j++; continue }
      if (a == "-help" || a ~ /^-[0-9]+$/ || (length(a) > 2 && a ~ /^-(SIG)?[A-Z][A-Z0-9]+$/)) continue
      letters = substr(a, 2)
      for (k = 1; k <= length(letters); k++) {
        ch = substr(letters, k, 1)
        if (index("utcoynZ", ch)) { if (k == length(letters)) j++; break }
      }
      continue
    }
    if (seen == "") seen = a
    if (common(a)) return bare("killall", a)
  }
  if (seen == "") return unnarrowed("killall")
  return ""
}

# One simple command, words 1..nw: "" or why it is refused.
function simple(w, nw, haskill, depth,   i, h, b, j, sc) {
  i = 1
  while (i <= nw) {
    h = w[i]; b = base(h)
    if (h ~ /^[A-Za-z_][A-Za-z0-9_]*=/) { i++; continue }
    if (h == "!" || h == "{" || h == "}" || h == "if" || h == "then" || h == "do" || h == "else" || h == "elif" || h == "while" || h == "until" || h == "time" || h == "command" || h == "builtin" || h == "exec" || h == "nohup") { i++; continue }
    if (b == "sudo" || b == "doas" || b == "env" || b == "nice" || b == "timeout" || b == "gtimeout" || b == "xargs" || b == "caffeinate") {
      i++
      while (i <= nw && w[i] ~ /^-/) { if (valued(b, w[i])) i++; i++ }
      if ((b == "timeout" || b == "gtimeout") && i <= nw) i++
      continue
    }
    break
  }
  if (i > nw) return ""
  b = base(w[i])
  if (b == "pkill") return pk(w, nw, i, "pkill")
  if (b == "pgrep") return haskill ? pk(w, nw, i, "pgrep") : ""
  if (b == "killall") return ka(w, nw, i)
  if ((b == "sh" || b == "bash" || b == "zsh" || b == "dash" || b == "ksh") && depth < 4) {
    for (j = i + 1; j < nw; j++) {
      if (w[j] !~ /^-/) break
      if (w[j] ~ /^-[a-zA-Z]*c[a-zA-Z]*$/) return check(w[j + 1], depth + 1)
    }
  }
  if (b == "eval" && depth < 4) {
    sc = ""
    for (j = i + 1; j <= nw; j++) sc = sc " " w[j]
    return check(sc, depth + 1)
  }
  return ""
}

function check(s, depth,   tok, typ, n, i, haskill, words, nw, r, skip) {
  n = tokenize(s, tok, typ)
  haskill = 0
  for (i = 1; i <= n; i++) if (typ[i] == "w" && base(tok[i]) == "kill") haskill = 1
  nw = 0; skip = 0
  for (i = 1; i <= n + 1; i++) {
    if (i <= n && typ[i] == "w") { if (skip) { skip = 0; continue }; words[++nw] = tok[i]; continue }
    if (i <= n && typ[i] == "r") { skip = 1; continue }
    if (nw > 0) { r = simple(words, nw, haskill, depth); if (r != "") return r }
    nw = 0; skip = 0
  }
  return ""
}

{ doc = doc $0 "\n" }
END {
  cmd = json_command(doc)
  if (cmd != "") { r = check(cmd, 0); if (r != "") print r }
}
' 2>/dev/null || true)"
  [ -n "$why" ] || exit 0
  printf '%s\n' "$why" >&2
  exit 2
fi

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

# A `claude -p` started from inside a seat — by the agent, or by a script it
# ran — inherits WSP_SEAT_ID and so reports here under its parent's seat. This
# hook does not try to tell: `wsp report` judges every hook by its session_id
# and ignores one from a session other than the seat's (wsp-364). A skip on
# CLAUDE_CODE_CHILD_SESSION would not be enough alone — nothing sets it for a
# script that calls `claude` directly — and under herdr it can be ambient in
# every pane. A script starting an agent from a seat should still unset
# WSP_SEAT_ID and HERDR_PANE_ID for it.


# What the agent is doing, in its own words, recorded against the seat it is
# standing in. Silent, and silent by construction rather than by redirection —
# `wsp report` prints nothing and exits 0 outside a seat — but stdout from a
# SessionStart hook lands in the session's context, so nothing is left to trust.
report() {
  printf '%s' "$payload" | wsp report "$1" >/dev/null 2>&1 || true
}

# `compound-sup submitted <socket>` with the hook payload on stdin. The socket
# is the seat's session socket, which is in its record (`socket`), not in its
# environment. Best-effort like everything here: no sup, no record, no socket,
# a far seat — nothing to say, and nothing to fail.
submitted() {
  command -v compound-sup >/dev/null 2>&1 || return 0
  rec="${WSP_STATE:-$HOME/.local/state/wsp}/compound-seats/$WSP_SEAT_ID/seat.json"
  [ -f "$rec" ] || return 0
  grep -q '"machine"[[:space:]]*:[[:space:]]*"[^"]' "$rec" 2>/dev/null && return 0
  sock="$(sed -n 's/.*"socket"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$rec" 2>/dev/null | head -n 1)"
  [ -n "$sock" ] || return 0
  printf '%s' "$payload" | compound-sup submitted "$sock" >/dev/null 2>&1 || true
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
  PermissionHold)
    # The one hook here that says something back. A PermissionRequest fires with
    # the dialog already on the pane's screen and runs this beside it: `wsp
    # report --hold` raises the dialog as a question for the person (an
    # AskUserQuestion's options, a plan, a tool's permission), waits for `wsp
    # answer`, and prints the decision for Claude Code to act on. Whoever
    # answers first wins, the pane or the person, so the pane is never taken
    # away.
    #
    # **Anything but an answer prints nothing and exits 0**, which is what
    # Claude Code reads as "no opinion" and leaves the dialog to the pane: a
    # wait that ran out, a store that is gone, no row to ask about, no `wsp`.
    # Never a deny, and the settings entry must never say `onFailure: block`.
    #
    # `$2` is the `timeout` the settings entry gives this hook, in seconds, so
    # that `wsp` stops waiting a little before Claude Code would kill it and
    # can say so on the question. Keep the two equal.
    printf '%s' "$payload" | wsp report PermissionRequest --hold "${2:-86400}" 2>/dev/null || true
    ;;
  UserPromptSubmit)
    # wsp-477: the seat's supervisor is told the prompt was submitted, which is
    # what acknowledges a `compound-sup deliver` (exactly once). Local seats
    # only: a far seat's session socket is on the far machine, and its record
    # here names a `machine`. Then reported as every other hook is.
    submitted
    report "$action"
    ;;
  *)
    # UserPromptSubmit, Stop, StopFailure, PermissionRequest, Elicitation — the
    # hook's own name, passed through. `wsp report` knows which of them mean
    # something and ignores the rest, so a hook wired here in error costs a
    # process and changes nothing.
    #
    # PostToolUse is here too and says nothing about the agent: it is what
    # closes a question the held hook above raised, when the pane's dialog was
    # answered first (wsp-302). Claude Code does not signal that hook, so the
    # tool running afterwards is the only word there is. Stop, StopFailure,
    # UserPromptSubmit and SessionEnd close it too, as abandoned.
    report "$action"
    ;;
esac

exit 0
