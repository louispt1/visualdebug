#!/bin/bash
# Runs the spike on all three Node fixtures and prints which original lines were captured.
F=$(cd "$(dirname "$0")/../../fixtures" && pwd)
one() { # dir focus cmd...
  d=$1; focus=$2; shift 2
  cd "$F/$d" && "$F/../spikes/node/run.sh" "$focus" true -- "$@" >/dev/null 2>&1; rc=$?
  lines=$(cat .visualdebug/spike/trace-*.jsonl 2>/dev/null | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const ev=s.split("\n").filter(Boolean).map(l=>JSON.parse(l));console.log(ev.length+" events; "+ev.map(e=>e.event[0]+e.line).join(" "))})')
  echo "== $d (exit $rc): $lines"
  grep -v "scriptParsed\|paused:" .visualdebug/spike/probe-errors.log 2>/dev/null | head -3
}
one node-plain '{"src/pricing.js":[[7,16]]}' node src/main.js
one node-jest '{"src/pricing.js":[[7,16]]}' node node_modules/.bin/jest --runInBand
one node-vitest-ts '{"src/pricing.ts":[[18,26]]}' node node_modules/.bin/vitest run --pool=forks --maxWorkers=1
