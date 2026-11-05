# Phase 0 findings (2026-11-05)

Spike code is throwaway; reproduce with `spikes/<lang>/run.sh` (Node: `spikes/node/all.sh`).
Machine: macOS arm64, Ruby 3.1–4.0.2, Python 3.11.11 / 3.13.2, Node 20.20.2 / 25.1.0, Rails 8.1.4,
Jest 30, Vitest 4.1.12, tsx 4.

## (a) Ruby: works

- `RUBYOPT=-r<probe>` under `bundle exec rspec` on the Rails fixture works on 3.1–4.0.
- Request spec traced from `OrdersController#price`: params, find (SQL), autoload, Pricer, loop, `update!`
  (SAVEPOINT/UPDATE/RELEASE), render. SQL lands in the correct frame via `sql.active_record`,
  subscribed lazily when the window opens.
- AR-safe serialization works: unloaded `CollectionProxy` shows as unloaded, and models show id/attrs.
  Objects whose `inspect` is library Ruby code are rendered from ivars by our own serializer instead.
- `--start-at Class#method` costs ~0 before the window: a `:end` TracePoint, fired once per class body,
  arms a TracePoint targeted at that one method. This works with Zeitwerk and doesn't depend on
  `script_compiled`, which bootsnap would bypass.
- VM quirks found and handled:
  1. `retry` re-fires `:call` with no `:return` (Arel `Visitor#visit`). A stack built by counting
     library push/pop drifts, so the stack holds **project frames only**, and each library call/return
     classifies itself by `caller_locations(…, 1)`.
  2. A TracePoint enabled inside a `:call` hook misses the first `:line` (same instruction), so the
     targeted TracePoint also listens to `:line` and emits just that one.
  3. Class bodies run during autoload report lines in the *caller's* frame, so a line must match the
     top frame's file.
  4. A relative `tp.path` for `ruby script.rb` needs `expand_path`.
- Cost per event: bare TracePoint ~0.1µs, call/return with return value ~4µs, line with locals
  snapshot ~11µs (serialization and JSON dominate). Rails request spec end to end: 0.77s → 0.82s with
  start-at, → 1.39s tracing all project code from boot.

## (b) Python: works

- sitecustomize injection via `PYTHONPATH` works under pytest and chains to a pre-existing
  `sitecustomize`.
- `sys.monitoring` (3.13) and `sys.settrace` (3.11) produce the same trace. Covered: raise inside a
  handled try/except, redaction of `api_token` locals/attrs, and dataclass reprs.
- pytest-xdist works (`-n 2`): one trace file per worker pid.
- Overhead outside the window: **zero** on 3.12+ (`DISABLE` per code location), about 6× on 3.11 (the
  global trace function still runs per call). Inside the window ~10µs/event, same as Ruby.
- Gaps:
  - `sys.monitoring` CALL events have no return value, so library calls show no return value.
  - settrace can't see C calls (`dict.get`, `round`).
  - Module and class bodies leak globals as "locals": skip `<module>`/class code.
  - `restart_events()` at window start also re-arms *other* tools' DISABLEd events (e.g. coverage).

## (c) Node: works, with one gotcha in the handshake

- **The spec's design works**: a worker with `connectToMainThread()`, main thread blocked in
  `Atomics.wait`. Handshake ~25ms per process.
  - My first attempts deadlocked. The cause wasn't the blocking: a pending inspector call doesn't keep
    the worker's event loop alive, so the worker exited mid-`Debugger.enable`. Fix: a keepalive timer
    in the worker. Inspector messages are serviced fine while the main thread is in `Atomics.wait`.
- Recording mechanism: breakpoints only open the window. From there `Debugger.stepInto` drives, with
  `node_modules` and node internals blackboxed. This yields real call/line/return events with
  `returnValue`, which per-line breakpoints can't give. Locals come from `local`+`block` scopes via
  `Runtime.getProperties`, which never invokes getters.
- Placing breakpoints, per runner:

  | Runner | How the probe sees the file | Breakpoints |
  |---|---|---|
  | plain Node CJS/ESM | untransformed | `setBreakpointByUrl` (exact, resolved at compile, no race) |
  | Vitest 4 (forks), tsx | vm.Script/ESM with inline map | `beforeScriptWithSourceMapExecution` pause, then mapped bps by scriptId (no race) |
  | Jest 30 `--runInBand` | `compileFunction`: **no** instrumentation pause | mapped bps from `scriptParsed`. Races only module top-level code; function bodies run later |

  Mapping: Node's built-in `module.SourceMap`. For original→generated, take
  `Debugger.getPossibleBreakpoints` and map each location (tsx/esbuild emits the whole module on one
  line, so line scanning fails). **No Rust `sourcemap` crate needed.**
- All of plain/Jest/Vitest/tsx produce the expected trace on Node 20 and 25.
- Cost: ~0 outside the window. **~0.35ms per step** inside it (3–4 cross-thread round trips per
  pause), about 35× Ruby/Python. 20k events ≈ 7s.
- Gaps for Phase 4:
  - async functions: the window closes at the first `await`, which returns a Promise; track it
    across resumption.
  - by-URL bps fire on transformed copies (stray events): ignore them on mapped scripts.
  - Vitest's module wrapper leaks module bindings into locals.
  - `process.exit()` prints "Waiting for the debugger to disconnect...": disconnect in an exit hook.
  - objectIds aren't stable across pauses, so there's no `id` (object identity) in v1.
  - Vitest `threads` pool runs tests in worker threads, which the probe skips: force `--pool=forks`.
