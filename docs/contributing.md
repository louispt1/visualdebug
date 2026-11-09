# Contributing

Start with [architecture.md](architecture.md); agent sessions should also read [`CLAUDE.md`](../CLAUDE.md).

## Setup

```sh
cargo build
(cd fixtures/rails-app && bundle install)
(cd fixtures/ruby-plain && bundle install)
python3 -m venv .venv && .venv/bin/pip install pytest django
(cd fixtures/node-jest && npm ci) && (cd fixtures/node-vitest-ts && npm ci)
```

## Tests

| What | Command |
|---|---|
| Rust unit, CLI and golden fixture tests | `cargo test` (fixtures whose runtime is missing are skipped) |
| Python golden tests with a specific interpreter | `VISUALDEBUG_TEST_PYTHON=.venv/bin/python cargo test` |
| Ruby probe | `ruby probes/ruby/test/probe_test.rb` |
| Python probe | `PYTHON=.venv/bin/python python3 -m unittest discover -s probes/python/tests` |
| Node probe | `node --test probes/node/test/probe.test.cjs` |
| Skill end to end (costs a Claude Code run) | `scripts/e2e-skill.sh` |

Golden snapshots live in `crates/visualdebug-cli/tests/snapshots`. After an intended change, run the
tests, review the `*.snap.new` files and rename them to `.snap` (or `cargo insta review`). Python on
3.11 (settrace) has its own `_settrace` snapshots; Node snapshots are per major version (other
versions check structure only; set `VISUALDEBUG_NEW_NODE_SNAPSHOTS=1` to create one).

## Rules

- A probe must never change the traced program's behaviour. Every hook body is wrapped so probe
  errors are logged to `probe-errors.log` and swallowed; serialization never calls code that could do
  I/O (no `Relation#inspect`, no third-party `__repr__`, previews only in Node).
- No network access and no LLM calls from the tool or the viewer.
- Probes stay dependency-free and are written in their own language.
- Probe changes come with a test in that language's own framework.
- Design decisions are recorded in `docs/architecture.md` with the reason.

## Releasing

Releases are built by [cargo-dist](https://opensource.axo.dev/cargo-dist/) (`dist-workspace.toml`,
`.github/workflows/release.yml`): push a `v*` tag. The Homebrew job needs a `HOMEBREW_TAP_TOKEN`
secret with push access to `louispt1/homebrew-tap`. Publish crates in order:
`visualdebug-core`, `visualdebug-viewer`, `visualdebug`.
