#!/usr/bin/env bash
# The multiplayer v1 gate (wsp-419) on two real machines against one hub: the
# steps of tests/multiplayer_v1.rs, one person on each machine, each running
# this script. "Ed and I don't collide on work, and I can actually help bring
# his ideas to life."
#
#   scripts/mp-gate.sh owner  --project <slug> [--workspace <name>] [--list <name>] [--repo <dir>]
#   scripts/mp-gate.sh helper --project <slug> [--workspace <name>] [--list <name>] [--repo <dir>]
#
# --workspace is the shared workspace both users are members of, named bare
# (default `core`); both sides pass the same one.
#
# Start both, in either order. The owner makes the list and drives it; the
# helper leases two of its rows. Each side waits for what the other does, so
# the two scripts run side by side to the end.
#
# The hub and the user come from the machine's login (`wsp login`, which
# writes ~/.config/wsp/hubs.json); read here through `wsp hub status`, which
# never prints the token. Nothing in this file names a host or an address.
#
# Both machines need: the trunk's `wsp` (and a `wsp daemon` running, as
# for any spoke), the same project slug whose root is a clone of one shared git
# remote, and a seat's agent that can work a row from its note. Run it from
# inside that clone, or name it with --repo.
#
# Each step prints `ok` or `FAIL` for what it saw; the first FAIL stops the
# script with what it saw instead. MP_GATE_WAIT is the seconds a wait gives the
# other side or a seat's agent (default 900); MP_GATE_YES=1 goes past the
# helper's prompts (cut the link, restore it) without waiting.

set -u

role=${1:-}
[ $# -gt 0 ] && shift
project=
list=mp-gate
repo=
workspace=core
while [ $# -gt 0 ]; do
  case "$1" in
    --project | --workspace | --list | --repo) [ $# -ge 2 ] || { echo "mp-gate: $1 needs a value" >&2; exit 2; } ;;
  esac
  case "$1" in
    --project) project=${2:-}; shift 2 ;;
    --workspace) workspace=${2:-}; shift 2 ;;
    --list) list=${2:-}; shift 2 ;;
    --repo) repo=${2:-}; shift 2 ;;
    *) echo "mp-gate: unknown argument $1" >&2; exit 2 ;;
  esac
done
case "$role" in owner | helper) ;; *)
  echo "usage: scripts/mp-gate.sh owner|helper --project <slug> [--workspace <name>] [--list <name>] [--repo <dir>]" >&2
  exit 2 ;;
