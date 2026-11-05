#!/bin/sh
# usage: run.sh <focus-json> <source_maps:true|false> -- cmd...
HERE=$(cd "$(dirname "$0")" && pwd)
OUT="$PWD/.visualdebug/spike"; rm -rf "$OUT"; mkdir -p "$OUT"
FOCUS="$1"; SM="$2"; shift 3
printf '{"root":"%s","out_dir":"%s","focus":%s,"source_maps":%s}' "$PWD" "$OUT" "$FOCUS" "$SM" > "$OUT/config.json"
VISUALDEBUG_CONFIG="$OUT/config.json" NODE_OPTIONS="${NODE_OPTIONS:+$NODE_OPTIONS }--require $HERE/probe.cjs" "$@"
