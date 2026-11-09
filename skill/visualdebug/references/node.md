# visualdebug: Node / TypeScript

Injected with `NODE_OPTIONS=--require`; no package.json changes. Node 22/24 (20 works). Uses the
built-in inspector from a worker thread, stepping through project code with `node_modules` skipped.
TypeScript (tsx, Vitest, ts-jest/babel) is mapped back to `.ts` lines via inline source maps.
Browser code is out of scope.

## Commands
Node needs something to focus on: `--focus-diff`, `--focus <file>`, or `--start-at`.
```
visualdebug run --focus-diff main -- npx vitest run src/pricing.test.ts
visualdebug run --focus-diff main --start-at priceOrder -- npx jest src/pricing.test.js
visualdebug run --focus-diff main -- npx tsx scripts/reprice.ts
visualdebug run --focus src/pricing.js -- node src/main.js
```
- Jest gets `--runInBand` and Vitest `--pool=forks --no-file-parallelism` automatically (tests in
  worker threads can't be traced). Through `npm test` that can't be done: call the runner directly.

## `--start-at` forms
- `priceOrder`, `Cart.total` / `Cart#total`: resolved to `file:line` by scanning the project's
  JS/TS sources (the command prints what it chose), or `src/pricing.ts:19` directly.
- Without it, a window opens whenever a focus file's code runs and lasts until that function returns.

## Reading Node traces
- No library-call rows: code in `node_modules` and Node internals is skipped entirely, so the trace
  jumps from the call site to the next project line. Callbacks into project code are recorded.
- Locals are the function's own scope (closures and module bindings are context); `this` is shown
  for class instances. Not-yet-initialized `let`/`const` may be missing or `undefined`.
- `async` functions return `Promise` at their return step; the resolved value is visible where the
  caller uses it.
- Exceptions are recorded as `!! raise` rows, including ones that are caught.
- No object identity in Node traces: "mutated" can't be detected, only changed reprs.

## Examples you write
`.visualdebug/examples/guest-discount.test.ts` run with the project's runner
(`npx vitest run .visualdebug/examples/guest-discount.test.ts`). The runner's `include` globs may
skip that directory. Then pass the file explicitly or use a `--config` that includes it, or write a
small `tsx` script instead. Mock network calls.

## Gotchas
- Per traced step Node costs ~0.3ms (each pause is an inspector round trip): keep windows small
  with `--start-at`.
- Each Node process pays ~25ms at startup for the inspector handshake.
