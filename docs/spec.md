# Original build spec (lightly condensed from the project owner's brief, 2026-11-05)

> Deviations agreed after Phase 0 are recorded in `docs/architecture.md`. Where they conflict, architecture.md wins.

# Build `visualdebug`: execution traces you can scrub through, annotated by AI, for PR review

> Working name is `visualdebug`. Before publishing, check the name is free on crates.io, Homebrew, PyPI, RubyGems and npm, and rename everywhere if needed.

## What we're building

`visualdebug` is an open source CLI that records how code actually executes when you run a concrete example, then turns that recording into a self-contained HTML viewer. In the viewer you can scrub forward and backward through execution line by line. Each step shows the current line, local variables (with what changed highlighted), the call stack, and side effects such as SQL queries.

It ships with a Claude Code skill. The skill lets an agent:

1. Read a diff (PR, staged/unstaged changes, commit(s)).
2. Pick or write examples that exercise the changed code.
3. Trace those examples.
4. Annotate the key steps with explanations, as a guided tour.
5. Open the viewer for the human reviewer.

The goal is understanding, not automated debugging. The human does the stepping; the AI picks good examples and narrates. (Inspiration: Geoffrey Litt, "Understanding is the new bottleneck", and his AI-built Prolog debugger.)

Target ecosystems, in priority order:
1. **Ruby / Rails**: RSpec, Minitest, `rails runner`, plain Ruby.
2. **Python**: pytest and plain scripts.
3. **JavaScript / TypeScript on Node**: plain Node, Jest, Vitest, tsx. Browser tracing is out of scope for v1.

## Core architecture (decided; push back before deviating)

**The tool itself is deterministic and never calls an LLM.** All AI behavior lives in the skill. This keeps the tool model-agnostic, testable and useful without AI.

- **Rust (single static binary):** the CLI, language detection, injecting probes into the target process, ingesting and normalizing traces, diff-based focus filtering, computing variable diffs between steps, syntax highlighting, summarizing traces for LLM consumption, merging annotations, bundling the HTML viewer, installing the skill.
- **Thin in-process probes, one per language, written in that language.** Zero third-party dependencies. Embedded in the Rust binary (`include_str!`), written at runtime to `~/.cache/visualdebug/probes/<version>/` (via `directories`), injected through environment variables, so users never modify their Gemfile, requirements or package.json.

| Language | Injection | Recording mechanism |
|---|---|---|
| Ruby | `RUBYOPT="-r<cache>/visualdebug_probe.rb"` | `TracePoint` on `:call, :return, :b_call, :b_return, :line, :raise` |
| Python | Prepend probe dir to `PYTHONPATH` with a `sitecustomize.py` that chains to any pre-existing `sitecustomize` | `sys.monitoring` on 3.12+, falling back to `sys.settrace` on 3.9–3.11 |
| Node | `NODE_OPTIONS="--require <cache>/visualdebug_probe.cjs"` (append to any existing value) | A worker thread uses `inspector.Session.connectToMainThread()` to set breakpoints on focus files and capture scopes on each pause. The main thread blocks with `Atomics.wait` until breakpoints are set. |

- **Probe ↔ Rust contract.** Probes append JSON Lines to `$OUT_DIR/trace-<pid>-<tid>.jsonl`. Configuration (focus files and lines, start trigger, limits, redaction patterns) comes in as a JSON file whose path is in an env var. No socket or IPC in v1.

Recommended crates: `clap` (derive), `serde`/`serde_json`, `anyhow`/`thiserror`, `syntect`, `sourcemap`, `directories`, `opener`, `tempfile`, `which`, `insta`, `assert_cmd`. Shell out to `git` rather than `git2`. Avoid async/tokio.

## Trace format

Versioned schema (`schema/trace.schema.json`, `"version": 1`). Each event has: `seq` (global order), `pid`, `tid`; `event`: `call | line | return | raise | block_call | block_return | side_effect`; `file` (project-relative) and `line`; `function` (qualified, e.g. `Pricer#call`, `pricing.apply_discount`, `applyDiscount`); `frame_id` and `depth`; `locals` (name → serialized value); `return_value` (on return), `exception` (on raise); `side_effect` payload: `{ kind: "sql", sql, binds?, duration_ms, name }`, or `{ kind: "log", ... }`.

Locals capture is snapshot-based in the probes. Rust computes per-step diffs (added, changed or removed) during ingestion. Each serialized value looks like `{ type, repr, truncated: bool, id? }`; `id` is object identity, used to show "same object, mutated".

