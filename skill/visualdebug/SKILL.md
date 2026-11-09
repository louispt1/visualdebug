---
name: visualdebug
description: Turn a code change into a guided, scrubbable execution trace a human can step through in the browser. Use this whenever the user wants to understand, review, walk through, or get oriented in a PR, diff, branch, commit, staged/unstaged changes, or an unfamiliar code path ("review this PR", "help me understand this change", "what does this branch do", "walk me through how X works", "how does this request get handled"), even if they never mention tracing, debugging or visualdebug. Works for Ruby/Rails, Python and Node/TypeScript projects. Prefer it over only reading the diff aloud whenever the code can actually be run.
---

# visualdebug: guided execution tours for code review

`visualdebug` records how code actually executes for a concrete example, then bundles the recording
into a self-contained HTML viewer the human scrubs through line by line (current line, variables with
changes highlighted, call stack, SQL). **You** choose good examples and narrate; **the human** steps.
The tool is deterministic and never calls an LLM; all judgement is yours.

The goal is the reviewer's *understanding*, not automated debugging. Optimize for "after 10 minutes
in the viewer, they could explain the change to a colleague and spot what might be wrong with it".

## Workflow

### 1. Get the change and understand its purpose
- Base branch: `git diff <base>...HEAD` (usually `main`; check `git branch -a` / the PR base).
  A PR number or URL: `gh pr diff <n>` and `gh pr view <n>` (title, description, linked issues).
  Uncommitted work: `git diff HEAD` plus untracked files.
- Read the diff, then read enough surrounding code to know **what existed before and why it
  changed**: callers of changed functions, the tests touching them, related models/config.
- Write down (for yourself) the 1–3 behaviours the change is about, including the edge case it
  specifically handles. These drive everything else.

