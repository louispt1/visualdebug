// visualdebug Node probe (inspector worker). Connected to the main thread's inspector:
// breakpoints open a recording window, then Debugger.stepInto drives it with node internals and
// node_modules blackboxed. Every pause is answered (resume or step) even when recording fails.
"use strict";
const { workerData, parentPort } = require("node:worker_threads");
const inspector = require("node:inspector");
const fs = require("node:fs");
const path = require("node:path");
const { SourceMap } = require("node:module");
const { fileURLToPath } = require("node:url");

const cfg = JSON.parse(fs.readFileSync(workerData.configPath, "utf8"));
const ROOT = (() => {
  try {
    return fs.realpathSync(cfg.root).replace(/\/$/, "") + "/"; // Node reports realpaths (e.g. /private/var on macOS)
  } catch {
    return cfg.root.replace(/\/$/, "") + "/";
  }
})();
const OUT = cfg.out_dir;
const LINE_FILES = cfg.line_files ? new Set(cfg.line_files) : null;
const MAX_EVENTS = cfg.max_events || 20000;
const LIM = cfg.limits || {};
const MAX_REPR = LIM.repr || 2000;
const REDACT = new RegExp(cfg.redact || "password|secret|private_key|credential", "i");
const START = parseStart(cfg.start_at);
const flag = new Int32Array(workerData.flag);
const outFile = path.join(OUT, `trace-${process.pid}.jsonl`);
const keepAlive = setInterval(() => {}, 1 << 30); // a pending inspector call does not keep a worker alive

function logErr(where, e) {
  try {
    fs.appendFileSync(path.join(OUT, "probe-errors.log"), `${process.pid} node-worker ${where}: ${(e && e.stack) || e}\n`);
  } catch {}
}

function parseStart(s) {
  if (!s || s === "auto:test") return null;
  const m = /^(.+):(\d+)$/.exec(s);
  return m ? { file: m[1], line: +m[2] } : null; // names are resolved to file:line by the CLI
}

const session = new inspector.Session();
session.connectToMainThread();
const post = (method, params = {}) => new Promise((res, rej) => session.post(method, params, (e, r) => (e ? rej(e) : res(r))));

// ------------------------------------------------------------------ output
let seq = 0, count = 0, buf = [], truncated = false, closed = false;
function stamp(ev) {
  ev.seq = ++seq;
  ev.t = Number(process.hrtime.bigint());
  ev.pid = process.pid;
  ev.tid = 0;
  return ev;
}
function emit(ev) {
  if (closed) return;
  if (count >= MAX_EVENTS) {
    if (!truncated) {
      truncated = true;
      closed = true;
      buf.push(JSON.stringify(stamp({ event: "meta", kind: "truncated" })));
      flush();
    }
    return;
  }
  count++;
  buf.push(JSON.stringify(stamp(ev)));
  if (buf.length >= 200) flush();
}
function meta(kind, extra = {}) {
  buf.push(JSON.stringify(stamp({ event: "meta", kind, ...extra })));
}
function flush() {
  if (!buf.length) return;
  try {
    fs.appendFileSync(outFile, buf.join("\n") + "\n");
  } catch (e) {
    logErr("flush", e);
  }
  buf = [];
}

// ------------------------------------------------------------------ scripts & source maps
const scripts = new Map(); // scriptId -> { url, abs, rel, map, setup }
const toAbs = (url) => (url.startsWith("file://") ? fileURLToPath(url) : url);
const isProject = (abs) =>
  abs.startsWith(ROOT) && !abs.includes("/node_modules/") && (!abs.includes("/.visualdebug/") || abs.includes("/.visualdebug/examples/"));
const relOf = (abs) => (abs.startsWith(ROOT) ? abs.slice(ROOT.length) : abs);
const lineLevel = (rel) => LINE_FILES === null || LINE_FILES.has(rel);
const resolveSource = (s, src) => toAbs(new URL(src, s.url.startsWith("file://") ? s.url : "file://" + s.url).href);
const mappedBps = new Set(); // breakpoint ids we set by scriptId on transformed scripts

function original(loc) {
  const s = scripts.get(loc.scriptId);
  if (!s) return null;
  if (!s.map) return { abs: s.abs, file: s.rel, line: loc.lineNumber + 1 };
  const e = s.map.findEntry(loc.lineNumber, loc.columnNumber || 0);
  if (!e || e.originalLine === undefined || !e.originalSource) return null;
  const abs = resolveSource(s, e.originalSource);
  return { abs, file: relOf(abs), line: e.originalLine + 1 };
}

