#!/bin/sh
# wsp-539: the daemon's attention tick and repair tick, per tick, from calls.jsonl.
#
# Usage: scripts/bench-ticks.sh [--since T] [--until T] [--last N] [FILE…]
#   T       an ISO time as calls.jsonl writes it (2026-11-25T21:40:00Z); compared
#           as text, so give it in UTC with the Z
#   --last  only the last N ticks that carry either pass (after --since/--until)
#   FILE    default $WSP_STATE/calls.1.jsonl and calls.jsonl (state dir
#           ~/.local/state/wsp), oldest first, so a window across a rotation reads
#
# Read-only. For each `"k":"tick"` line on which the attention pass was due or
# the repair pass ran, prints the attention phase and the repair pass (the sum
# of the phases `repair::tick` names, `repair:frozen` … `pickup`, in
# crates/engine/src/repair.rs) as wall ms (`phases`) and CPU ms
# (`phase_cpu_ms`), then min / median / max of each column. CPU is the figure to
# compare under load (docs/daemon-cost.md). The load printed is now, not the
# window's: note `uptime` while the window is being written.
#
# `phase_cpu_ms` is the process's CPU over the phase, other threads (mirror,
# fold) included, so a phase can read more CPU than wall; that is why the
# median of several ticks is the figure, not one tick. A daemon's first tick
# after a start or a reload is cold (repair ~850 ms CPU against ~150 steady, on
# 2026-11-25): leave it out of the comparison.
#
# "Due": a tick where the attention pass is not due still records the phase, at
# 0.0 ms; a due pass reads the whole store and is never under 0.1 ms. The
# repair pass is present only on the ticks it ran.
#
# The group 3/4/5 barriers of crate-split time a build by running it as the
# live daemon (a systemd drop-in `ExecStart=<binary> daemon` and `systemctl
# --user restart wsp-daemon`; launchd: the plist's program) for ~5 minutes each,
# before and after interleaved, and reading the window each wrote (wsp-539).
# Compare the steady ticks: leave out each window's first.
set -eu

since="" until="" last=0
while [ $# -gt 0 ]; do
  case $1 in
    --since) since=$2; shift 2 ;;
    --until) until=$2; shift 2 ;;
    --last) last=$2; shift 2 ;;
    -h|--help) sed -n "2,/^set -eu/p" "$0" | sed "$ d"; exit 0 ;;
    --) shift; break ;;
    -*) echo "bench-ticks: unknown flag $1" >&2; exit 2 ;;
    *) break ;;
  esac
done

if [ $# -eq 0 ]; then
  dir=${WSP_STATE:-$HOME/.local/state/wsp}
  set --
  [ -f "$dir/calls.1.jsonl" ] && set -- "$dir/calls.1.jsonl"
  set -- "$@" "$dir/calls.jsonl"
fi

command -v jq >/dev/null || { echo "bench-ticks: needs jq" >&2; exit 2; }

grep -h '"k":"tick"' "$@" | jq -rs --arg since "$since" --arg until "$until" --argjson last "$last" '
  # The phases of one repair pass, in the order repair::tick enters them.
  def repair_phases: [
    "repair:frozen", "repair:held", "repair:done-lists-held", "cycle",
    "repair:tells", "closing", "barrier", "ending", "ending:checks",
    "sweep:check-trees", "sweep:build-output", "sweep:auto-close",
    "sweep:scratch", "sweep:disk", "sweep:throttle", "sweep:cold",
    "ending:verification", "repair:gone-seat-claims", "stand-down",
    "repair:refusing", "seats", "pickup"
  ];
  def sum_of($m): [repair_phases[] as $p | ($m[$p] // 0)] | add;
  def r1: (. * 10 | round) / 10;
  def med: sort | if length == 0 then null
                  elif length % 2 == 1 then .[length / 2 | floor]
                  else (.[length / 2 - 1] + .[length / 2]) / 2 end;
  def stats($k): [.[] | .[$k] | select(. != null)] as $v
    | if ($v | length) == 0 then "-"
      else "\($v | min | r1)/\($v | med | r1)/\($v | max | r1)" end;
  def cell: if . == null then "-" else (r1 | tostring) end;

  [ .[]
    | select($since == "" or .t >= $since)
    | select($until == "" or .t <= $until)
    | (.phases // {}) as $w | (.phase_cpu_ms // {}) as $c
    | { t,
        tick_wall: .wall_ms,
        att_wall: (if ($w.attention // 0) > 0 then $w.attention else null end),
        att_cpu:  (if ($w.attention // 0) > 0 then ($c.attention // 0) else null end),
        rep_wall: (if $w["repair:frozen"] != null then sum_of($w) else null end),
        rep_cpu:  (if $w["repair:frozen"] != null then sum_of($c) else null end) }
    | select(.att_wall != null or .rep_wall != null) ]
  | (if $last > 0 then .[-$last:] else . end) as $ticks
  | ( ["time", "attention_wall_ms", "attention_cpu_ms", "repair_wall_ms", "repair_cpu_ms", "tick_wall_ms"] | @tsv ),
    ( $ticks[] | [.t, (.att_wall | cell), (.att_cpu | cell), (.rep_wall | cell), (.rep_cpu | cell), (.tick_wall | cell)] | @tsv ),
    ( ["min/med/max (\($ticks | length) ticks)",
       ($ticks | stats("att_wall")), ($ticks | stats("att_cpu")),
       ($ticks | stats("rep_wall")), ($ticks | stats("rep_cpu")),
       ($ticks | stats("tick_wall"))] | @tsv )
' | column -t -s "$(printf '\t')"

echo "load now: $(cut -d' ' -f1-3 /proc/loadavg 2>/dev/null || uptime | sed 's/.*load averages*: //')"
