#!/bin/sh
# run the wip_manager test binary N times at once, optionally with burners
bin=$(ls -t target/debug/deps/wip_manager-* | grep -v '\.d$' | head -1)
n=${1:-8}; burn=${2:-0}
pids=""
for i in $(seq $burn); do ( while :; do :; done ) & pids="$pids $!"; done
fail=0; run=""
for i in $(seq $n); do $bin wip_under >${TMPDIR:-/tmp}/wm.$i.out 2>&1 & run="$run $!"; done
for p in $run; do wait $p || fail=$((fail+1)); done
kill $pids 2>/dev/null
[ $fail -eq 0 ] || grep -h -A1 panicked ${TMPDIR:-/tmp}/wm.*.out
echo "failed $fail of $n"; exit $fail