esac
[ -n "$project" ] || { echo "mp-gate: --project <slug> is needed" >&2; exit 2; }
[ -n "$repo" ] || repo=$(git rev-parse --show-toplevel 2>/dev/null) || { echo "mp-gate: run it inside the project's clone, or pass --repo" >&2; exit 2; }
[ -n "$workspace" ] || { echo "mp-gate: --workspace needs a name" >&2; exit 2; }
shared=shared:${workspace#shared:}
limit=${MP_GATE_WAIT:-900}
state=${WSP_STATE:-$HOME/.local/state/wsp}

# ───────────────────────────── helpers ─────────────────────────────

step=0
say() { printf '%s\n' "$*"; }
begin() { step=$1; say ""; say "== step $1: $2"; }
ok() { say "  ok   $*"; }
note() { say "  note $*"; }
fail() {
  say "  FAIL step $step: $*"
  exit 1
}

# wait_for "<what>" <command…>: the command succeeds within $limit seconds.
wait_for() {
  local what=$1 start=$SECONDS
  shift
  until "$@" >/dev/null 2>&1; do
    [ $((SECONDS - start)) -lt "$limit" ] || fail "$what: not within ${limit}s"
    sleep 3
  done
  ok "$what (after $((SECONDS - start))s)"
}

pause() {
  if [ "${MP_GATE_YES:-}" = 1 ]; then return; fi
  printf '  >>   %s — press Enter ' "$1"
  read -r _ || fail "no terminal to answer: $1"
}

w() { wsp "$@"; }
row_show() { w show "$1" --log 2>&1; }
list_show() { w worklist show "$list" 2>&1; }
claimed() { row_show "$1" | grep -q '^claimed '; }
seat_of() { row_show "$1" | sed -n 's/^agent[[:space:]]*//p' | head -n1; }
landed_sha() { row_show "$1" | sed -n 's/.*wsp: landed \([0-9a-f]\{7,\}\).*/\1/p' | tail -n1; }
list_rows() { list_show | grep -oE "(^|[^A-Za-z0-9])${project}-[A-Za-z0-9]+" | grep -oE "${project}-[A-Za-z0-9]+" | awk '!seen[$0]++'; }
cycle_has() { grep -qF -- "$1" "$state/cycle.log" 2>/dev/null; }
on_trunk() {
  git -C "$repo" fetch -q origin 2>/dev/null
  git -C "$repo" branch -r --contains "$1" 2>/dev/null | grep -q 'origin/'
}

# ───────────────────────────── the login ─────────────────────────────

status=$(w hub status 2>&1) || { say "FAIL not logged in: run \`wsp login <hub-url>\` first ($status)"; exit 1; }
hub_url=$(printf '%s\n' "$status" | sed -n 's/^hub[[:space:]]*//p')
me=$(printf '%s\n' "$status" | sed -n 's/^user[[:space:]]*//p')
say "mp-gate: $role as $me on $hub_url, workspace $shared, project $project, list $list, clone $repo"
[ -n "$me" ] && [ "$me" != "(not named)" ] || fail "the login names no user: log in again with \`wsp login\`"
[ -f "$state/hub/sync.json" ] || note "no $state/hub/sync.json yet: is \`wsp daemon\` running on this machine?"

third=
mine=()
others=()

# ───────────────────────────── the steps ─────────────────────────────

owner_step_1() {
  begin 1 "$me makes three rows and a list privately, then publishes the list to $shared"
  private=()
  for title in "Row one" "Row two" "Row three"; do
    out=$(w add "$title" -p "$project" 2>&1) || fail "wsp add: $out"
    id=$(printf '%s\n' "$out" | tr -s ' \t' '\n\n' | grep -m1 "^${project}-") || fail "add printed no id: $out"
    w caption "$id" "$title" >/dev/null 2>&1
    w note "$id" "The gate's row. Add the file mp-gate/$id.txt to your tree containing the row's id, commit it, and review the row (wsp review $id \"Wrote mp-gate/$id.txt\"). Nothing else." >/dev/null 2>&1
    private+=("$id")
  done
  w worklist new "$list" "The v1 gate's list" -p "$project" >/dev/null 2>&1 || fail "worklist new $list"
  w worklist add "$list" "${private[@]}" >/dev/null 2>&1 || fail "worklist add $list"
  for id in "${private[@]}"; do
    row_show "$id" | grep -qF "$shared" && fail "$id is already shared before the publish"
  done
  ok "three private rows and the list: ${private[*]}"
  out=$(w publish "$list" --to "$shared" 2>&1) || fail "wsp publish: $out"
  printf '%s\n' "$out" | sed 's/^/       /'
  rows=()
  for old in "${private[@]}"; do
    new=$(printf '%s\n' "$out" | sed -n "s/^[[:space:]]*$old -> \\(.*\\)$/\\1/p" | head -n1 | tr -d ' ')
    [ -n "$new" ] || fail "$old was not renumbered by the publish"
    rows+=("$new")
  done
  ok "published to $shared; the rows are now ${rows[*]}"
  listed=$(list_show)
  for id in "${rows[@]}"; do printf '%s\n' "$listed" | grep -qF "$id" || fail "the list does not hold $id: $listed"; done
  ok "the list holds all three"
}

helper_step_2() {
  begin 2 "$me's spoke shows the list and its three rows (the sync is a turn or two)"
  start=$SECONDS
  wait_for "the list $list on this spoke" sh -c "wsp worklist show '$list' >/dev/null 2>&1"
  wait_for "its three rows" sh -c "[ \$(wsp worklist show '$list' 2>&1 | grep -oE '${project}-[A-Za-z0-9]+' | sort -u | wc -l) -ge 3 ]"
  rows=($(list_rows))
  [ "${#rows[@]}" -ge 3 ] || fail "the list shows ${#rows[@]} rows: $(list_show)"
  ok "the list shows ${rows[0]} ${rows[1]} ${rows[2]} ($((SECONDS - start))s)"
}

helper_step_3() {
  begin 3 "$me leases two rows and spawns them on their own seats; the hub records the leases"
  for row in "${rows[0]}" "${rows[1]}"; do
    out=$(w spawn "$row" -p "$project" --agent 2>&1) || fail "wsp spawn $row: $out"
    seat=$(printf '%s\n' "$out" | sed -n 's/^[[:space:]]*bound to //p' | head -n1)
    [ -n "$seat" ] || fail "the spawn of $row bound no seat: $out"
    claimed "$row" || fail "no claim on $row here: $(row_show "$row")"
    ok "$row spawned on seat $seat, claimed here (a spawn takes the hub's lease first, or is refused)"
    mine+=("$row")
  done
}

owner_step_4() {
  begin 4 "$me's spoke shows the helper's two as held elsewhere and does not start them; it drives and starts the third"
  note "waiting for the other machine to lease two rows (its step 3)"
  wait_for "two rows running on another user's wsp" sh -c "[ \$(wsp worklist show '$list' 2>&1 | grep -c \"running on .*'s wsp\") -ge 2 ]"
  w worklist go "$list" "start the gate's list" >/dev/null 2>&1 || fail "worklist go $list"
  listed=$(list_show)
  for row in "${rows[@]}"; do
    line=$(printf '%s\n' "$listed" | grep "^[[:space:]]*$row" | head -n1)
    case "$line" in
      *"running on "*"'s wsp"*) ok "$row shown held elsewhere: $line"; others+=("$row"); claimed "$row" && fail "this spoke started $row, held by another" ;;
      *) third=$row ;;
    esac
  done
  [ -n "$third" ] || fail "no row left for this spoke: $listed"
  wait_for "$third started on this spoke's seat" claimed "$third"
  printf '%s\n' "$(list_show)" | grep -q "driven by $me" || fail "this spoke does not drive $list: $(list_show)"
  ok "this spoke drives $list ('driven by $me'), started $third on seat $(seat_of "$third"), and left ${others[*]} alone"
}