### 2. Check the environment
Run `visualdebug doctor`. Fix what you safely can (e.g. a missing `bundle install` in a fixture is the
user's call, so ask). If the language isn't supported, see "Unsupported languages" below.

### 3. Choose 1–3 examples
- Pick examples that make the change's behaviour **visible**: the main path, plus one edge case the
  change specifically handles (the bug it fixes, the new branch it adds). Avoid redundant examples.
- **Prefer existing tests** that exercise the changed lines: `git grep` for the changed function names
  in `spec/`, `test/`, `tests/`, `*.test.ts`.
- Otherwise write a **temporary** example under `.visualdebug/examples/` (a spec, a pytest test, a
  small script). It must be side-effect-safe: test environment, transactional fixtures, no external
  network calls, no writes outside the test database or a temp dir.
- **Never modify application code.** Don't commit examples.
- Language specifics (commands, entry points, runner quirks): read the matching reference
  (`references/ruby.md`, `references/python.md`, `references/node.md`) before your first run.

### 4. Trace each example
```
visualdebug run --focus-diff <base> --start-at <entry> --name <short-name> -- <command>
```
- `--focus-diff <base>` records changed files line by line and other project files call/return only,
  and marks changed lines (`*` in summaries, gutter in the viewer). It includes uncommitted changes.
- `--start-at` is the function where the interesting behaviour begins (`Pricer#call`,
  `pricing.apply_discount`, `priceOrder`, or `file:line`). Without it, test runners record each test
  body; plain scripts record the whole program.
- The command's exit status passes through. A failing test still produces a trace; keep it if the
  failure is the point (see "Failing traces").
- **If the run warns TRUNCATED or the summary is mostly noise, narrow and re-run** (a later
  `--start-at`, a single test via `file:line` / `::test_name`) rather than annotating a bad trace.
- If nothing was recorded, the hint printed tells you why (start target never reached, wrong focus).

### 5. Read the summary
```
visualdebug summarize <name>            # add --max-lines 600 for long traces
```
Each row starts with a step id (`#17`) or a range (`#18-#61` for a compressed loop). Rows show the
source line about to run and how variables changed since the previous step in that frame
(`discount: nil → 0.1` means the *previous* line set it). `→`/`←` are calls/returns, `(library)`
marks one collapsed call into a gem/package, `SQL` rows are queries, `!! raise` is an exception.
Read it alongside the diff until you can tell the story of the change in execution order.

### 6. Write `annotations.json`
Save it next to the trace (e.g. `.visualdebug/annotations-<name>.json`):
```json
{
  "version": 1,
  "title": "How guest checkouts get the bulk discount",
  "overview": "markdown: background first: what existed before, why it changed, the one-sentence intuition",
  "tour": [
    { "step": 17, "title": "Tier lookup sets the discount", "body": "markdown: why this matters", "highlight_vars": ["discount"] }
  ],
  "notes": [ { "step": 62, "body": "markdown aside for a step that isn't a stop" } ],
  "quiz": [
    { "question": "...", "options": ["...", "..."], "answer": 1, "explanation": "...", "step": 17 }
  ]
}
```
Explainer principles (they matter more than coverage):
- **Background first.** The overview says what existed before and why it changed, then gives the
  intuition in a sentence or two. No step details yet.
- **Intuition before details.** Early stops show the shape of the flow; details come later.
- **Explain *why* at every stop**, not what the line literally does; the reader can see the line.
  Good: "`discount` stays `nil` here because guest checkouts have no tier, and this is the case the PR
  fixes." Bad: "This line sets discount."
- **5–12 stops in narrative order** (usually execution order). Point at the step where the effect is
  *visible* (variables show state before the line runs, so a change appears at the next step).
  Use `highlight_vars` for the variables the stop is about. Link other steps in markdown as `#42`.
- Put stops on changed lines (`*`) and on the consequences of the change (a different branch, a
  different SQL query, a different return value), not on setup boilerplate.
- **Quiz: 3–5 questions that check real understanding of the change**: predict behaviour for a
  different input, identify which step decides the outcome, spot what would break if a line were
  removed. No trivia ("what is the variable called"). Each explanation should teach, and link a step.

### 7. Merge and open
```
visualdebug annotate <name> .visualdebug/annotations-<name>.json
visualdebug view <name> [<name2> ...]      # several traces become tabs
```
`annotate` rejects step ids that don't exist and warns about weak tours; fix and re-run.
`view` writes `.visualdebug/view-<name>.html` and opens it (pass `--no-open` in headless
environments and give the path instead).

### 8. Hand off, then stop
Give the human a 2–3 sentence orientation (what the change does, which example shows what, where the
interesting part is) and the controls:

> → / ← step · Shift+→ step over · u step out · n / p next/previous tour stop · click a line to jump
> to its next execution · click a variable for its history · `?` for all shortcuts

Then **stop and let them explore**. Offer to trace another example (a different input, the failing
case, the old behaviour) or answer questions about what they see. Don't pre-empt their exploration
with a long written review.

## Situations

**Failing traces.** A failing test's trace is valuable: annotate where behaviour diverges from what
the test expected (the step whose variable or branch is wrong), and say so in the overview.

**Before vs after.** When the question is "what does this change *change*" (a bug fix, a refactor
that should be behaviour-preserving, a performance tweak), run the same example on both versions:
`visualdebug compare <base> --start-at <entry> -- <cmd>`. It uses a temporary git worktree (your
working tree is untouched), prints the first divergence (control: different code ran; data: same
code, different value), and `visualdebug view <name>` shows both side by side starting there. The
command must exist and work on the base too (an example in `.visualdebug/examples/` is copied over).

**Huge Rails boots / big apps.** Always pass `--start-at` (the controller action or service entry
point); otherwise boot noise and setup dominate. Prefer a single example (`spec/x_spec.rb:42`).

**Noisy traces.** Narrow with a later `--start-at`, a single test, or a smaller focus; check
`--max-events` wasn't hit. Library internals are collapsed automatically; if a project helper
dominates, start inside the code that matters.

**Concurrency / multiple processes.** Each process is traced (pytest-xdist workers, forked
servers) and merged by time; prefer single-process runs for a readable story.

**Unsupported languages** (Go, Java, browser JS, ...): say so plainly, then fall back to a written
walkthrough of the diff in execution order with concrete example values, and offer to trace any
Ruby/Python/Node part of the change.

**Secrets.** Values whose names look secret (password, secret, private_key, credential) are redacted
before they're written. Still don't paste trace contents into external services.