### Value serialization rules (critical: tracing must not change program behavior)
- Re-entrancy guard while the probe serializes.
- Limits: repr length 200, container elements 10, nesting depth 3. Mark cut-offs `truncated`.
- Never trigger I/O or lazy loading. Rails: `ActiveRecord::Relation#inspect` loads records — render unloaded relations as `#<Relation User where(...) unloaded>` (optionally `to_sql`) only when `loaded?` is false. Association proxies: `unloaded association` unless loaded. AR models: class, `id`, `new_record?`, `changed` attribute names, truncated attributes hash; not default inspect.
- Wrap every repr in rescue/try; fall back to `#<ClassName (repr failed)>`.
- Python: `reprlib` with limits. Skip `__repr__` on objects whose class is defined outside the project unless builtin. Treat Django QuerySets like AR relations.
- Redaction of variable names / hash keys matching `password|passwd|secret|token|api_key|apikey|authorization|cookie|session|private_key|credential` → `[REDACTED]`.

## Focus and scoping

- Default include: project source only. Exclude gems / `vendor/bundle`, `site-packages` / virtualenvs, `node_modules`, the standard library, test framework internals.
- Library calls are recorded as a single collapsed `call` event with no inner steps (`→ ActiveRecord::Persistence#save (library)`).
- `--focus-diff <base>`: changed files and line ranges from `git diff <base>...HEAD`. Full line-level detail inside changed files, call/return-only in other project files. Viewer marks changed lines in the gutter.
- `--start-at <file:line | Class#method | module.func>`: don't record until execution first enters the target; stop when that frame returns. Without it, record from the start of the test body where detectable, otherwise from program start.
- `--max-events` (default 20k): on hitting it, stop, mark trace `truncated`, warn suggesting `--start-at` or narrower focus.
- Loop compression in Rust at ingestion/summarize time: consecutive iterations of the same line range are grouped.

## CLI surface

```
visualdebug doctor
visualdebug run [--focus-diff BASE] [--start-at T] [--name N] [--max-events N] -- <command...>
visualdebug summarize <trace> [--format text|json] [--max-lines N]
visualdebug annotate <trace> <annotations.json>
visualdebug view [<trace>...] [--out FILE] [--no-open]
visualdebug compare <base-ref> -- <command...>       # phase 7
visualdebug install-skill [--user | --project]
visualdebug clean
```

Docs must show working:
```
visualdebug run --focus-diff main --start-at "Pricer#call" -- bundle exec rspec spec/services/pricer_spec.rb:42
visualdebug run --focus-diff main -- pytest tests/test_pricing.py::test_bulk_discount
visualdebug run --focus-diff main -- npx vitest run src/pricing.test.ts
```

Requirements: everything in `.visualdebug/` in the project root (add to `.gitignore` if present and say so); `DISABLE_SPRING=1`; detect parallel runners (pytest-xdist, Jest workers, Vitest pools, Rails parallel tests) and force single-process or print the flag; pass through exit status but still write the trace.

### `summarize` output
Compact, references step numbers annotations can target:
```
trace: pricer_spec:42  (1,284 events, 212 shown, focus: app/services/pricer.rb [changed L18-40])
#14  pricer.rb:18  Pricer#call            order=#<Order id=7 total=120.0>
#15  pricer.rb:19                          discount: (new) nil
#16  pricer.rb:22  → Customer#tier (library-ish, project file, call-only)  returned :gold
#17  pricer.rb:23                          discount: nil → 0.1
#18-#61 pricer.rb:30-32  loop ×15 (line_items.each)  subtotal: 0 → 108.0
#62  pricer.rb:35  SQL  UPDATE "orders" SET "total" = ...  (0.4ms)
#63  pricer.rb:40  ← Pricer#call returned 108.0
```
Changed lines marked. Stable step ids survive loop compression.

### Annotations format (`annotations.json`)
```json
{
  "version": 1,
  "title": "How the bulk discount change works",
  "overview": "markdown",
  "tour": [ { "step": 17, "title": "...", "body": "markdown", "highlight_vars": ["discount"] } ],
  "notes": [ { "step": 62, "body": "markdown" } ],
  "quiz": [ { "question": "...", "options": ["...", "..."], "answer": 1, "explanation": "...", "step": 17 } ]
}
```
`annotate` rejects unknown step ids and warns on tours > ~15 stops.

## The viewer

Single self-contained HTML file, no network, works from `file://`. Plain HTML/CSS/JS, no build step; assets in `crates/visualdebug-viewer/assets/` embedded with `include_str!`. Rust injects trace data + pre-highlighted source.

