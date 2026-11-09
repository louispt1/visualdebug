# Architecture

`visualdebug` = a deterministic Rust CLI + one thin in-process probe per language + a static HTML
viewer + a Claude Code skill. The tool never calls an LLM. The original brief is `docs/spec.md`;
this file records how it was built and **every deviation from the spec, with the reason**.

## Data flow

```
visualdebug run -- <cmd>
  ├─ writes .visualdebug/runs/<name>/config.json          (focus, start_at, limits, redaction)
  ├─ writes probes to <cache>/probes/<version>-<hash>/    (embedded with include_str!)
  ├─ spawns <cmd> with RUBYOPT / PYTHONPATH / NODE_OPTIONS / VISUALDEBUG_CONFIG
  │     each probe appends JSONL to .visualdebug/runs/<name>/trace-<pid>.jsonl
  └─ ingests -> .visualdebug/traces/<name>.json            (normalized; schema/trace.schema.json)
visualdebug summarize <name>   -> step-numbered text/JSON (loop-compressed)
visualdebug annotate <name> annotations.json -> validated, merged into the trace
visualdebug view <name>...     -> .visualdebug/view-<name>.html (one self-contained file)
```

Crates: `visualdebug-core` (types, ingestion, focus, summarize, annotations), `visualdebug-viewer`
(highlighting + bundling; assets are plain HTML/CSS/JS), `visualdebug-cli` (package name
`visualdebug` so `cargo install visualdebug` works; `crates/visualdebug-cli/{probes,skill}` are
symlinks so the packaged crate contains them).

## Probe contract

- One env var: `VISUALDEBUG_CONFIG` = path to the run's `config.json`
  (`root`, `out_dir`, `line_files` (null = every project file), `start_at`, `max_events`, `redact`, `limits`).
- Probes write `{event, seq, t, pid, tid, ...}` lines; `t` is the monotonic clock in ns, so files from
  several processes merge by time. Library calls are `call`/`return` with `library: true`; Rust merges
  each pair into one step.
- Locals are snapshots taken *before* the line runs. Rust computes per-frame diffs (added/changed/removed,
  and `mutated` when the object id is unchanged).
- Depth counts project frames only.
- Every probe hook body swallows its own exceptions and logs to `<out_dir>/probe-errors.log`.

## Recording windows

| `start_at` | Meaning |
|---|---|
| `null` | whole program (Ruby: plus a synthetic `<main>` frame for the entry script) |
| `auto:test` | default for test runners: one window per test body, setup excluded |
| `Class#method`, `Class.method`, `module.func` | first entry into that target, until it returns (re-arms for later calls) |
| `file:line` | Ruby/Python: the method containing that line; Node: that line |

## Deviations from the spec (agreed after Phase 0 unless noted)

1. **Overhead target.** "Under 10× inside the focus window" is not achievable for line-level tracing with
   locals snapshots in any runtime. Targets: ≈0 outside the window; per step ≲15µs (Ruby/Python),
   ≲0.5ms (Node).
2. **Runtime floors** (EOL as of Nov 2026): Ruby 3.2+ (CI 3.3/3.4/4.0), Python 3.11+ (settrace only on
   3.11), Node 22/24 (20 best effort).
3. **One env var** (`VISUALDEBUG_CONFIG`); the output dir lives in the config.
4. **Node source maps are resolved in the probe** (breakpoints must be placed before code runs), with
   Node's built-in `module.SourceMap` + `Debugger.getPossibleBreakpoints`. No Rust `sourcemap` crate.
