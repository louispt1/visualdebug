// Run: node --test probes/node/test/probe.test.cjs
"use strict";
const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const PROBE = path.join(__dirname, "..", "visualdebug_probe.cjs");

function trace(code, { lineFiles = ["app.js"], startAt = null, maxEvents = 1000 } = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "vd-node-")));
  const out = path.join(root, ".visualdebug", "run");
  fs.mkdirSync(out, { recursive: true });
  fs.writeFileSync(path.join(root, "app.js"), code);
  const cfgPath = path.join(out, "config.json");
  fs.writeFileSync(cfgPath, JSON.stringify({ root, out_dir: out, line_files: lineFiles, start_at: startAt, max_events: maxEvents }));
  const r = spawnSync(process.execPath, ["app.js"], {
    cwd: root,
    encoding: "utf8",
    env: { ...process.env, VISUALDEBUG_CONFIG: cfgPath, NODE_OPTIONS: `--require "${PROBE}"` },
  });
  const all = fs.readdirSync(out).filter((f) => f.startsWith("trace-")).flatMap((f) =>
    fs.readFileSync(path.join(out, f), "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l)));
  const errPath = path.join(out, "probe-errors.log");
  const errors = fs.existsSync(errPath) ? fs.readFileSync(errPath, "utf8") : "";
  fs.rmSync(root, { recursive: true, force: true });
  return { events: all.filter((e) => e.event !== "meta"), all, r, errors };
}

const PROGRAM = `
function add(a, b) {
  const sum = a + b;
  return sum;
}
function main() {
  let total = 0;
  for (let i = 0; i < 3; i++) total = add(total, i);
  const password = "hunter2";
  const copy = password;
  return total;
}
console.log(main());
`;

test("output, exit status and stderr are unchanged", () => {
  const { r, events, errors } = trace(PROGRAM + "process.exit(3);\n");
  assert.equal(r.stdout, "3\n");
  assert.equal(r.status, 3);
  assert.equal(r.stderr, "", "no 'Waiting for the debugger' or other noise");
  assert.equal(errors, "");
  assert.ok(events.some((e) => e.event === "call" && e.function === "main"));
});

test("call/line/return with locals and return values", () => {
  const { events } = trace(PROGRAM);
  const ret = events.filter((e) => e.event === "return" && e.function === "add");
  assert.deepEqual(ret.map((e) => e.return_value.repr), ["0", "1", "3"]);
  const line = events.find((e) => e.event === "line" && e.function === "main" && e.locals && "total" in e.locals);
  assert.ok(line);
  const main = events.find((e) => e.event === "call" && e.function === "main");
  assert.equal(main.depth, 1, "module top-level is not a window root");
});

test("redaction by name and by value", () => {
  const { events } = trace(PROGRAM);
  const dump = JSON.stringify(events);
  assert.ok(!dump.includes("hunter2"), "secret leaked");
  assert.ok(dump.includes("[REDACTED]"));
});

test("exceptions are recorded and still caught by the program", () => {
  const { events, r } = trace(`
function risky() {
  throw new TypeError("bad");
}
function main() {
  try {
    risky();
  } catch (e) {
    return "caught";
  }
}
console.log(main());
`);
  assert.equal(r.stdout, "caught\n");
  const raise = events.find((e) => e.event === "raise");
  assert.ok(raise, "raise event");
  assert.equal(raise.exception.type, "TypeError");
});

test("start_at file:line records only that function", () => {
  const { events } = trace(PROGRAM, { lineFiles: null, startAt: "app.js:3" });
  const calls = events.filter((e) => e.event === "call").map((e) => e.function);
  assert.ok(calls.length >= 1 && calls.every((f) => f === "add"), calls.join(","));
});

test("max_events truncates", () => {
  const { all, events } = trace(PROGRAM, { maxEvents: 5 });
  assert.equal(events.length, 5);
  assert.ok(all.some((e) => e.kind === "truncated"));
});