// Lines to break on: with a start target, only that line (stepping supplies the detail, and
// breakpoints elsewhere would pause outside the window); otherwise every line of the focus files.
function wantedLines(rel, n) {
  if (START) return START.file === rel ? [START.line] : [];
  const out = [];
  if (LINE_FILES && LINE_FILES.has(rel)) for (let l = 1; l <= n; l++) out.push(l);
  return out;
}

function setupScript(scriptId) {
  const s = scripts.get(scriptId);
  if (!s || !s.map) return Promise.resolve();
  return (s.setup ||= setupMapped(scriptId, s).catch((e) => logErr("setupMapped", e)));
}

// Transformed script: every breakable generated location, mapped back; first one per wanted line.
async function setupMapped(scriptId, s) {
  const { scriptSource } = await post("Debugger.getScriptSource", { scriptId });
  const lines = scriptSource.split("\n");
  const { locations } = await post("Debugger.getPossibleBreakpoints", {
    start: { scriptId, lineNumber: 0, columnNumber: 0 },
    end: { scriptId, lineNumber: lines.length - 1, columnNumber: lines[lines.length - 1].length },
  });
  const firstFor = new Map(); // "rel:line" -> location
  for (const loc of locations) {
    const e = s.map.findEntry(loc.lineNumber, loc.columnNumber || 0);
    if (!e || e.originalSource === undefined) continue;
    const rel = relOf(resolveSource(s, e.originalSource));
    const key = `${rel}:${e.originalLine + 1}`;
    if (!firstFor.has(key)) firstFor.set(key, loc);
  }
  const want = [];
  for (const rel of new Set([...firstFor.keys()].map((k) => k.slice(0, k.lastIndexOf(":"))))) {
    const maxLine = Math.max(...[...firstFor.keys()].filter((k) => k.startsWith(rel + ":")).map((k) => +k.slice(rel.length + 1)));
    for (const l of wantedLines(rel, maxLine)) {
      // a start line may not be breakable itself: take the next mapped line
      for (let x = l; x <= maxLine; x++) {
        const loc = firstFor.get(`${rel}:${x}`);
        if (loc) { want.push(loc); break; }
      }
    }
  }
  await Promise.all(
    want.map((loc) =>
      post("Debugger.setBreakpoint", { location: { scriptId, lineNumber: loc.lineNumber, columnNumber: loc.columnNumber } })
        .then((r) => mappedBps.add(r.breakpointId))
        .catch(() => {}),
    ),
  );
}

session.on("Debugger.scriptParsed", ({ params }) => {
  try {
    if (!params.url) return;
    const abs = toAbs(params.url);
    if (!params.sourceMapURL && !isProject(abs)) return;
    let map = null;
    if (params.sourceMapURL && params.sourceMapURL.startsWith("data:")) {
      try {
        const body = params.sourceMapURL.slice(params.sourceMapURL.indexOf(",") + 1);
        const json = /;base64,/.test(params.sourceMapURL.slice(0, 80)) ? Buffer.from(body, "base64").toString() : decodeURIComponent(body);
        map = new SourceMap(JSON.parse(json));
      } catch (e) {
        logErr("sourcemap", e);
      }
    }
    if (!map && !isProject(abs)) return;
    scripts.set(params.scriptId, { url: params.url, abs, rel: relOf(abs), map });
    // compileFunction'd scripts (CJS under Jest) get no instrumentation pause: set mapped breakpoints
    // now. This races only module top-level code; function bodies run later.
    if (map) setupScript(params.scriptId);
  } catch (e) {
    logErr("scriptParsed", e);
  }
});