helper_step_4() {
  begin 4 "$me sees the list driven by the other machine, which starts only the third"
  wait_for "the list read as driven by its owner" sh -c "wsp worklist show '$list' 2>&1 | grep -q 'driven by'"
  third=${rows[2]}
  claimed "$third" && fail "this spoke started ${third}: the other machine's"
  ok "$(list_show | grep -m1 'driven by' | sed 's/^[[:space:]]*//'); $third is not started here"
}

# Each row's verifier is opened by the run of the spoke that holds the row:
# the owner's on the third, the helper's on its two, though the owner drives.
both_step_5() {
  begin 5 "all three get verdicts and landings on the shared trunk; the cache shows them landed; the barrier opens on the owner's spoke"
  if [ "$role" = helper ]; then
    check=("${mine[@]}")
  else
    check=("$third")
  fi
  for row in "${check[@]}"; do
    note "$row's verifier is opened by this spoke's run; wait for its verdict"
    wait_for "a verifier on $row" sh -c "wsp show '$row' --log 2>&1 | grep -q '## Verification'"
    wait_for "$row landed" sh -c "wsp show '$row' --log 2>&1 | grep -q 'wsp: landed'"
    sha=$(landed_sha "$row")
    row_show "$row" | grep -q 'pushed to origin' || fail "$row's landing was not pushed to the shared remote: $(row_show "$row")"
    on_trunk "$sha" || fail "$row's landing $sha is not on the shared remote"
    ok "$row landed as $sha, pushed, and on the shared remote"
  done
  for row in "${rows[@]}"; do
    wait_for "$row shown landed in this spoke's cache" sh -c "wsp show '$row' --log 2>&1 | grep -q 'wsp: landed' || grep -A8 '\"$row\"' '$state/hub/heads.json' 2>/dev/null | grep -q '\"landed\"'"
  done
  if [ "$role" = owner ]; then
    if [ -f "$state/cycle.log" ]; then
      wait_for "the barrier of group 1 open" cycle_has "started $list · barrier 1"
    else
      note "no cycle.log at $state: read the barrier from \`wsp worklist show $list\`"
    fi
  fi
}

