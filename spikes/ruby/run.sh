#!/bin/sh
# usage: run.sh <start_at|""> <focus-json|null> -- cmd...
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
OUT="$PWD/.visualdebug/spike"; rm -rf "$OUT"; mkdir -p "$OUT"
START="$1"; FOCUS="$2"; shift 3
[ -n "$START" ] && START_JSON="\"$START\"" || START_JSON=null
printf '{"root":"%s","out_dir":"%s","start_at":%s,"focus":%s}' "$PWD" "$OUT" "$START_JSON" "$FOCUS" > "$OUT/config.json"
VISUALDEBUG_CONFIG="$OUT/config.json" RUBYOPT="-r$HERE/probe.rb ${RUBYOPT:-}" DISABLE_SPRING=1 "$@"