// ------------------------------------------------------------------ values
const redactedStrings = new Set();
const REDACTED = { type: "redacted", repr: "[REDACTED]", truncated: false };
function cut(s) {
  return s.length > MAX_REPR ? s.slice(0, MAX_REPR) + "…" : s;
}
function previewRepr(p, className) {
  if (p.subtype === "array") return `[${p.properties.map((x) => propValue(x)).join(", ")}${p.overflow ? ", …" : ""}]`;
  if (p.subtype === "map" && p.entries) return `Map(${p.entries.map((e) => `${e.key ? e.key.description : "?"} => ${e.value.description}`).join(", ")}${p.overflow ? ", …" : ""})`;
  if (p.subtype === "set" && p.entries) return `Set(${p.entries.map((e) => e.value.description).join(", ")}${p.overflow ? ", …" : ""})`;
  const body = p.properties.map((x) => `${x.name}: ${REDACT.test(x.name) ? "[REDACTED]" : propValue(x)}`).join(", ");
  return `${className && className !== "Object" ? className + " " : ""}{${body}${p.overflow ? ", …" : ""}}`;
}
function propValue(x) {
  if (x.type === "string") return redactedStrings.has(x.value) ? "[REDACTED]" : JSON.stringify(x.value);
  if (x.type === "object" && x.subtype === "null") return "null";
  if (x.valuePreview) return previewRepr(x.valuePreview, x.valuePreview.description);
  return x.value !== undefined ? String(x.value) : x.type === "function" ? "ƒ" : x.type;
}
function ser(ro) {
  if (!ro) return { type: "undefined", repr: "undefined", truncated: false };
  let repr, truncated = false;
  if (ro.type === "string") {
    if (redactedStrings.has(ro.value)) return REDACTED;
    truncated = ro.value.length > MAX_REPR;
    repr = JSON.stringify(truncated ? ro.value.slice(0, MAX_REPR) : ro.value);
  } else if (ro.unserializableValue) repr = ro.unserializableValue;
  else if (ro.type === "undefined") repr = "undefined";
  else if (ro.subtype === "null") repr = "null";
  else if ("value" in ro && ro.type !== "object") repr = String(ro.value);
  else if (ro.type === "function") repr = `ƒ ${(ro.description || "").split("(")[0].replace(/^(async\s+)?function\s*/, "").trim() || "anonymous"}`;
  else if (ro.preview) {
    repr = previewRepr(ro.preview, ro.className);
    truncated = !!ro.preview.overflow;
  } else repr = ro.description || ro.className || ro.type;
  return { type: ro.subtype && ro.subtype !== "null" ? ro.subtype : ro.className || ro.type, repr: cut(repr), truncated: truncated || repr.length > MAX_REPR };
}

// Scopes from innermost up to the function's own scope; closures/module/global are context, not locals.
async function locals(frame) {
  const out = {};
  for (const sc of frame.scopeChain) {
    if (sc.type !== "local" && sc.type !== "block" && sc.type !== "catch") break;
    const { result } = await post("Runtime.getProperties", { objectId: sc.object.objectId, ownProperties: true, generatePreview: true });
    for (const p of result) {
      if (p.name in out || !p.value) continue;
      if (REDACT.test(p.name)) {
        if (p.value.type === "string") redactedStrings.add(p.value.value);
        out[p.name] = REDACTED;
      } else out[p.name] = ser(p.value);
    }
    if (sc.type === "local") break;
  }
  const t = frame.this;
  // class instances only: plain objects here are usually module.exports or a namespace
  if (t && t.type === "object" && t.subtype !== "null" && !["Object", "global", "Window", "process", "Module"].includes(t.className)) out["this"] = ser(t);
  return out;
}

// ------------------------------------------------------------------ the window
// Our frame stack (bottom first): { fid, key, name, lineLevel, line, seen }. Aligned against the project
// frames of each pause (V8 call frame ids are not stable across pauses).
let win = null;
let fidSeq = 0;
const posKey = (f) => `${f.location.lineNumber}:${f.location.columnNumber}`;
const frameKey = (f) => `${f.functionLocation ? f.functionLocation.scriptId + ":" + f.functionLocation.lineNumber + ":" + f.functionLocation.columnNumber : f.location.scriptId}|${f.functionName}`;

function projectFrames(callFrames) {
  const out = [];
  for (const f of callFrames) {
    const o = original(f.location);
    if (o && isProject(o.abs)) out.push({ f, o });
  }
  return out.reverse(); // bottom first
}

