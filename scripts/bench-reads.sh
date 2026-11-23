#!/bin/sh
# wsp-741: the list reads must answer in under a second at idle.
# Usage: scripts/bench-reads.sh [path/to/wsp]   (default: wsp on PATH)
# Read-only on the live store. Wall time is meaningless on a loaded machine, so
# the CPU time is printed too; the budget is judged on wall time at idle.
W=${1:-wsp}
BUDGET_MS=${BUDGET_MS:-1000}
fail=0
for verb in "worklist ls --json" "wip --json"; do
  best=""
  for i in 1 2 3; do
    t0=$(python3 -c 'import time;print(int(time.time()*1000))')
    # shellcheck disable=SC2086
    cpu=$( { /usr/bin/time -p "$W" $verb >/dev/null; } 2>&1 | awk '/^user|^sys/{s+=$2} END{printf "%.2f", s}')
    t1=$(python3 -c 'import time;print(int(time.time()*1000))')
    ms=$((t1 - t0))
    [ -z "$best" ] || [ "$ms" -lt "$best" ] && best=$ms
    last_cpu=$cpu
  done
  echo "wsp $verb: best of 3 ${best} ms wall, ${last_cpu} s cpu (budget ${BUDGET_MS} ms)"
  [ "$best" -le "$BUDGET_MS" ] || fail=1
done
exit $fail