- Layout: code pane (current line highlighted, changed-line gutter marks, never-executed lines faded); variables pane (changes highlighted, before → after); call stack; SQL/side-effects panel; annotation panel; multiple traces as tabs.
- Navigation: ←/→ or j/k step; Shift+←/→ step over; u step out; Home/End; timeline scrubber with depth sparkline and markers for annotations and SQL; click a line → next/previous time it executed; click a variable → value history.
- Tour mode (default when annotations exist), Next/Prev between stops, drop into free scrubbing and return.
- Quiz at end of tour, answers reveal explanation and a "show me" link.
- Deep links `#step-123`.
- Light/dark via `prefers-color-scheme`, keyboard accessible, calm and dense.
- Smooth with 20k events: virtualize lists, precompute diffs in Rust.

## The skill (`skill/visualdebug/SKILL.md`, installed by `install-skill`)

YAML frontmatter with `name` and a deliberately "pushy" `description`; body < ~300 lines; per-language details in `skill/visualdebug/references/{ruby,python,node}.md`. Trigger whenever the user wants to understand, review or walk through a PR, diff, branch or unfamiliar code path.

Workflow: (1) get the change (`git diff <base>...HEAD` or `gh pr diff <n>`), read surrounding code; (2) `visualdebug doctor`; (3) choose 1–3 examples (main path + one edge case the change handles), prefer existing tests, else temporary example under `.visualdebug/examples/`, never modify application code, side-effect-safe; (4) `visualdebug run --focus-diff <base> --start-at <entry> -- <cmd>`, narrow and re-run if truncated/noisy; (5) `visualdebug summarize`; (6) write `annotations.json` following Litt's explainer principles (background first, intuition before details, *why* at each stop, 5–12 stops, 3–5 question quiz on real understanding); (7) `annotate` then `view`; (8) hand off with a 2–3 sentence orientation and keyboard shortcuts, then stop; offer more traces/questions. Also guidance on failing traces, huge Rails boots (always `--start-at`), unsupported languages.

## Repository layout

```
Cargo.toml (workspace; MIT OR Apache-2.0)
crates/visualdebug-cli, crates/visualdebug-core, crates/visualdebug-viewer
probes/ruby/visualdebug_probe.rb; probes/python/sitecustomize.py, _visualdebug_probe.py; probes/node/visualdebug_probe.cjs, visualdebug_worker.cjs
schema/trace.schema.json, schema/annotations.schema.json
skill/visualdebug/SKILL.md, skill/visualdebug/references/*.md
fixtures/ ruby-plain, rails-app, python-plain, python-pytest, node-plain, node-jest, node-vitest-ts
docs/ README, architecture.md, trace-format.md, contributing.md
CLAUDE.md
```

## Phases (stop for review at the end of each phase)

0. Spikes (Ruby/Rails, Python monitoring+settrace, Node worker inspector incl. Jest/Vitest/TS source maps), measure overhead.
1. Core + Ruby: workspace, schema types, `run`, ingestion with diffs, focus filtering, `--start-at` for Ruby, `summarize`, first viewer (step, scrub, variables, stack), snapshot tests. Deliverable: trace a request spec in the Rails fixture and scrub it in the browser.
2. Viewer depth: step over/out, line and variable history, SQL panel, loop compression, changed-line gutter, multiple traces, 20k-event perf check.
3. Python parity incl. QuerySet-safe serialization and xdist handling.
4. Node/TypeScript parity incl. source maps and runner detection.
5. Annotations and skill: `annotate`, tour mode and quiz, the skill and references, `install-skill`; end-to-end test on a realistic small PR branch in the Rails fixture.
6. Distribution and polish: cargo-dist (macOS arm64/x64, Linux x64/arm64, shell installer, Homebrew tap; Windows best effort), `cargo install visualdebug`, polished `doctor`, README with GIF, CI matrix.
7. Stretch: `compare` via temporary `git worktree`, aligned traces, side-by-side view with first divergence.

## Engineering standards

- Correctness over cleverness: probe hook bodies swallow exceptions and log to `.visualdebug/probe-errors.log`.
- Tests: golden `insta` snapshots of normalized, summarized traces per fixture (nondeterminism scrubbed); probe tests in each language's own framework; CLI tests with `assert_cmd`.
- Overhead reported by `doctor --bench`.
- No telemetry, no network access from tool or viewer.
- Keep `docs/architecture.md` and `CLAUDE.md` current; record every deviation from this spec with the reason.
