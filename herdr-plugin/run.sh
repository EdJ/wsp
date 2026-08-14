#!/bin/sh
# Locate the wsp binary without relying on herdr's inherited PATH, then run it.
# `pane-*` subcommands wrap a read-only view so a popup pane stays open until
# the user dismisses it.
set -eu

for candidate in \
  "${WSP_BIN:-}" \
  "$HOME/.local/bin/wsp" \
  "$HOME/claude/wsp/target/release/wsp" \
  "$(command -v wsp 2>/dev/null || true)"
do
  if [ -n "$candidate" ] && [ -x "$candidate" ]; then
    WSP="$candidate"
    break
  fi
done

if [ -z "${WSP:-}" ]; then
  echo "wsp: binary not found (looked in ~/.local/bin, the cargo target dir and PATH)" >&2
  exit 127
fi

cmd="${1:-wip}"
shift 2>/dev/null || true

case "$cmd" in
  pane-*)
    view="${cmd#pane-}"
    [ "$view" = "where" ] && view="where"
    "$WSP" "$view" "$@" || true
    printf '\n\033[2m— any key to close —\033[0m'
    # Raw single-key read; falls back to line-read where stty is unavailable.
    if stty -f /dev/tty raw -echo 2>/dev/null; then
      dd if=/dev/tty bs=1 count=1 >/dev/null 2>&1 || true
      stty -f /dev/tty sane 2>/dev/null || true
    else
      read -r _ || true
    fi
    ;;
  *)
    exec "$WSP" "$cmd" "$@"
    ;;
esac
