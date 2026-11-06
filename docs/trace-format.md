# Trace format (version 1)

Two layers, both JSON. Machine-readable schemas: [`schema/trace.schema.json`](../schema/trace.schema.json)
and [`schema/annotations.schema.json`](../schema/annotations.schema.json).

## 1. Probe events (`.visualdebug/runs/<name>/trace-<pid>.jsonl`)

One JSON object per line, written by the in-process probes:

```json
{"event":"line","file":"app/services/pricer.rb","line":19,"function":"Pricer#call","frame_id":3,"depth":1,
 "locals":{"discount":{"type":"NilClass","repr":"nil","truncated":false}},"seq":42,"t":627582973881000,"pid":51234,"tid":7}
```

| field | |
|---|---|
| `event` | `call`, `line`, `return`, `raise`, `block_call`, `block_return`, `side_effect`, or `meta` |
| `seq`, `t`, `pid`, `tid` | per-process order, monotonic clock (ns), process, thread |
| `file`, `line` | project-relative path; for TypeScript the original `.ts` line |
| `function` | qualified: `Pricer#call`, `Pricer.build`, `pricing.Pricer.total`, `priceOrder` |
| `frame_id`, `depth` | per-process frame id; depth counts project frames inside the window |
| `library` | `true` on a `call`/`return` into non-project code made directly by project code |
| `locals` | name → value, a snapshot **before** the line runs (Ruby adds the receiver's `@ivars`) |
| `return_value` | on `return` |
| `exception` | `{type, message}` on `raise` |
| `side_effect` | `{kind:"sql", sql, name, binds, duration_ms}` |
| `kind` (meta) | `start` (language, runtime), `window_start`, `window_end`, `truncated` |

A value is `{type, repr, truncated, id?}`: `repr` is at most ~2000 characters (`--max-repr`), containers
show 50 items and 3 levels, `id` is object identity (Ruby, Python), and redacted values are
`{"type":"redacted","repr":"[REDACTED]"}`.

## 2. Normalized trace (`.visualdebug/traces/<name>.json`)

Produced by `visualdebug run` from all of a run's JSONL files:

- events from all processes merged by `t`; library `call`/`return` pairs become one step with
  `library: true` and `return_value`;
- `frame` is a trace-wide frame index; library calls and side effects carry their caller's frame and
  file/line;
- each step with `locals` gets a `diff` against the previous snapshot of the same frame:
  `added`, `changed` (name → previous repr), `removed`, `mutated` (changed but same object id);
- `id` (1-based) is stable — summaries and annotations refer to it;
- `focus.files` maps changed files to changed line ranges; `annotations` is filled by `annotate`.

## 3. Summaries (`visualdebug summarize`)

Not a storage format, but a contract with the skill: every row starts with `#id` or `#a-#b`, `*`
marks changed lines, `│` rows show the source about to run and `⟶ var: before → after`, `→`/`←` are
calls/returns, `(library)` marks collapsed library calls, `SQL` rows are queries, `!! raise` is an
exception, and `loop ×N` rows compress repeated iterations (step range preserved).
`--format json` emits the same rows as objects.