async function onPaused(params) {
  let next = "resume";
  try {
    if (params.reason === "instrumentation") {
      await setupScript(params.data && params.data.scriptId);
      return;
    }
    if (closed) return;
    const P = projectFrames(params.callFrames);
    if (!P.length) {
      next = win ? "stepInto" : "resume";
      return;
    }
    const top = P[P.length - 1];
    // Node's own JS (node:internal/*) can't be blackboxed: its scripts have no context id, so V8
    // never applies patterns, ranges or skipLists to them. Step back out instead of through.
    if (win && top.f !== params.callFrames[0] && params.reason !== "exception" && params.reason !== "promiseRejection") {
      next = "stepOut";
      return;
    }
    if (!win) {
      if (!opensWindow(params, top)) return;
      win = { stack: [] };
      meta("window_start");
      await post("Debugger.setPauseOnExceptions", { state: "all" }).catch(() => {});
    }
    // the window's root frame, found again on every pause (callers vanish after an await)
    const base = win.stack.length ? P.findIndex((x) => frameKey(x.f) === win.stack[0].key) : P.length - 1;
    if (base < 0) {
      endWindow();
      return;
    }
    const rel = P.slice(base);
    // align
    let k = 0;
    while (k < win.stack.length && k < rel.length && win.stack[k].key === frameKey(rel[k].f)) k++;
    win.stack.length = k;
    let enteredTop = false;
    for (let i = k; i < rel.length; i++) {
      const { f, o } = rel[i];
      const fr = { fid: ++fidSeq, key: frameKey(f), name: f.functionName || "(anonymous)", lineLevel: lineLevel(o.file) || win.stack.length === 0, line: o.line, seen: new Set([posKey(f)]) };
      win.stack.push(fr);
      const isTop = i === rel.length - 1;
      const ev = { event: "call", file: o.file, line: o.line, function: fr.name, frame_id: fr.fid, depth: win.stack.length };
      if (isTop) ev.locals = await locals(f);
      emit(ev);
      if (isTop) enteredTop = true;
    }
    const fr = win.stack[win.stack.length - 1];
    const { f, o } = top;
    if (params.reason === "exception" || params.reason === "promiseRejection") {
      const d = params.data || {};
      emit({ event: "raise", file: o.file, line: o.line, function: fr.name, frame_id: fr.fid, depth: win.stack.length,
             exception: { type: d.className || d.type || "Error", message: cut(String(d.description || "").split("\n")[0]) } });
    } else if (f.returnValue) {
      emit({ event: "return", file: o.file, line: o.line, function: fr.name, frame_id: fr.fid, depth: win.stack.length, return_value: ser(f.returnValue) });
      win.stack.pop();
      if (!win.stack.length) {
        endWindow();
        return;
      }
    } else if (!enteredTop) {
      // V8 pauses at every call and statement, and again after each call returns: record a line
      // once per visit, like Ruby/Python. A position seen twice on the same line is a loop going round.
      const sameVisit = o.line === fr.line && !fr.seen.has(posKey(f));
      if (!sameVisit) fr.seen = new Set();
      fr.line = o.line;
      fr.seen.add(posKey(f));
      if (fr.lineLevel && !sameVisit) emit({ event: "line", file: o.file, line: o.line, function: fr.name, frame_id: fr.fid, depth: win.stack.length, locals: await locals(f) });
    }
    next = "stepInto";
  } catch (e) {
    logErr("paused", e);
    next = "resume";
  } finally {
    post("Debugger." + next).catch(() => post("Debugger.resume").catch(() => {}));
  }
}

// Module top-level code (CommonJS wrapper, ESM module, Vitest's module function) runs at import.
function isModuleTopLevel(f) {
  const fl = f.functionLocation;
  return (f.scopeChain[0] && f.scopeChain[0].type === "module") || (!f.functionName && (!fl || (fl.lineNumber === 0 && fl.columnNumber === 0)));
}

function opensWindow(params, top) {
  if (isModuleTopLevel(top.f)) return false;
  const hits = params.hitBreakpoints || [];
  const s = scripts.get(top.f.location.scriptId);
  // A by-URL breakpoint resolved inside a transformed copy of the file hits at the wrong place.
  if (s && s.map && hits.length && !hits.some((h) => mappedBps.has(h))) return false;
  if (START) return top.o.file === START.file && top.o.line >= START.line;
  return LINE_FILES === null || LINE_FILES.has(top.o.file);
}

function endWindow() {
  win = null;
  meta("window_end");
  flush();
  post("Debugger.setPauseOnExceptions", { state: "none" }).catch(() => {});
}

session.on("Debugger.paused", ({ params }) => {
  onPaused(params);
});

parentPort.on("message", (m) => {
  if (m !== "exit") return;
  try {
    flush();
    session.disconnect();
  } catch (e) {
    logErr("exit", e);
  }
  clearInterval(keepAlive);
  Atomics.store(flag, 2, 1);
  Atomics.notify(flag, 2);
});

(async () => {
  try {
    await post("Debugger.enable");
    await post("Debugger.setBlackboxPatterns", { patterns: ["/node_modules/", "^node:", "^internal/", "/\\.visualdebug/(?!examples/)"] });
    // Untransformed files: by-URL breakpoints resolve exactly at compile time.
    const files = new Set([...(LINE_FILES || []), ...(START ? [START.file] : [])]);
    const calls = [];
    for (const rel of files) {
      let n = 0;
      try {
        n = fs.readFileSync(path.join(ROOT, rel), "utf8").split("\n").length;
      } catch {
        continue;
      }
      const re = path.join(ROOT, rel).replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + "$";
      for (const l of wantedLines(rel, n)) calls.push(post("Debugger.setBreakpointByUrl", { urlRegex: re, lineNumber: l - 1 }).catch(() => {}));
    }
    await Promise.all(calls);
    // Transformed scripts run via vm.Script / ESM (Vitest, tsx): pause before they run to place mapped breakpoints.
    await post("Debugger.setInstrumentationBreakpoint", { instrumentation: "beforeScriptWithSourceMapExecution" }).catch(() => {});
    meta("start", { language: "node", runtime: process.version, probe_version: 1 });
  } catch (e) {
    logErr("setup", e);
  }
  Atomics.store(flag, 0, 1);
  Atomics.notify(flag, 0);
})();