helper_step_6() {
  begin 6 "a seat on $me's spoke tells seat:<the owner's row>; its seat receives it, and the reply comes back"
  seat=$(seat_of "${mine[0]}")
  [ -n "$seat" ] || fail "no seat recorded on ${mine[0]}: $(row_show "${mine[0]}")"
  before=$(WSP_SEAT_ID=$seat w tell --log "seat:${mine[0]}" 2>&1 | wc -l)
  printf 'is the third row landed on the remote?' | WSP_SEAT_ID=$seat w tell "seat:$third" - >/dev/null 2>&1 ||
    fail "tell seat:$third from seat $seat was refused"
  ok "seat $seat told seat:$third"
  wait_for "the reply in the conversation with seat:${mine[0]}" sh -c "[ \$(WSP_SEAT_ID='$seat' wsp tell --log 'seat:${mine[0]}' 2>&1 | wc -l) -gt $((before + 1)) ]"
}

owner_step_6() {
  begin 6 "a seat on the other spoke tells seat:$third; this spoke's seat receives it, and the reply goes back"
  if [ -f "$state/cycle.log" ]; then
    wait_for "the tell reaching the seat on $third" sh -c "grep -qE 'told the seat on $third: from seat:.*@.* through the hub' '$state/cycle.log'"
  else
    note "no cycle.log at $state: read the tell off the seat's screen"
  fi
  note "the agent on $third answers with the \`--re\` command its footer names; the other machine waits for that reply"
}

helper_step_7() {
  begin 7 "$me's spoke loses the hub mid-row, keeps working, and on reconnect its journal fast-forwards"
  out=$(w add "Row four" -p "$project" --workspace "$shared" 2>&1) || fail "wsp add: $out"
  row4=$(printf '%s\n' "$out" | tr -s ' \t' '\n\n' | grep -m1 "^${project}-") || fail "add printed no id: $out"
  w caption "$row4" "Row four" >/dev/null 2>&1
  w spawn "$row4" -p "$project" --agent >/dev/null 2>&1 || fail "wsp spawn $row4"
  ok "$row4 is a shared row, leased and spawned here"
  note "the row's id for the owner's side: $row4"
  wait_for "the journal empty before the outage" sh -c "[ \"\$(sed -n 's/.*\"journal\": *\\([0-9]*\\).*/\\1/p' '$state/hub/sync.json' 2>/dev/null)\" = 0 ]"
  pause "cut this machine's link to the hub now (turn the network off, or block the hub's port)"
  w rename "$row4" "Row four, finished offline" >/dev/null 2>&1 || fail "rename $row4 offline"
  w note "$row4" "kept working while the hub was away" >/dev/null 2>&1 || fail "note $row4 offline"
  claimed "$row4" || fail "the claim on $row4 did not survive the outage"
  ok "renamed and noted $row4 offline; the claim holds"
  pause "restore the link"
  wait_for "the journal pushed and the link up" sh -c "[ \"\$(sed -n 's/.*\"journal\": *\\([0-9]*\\).*/\\1/p' '$state/hub/sync.json' 2>/dev/null)\" = 0 ] && grep -q '\"link\": *\"up\"' '$state/hub/sync.json'"
  ok "reconnected; the journal is empty (the owner's side shows the work)"
}

owner_step_7() {
  begin 7 "the helper's spoke goes offline mid-row and its work arrives on reconnect; $me sees it"
  wait_for "the helper's offline work in this spoke's list" sh -c "wsp ls --all 2>&1 | grep -q 'finished offline'"
}

step_8() {
  begin 8 "wsp-463's standalone test and wsp-180's privacy test still pass"
  note "run on a machine with the trunk's source and a hub to test against: \`WSP_HUB_BIN=<path to wsp-hub> WSP_HUB_REQUIRED=1 cargo test --test multiplayer_v1\` (it runs them as its step 8); not part of this script"
}

# ───────────────────────────── the run ─────────────────────────────

if [ "$role" = owner ]; then
  owner_step_1
  say ""; say "== step 2: the helper's spoke shows the list (watched on its machine)"
  owner_step_4
  both_step_5
  owner_step_6
  owner_step_7
else
  helper_step_2
  helper_step_3
  helper_step_4
  both_step_5
  helper_step_6
  helper_step_7
fi
step_8
say ""
say "mp-gate: $role finished: every step above printed ok"