5. **Syntax highlighting** uses syntect with `two-face` syntaxes (syntect's defaults lack TypeScript/TSX).
6. **Event order** = per-process `seq` + shared monotonic `t`; no cross-process counter.
7. **Node has no object identity** in v1 (debugger handles change between pauses).
8. **Python library calls have no return value** (sys.monitoring `CALL` has none; the effect shows in the
   next step's variables).
9. **Ruby stack = project frames only.** Ruby re-fires `:call` on `retry` without a `:return`, so a stack
   counting library frames drifts. Library calls classify themselves with `caller_locations`; a re-fired
   `retry` is told apart from recursion by its caller.
10. **`--focus-diff BASE` diffs `merge-base(BASE, HEAD)` against the working tree** (plus untracked
    files), not `BASE...HEAD`: reviewing uncommitted changes is a core use case and this is a superset.
11. **Changed files are recorded line by line in full**; the changed ranges drive gutter marks and `*` in
    summaries. The window's root frame (e.g. the test body) is always line-level, as context.
12. **`file:line` start targets** resolve to the enclosing method (Ruby: by walking the compiled ISeq
    tree, which runs nothing and activates no gems).
13. **Extra flags:** `run --focus FILE` (repeatable) for focus without git.
14. **Noise filtering (Ruby):** Rails `SCHEMA` queries, load/definition hooks (`require`, `inherited`,
    `const_added`, ...), library calls made by class bodies during autoload, and test-framework ivars are
    not recorded. Objects with the default `#inspect` and Structs are rendered by the probe itself (so
    redaction applies and addresses are hidden); values redacted by name stay redacted by identity.
15. **Summaries include the source text** of each line row (trimmed) — cheap context for an LLM.
16. **Value limits are 2000 characters and 50 items** (spec: 200 and 10), overridable with
    `--max-repr`. At 200 a single model instance was already cut off, and the probe keeps nothing
    past the limit, so the viewer had nothing to expand. The viewer clamps long values to 3 lines instead.
17. **Viewer interactions:** clicking a variable's *name* opens its value history; clicking a clamped value
    expands it (spec: clicking the variable opens history). The timeline colours each column by the
    function running (grey for library code and files outside the focus) and SQL marks grow with query
    time, on top of the spec's depth sparkline. Per-step timing and memory are deliberately not shown:
    tracing overhead dominates them, so they would point at the wrong hotspots.
18. **`.visualdebug/` ignores itself** with a `.gitignore` of `*` inside it (spec: add it to the project's
    `.gitignore`). Editing the project's `.gitignore` left a change to commit in every repo traced.
19. **Redaction pattern is `password|secret|private_key|credential`** (spec also lists `passwd`, `token`,
    `api_key`, `apikey`, `authorization`, `cookie`, `session`). Traces run in test/dev, where those values
    are rarely useful to an attacker, and the broad pattern hid ordinary values like `session_id`.

## Ruby probe notes

- Arming without per-call cost: a `:end` TracePoint (one event per class body) waits for the target
  method, then a TracePoint *targeted* at that method opens the window. Targeted TracePoints also
  listen to `:line`, because a TracePoint enabled inside a `:call` hook misses that method's first line.
- `auto:test`: RSpec gets an `around(:example)` hook (installed when `Runner#run_specs` is called) that
  targets the example block; Minitest/Rails tests arm the test method inside `Minitest::Test#run`.
- Serialization never calls `Relation#inspect`, association readers, or library-defined `#inspect`.

## Python probe notes

- `sitecustomize.py` (probe dir prepended to `PYTHONPATH`) installs the probe, then re-imports
  `sitecustomize` with its own dir removed from `sys.path`, so a project's own sitecustomize still runs.
- `sys.monitoring` (3.12+): before a window only `PY_START` is on, and every non-trigger code location
  returns `DISABLE`, so code outside the window costs nothing. Opening a window calls
  `restart_events()`, which also re-arms other tools' disabled locations (coverage re-disables its own).
- `sys.settrace` (3.11): the global tracer still runs once per call before the window (~6x on tight
  loops). It *does* see library return values; `sys.monitoring` does not (deviation 8).
- Generators and coroutines keep one frame id across yield/resume (`yielded: true` on the suspension).
- Recorded frames: functions, plus the main script's top level. Class bodies and imported modules' top
  level are skipped (same as Ruby).
- `__repr__` is called only when the class that *defines* it is builtin, stdlib or project code. So a
  Django `Model` (whose `__repr__` calls a possibly lazy-loading `__str__`) is rendered from `__dict__`,
  minus `_`-prefixed library internals. QuerySets show `loaded`/`unloaded` from `_result_cache`.
- Django SQL is captured with a `connection.execute_wrappers` entry, added to existing connections when
  a window opens and to new ones via `connection_created`.
- Library calls skip builtins (`len`, `dict.get`, ...) and generated code (`<string>`, e.g. dataclass
  `__init__`), and pytest's `@py_assert*` temporaries are hidden.

## Node probe notes

- `--require visualdebug_probe.cjs` starts an inspector worker (`connectToMainThread`) and blocks in
  `Atomics.wait` until it is ready (~25ms). The worker keeps itself alive with a timer: a pending
  inspector call does not, and without it the worker exits mid-handshake (the Phase 0 "deadlock").
- Recording: breakpoints only *open* a window; `Debugger.stepInto` drives it, with node internals and
  `node_modules` blackboxed. Every pause is answered (step or resume) even if recording fails.
- Frame identity: V8 call frame ids change between pauses, so the project-frame stack is aligned
  between pauses by function location + name; a `returnValue` pause pops the frame.
- Breakpoint placement: untransformed files get `setBreakpointByUrl` (exact, resolved at compile time).
  Transformed files (Vitest, tsx, Jest/babel) get breakpoints by scriptId at the first breakable
  generated location of each wanted original line (`Debugger.getPossibleBreakpoints` + Node's
  `module.SourceMap`), from a `beforeScriptWithSourceMapExecution` pause, or straight from
  `scriptParsed` for `compileFunction` scripts (Jest), which races only module top-level code.
  By-URL hits inside a transformed copy are ignored.
- Module top-level code (CJS wrapper, ESM, Vitest's module function) never opens a window.
- Locals: scopes up to the function's own `local` scope (closures, module and global are context);
  `this` only for class instances. Redaction is by name, plus by string value for redacted strings
  (no object identity in Node).
- Exceptions pause only while a window is open (`setPauseOnExceptions: all`, reset after).
- On exit the main thread asks the worker to flush and disconnect, so Node doesn't print
  "Waiting for the debugger to disconnect...".
- No library-call events: blackboxed code is invisible to stepping.
- `--start-at` names (`priceOrder`, `Cart.total`) are resolved to `file:line` by the CLI by scanning
  JS/TS sources; the Node probe itself only understands `file:line`.
- Runners: Jest gets `--runInBand`; Vitest gets `--pool=forks --no-file-parallelism` (the probe skips
  worker threads). Golden tests keep exact snapshots per Node major version and check structure on others.

## compare (Phase 7)

`visualdebug compare <base-ref> -- <cmd>` adds a temporary `git worktree` of the whole repo at the
base ref under `.visualdebug/worktrees/` and removes it afterwards (`--keep-worktree` to inspect).
Untracked things the command needs are shared or created: `node_modules`, `.venv`, `venv`,
`vendor/bundle` and `.bundle` are symlinked, `storage`/`tmp`/`log` are created empty, and
`.visualdebug/examples` is copied. The same command runs there (`<name>@base`) and in the working tree
(`<name>`), each focused on the same changed files, with line ranges for its own version
(`git diff -R` for the base).

Alignment works on step *signatures* (kind + function + the source text of the line, so line shifts
don't matter). A greedy walk resyncs within a 400-step lookahead, which keeps 20k-step traces cheap.
The first divergence is either
**control** (a step only one side has) or **data** (same step, a variable both sides have with a
different value; timestamps and memory addresses are scrubbed first). The comparison is stored on the
HEAD trace. `view` brings in the base trace (its sources come from `git show <base>:<path>`) and shows
both panes side by side, stepping through the alignment and starting just before the divergence.
