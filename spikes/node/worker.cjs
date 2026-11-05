"use strict";
const { workerData } = require("node:worker_threads");
const inspector = require("node:inspector");
const fs = require("node:fs");
const path = require("node:path");
const { SourceMap } = require("node:module");
const { fileURLToPath } = require("node:url");

const cfg = JSON.parse(fs.readFileSync(workerData.configPath, "utf8"));
const ROOT = cfg.root.replace(/\/$/, "") + "/";
const OUT = cfg.out_dir;
const FOCUS = cfg.focus || {};
const MAX_EVENTS = cfg.max_events || 20000;
const REDACT = new RegExp(cfg.redact || "password|passwd|secret|token|api_key|apikey|authorization|cookie|session|private_key|credential", "i");
const flag = new Int32Array(workerData.sab);
const outFile = path.join(OUT, `trace-${process.pid}.jsonl`);
const errFile = path.join(OUT, "probe-errors.log");
const logErr = (where, e) => { try { fs.appendFileSync(errFile, `${process.pid} ${where}: ${e && e.stack || e}\n`); } catch {} };

const session = new inspector.Session();
session.connectToMainThread();
const keepAlive = setInterval(() => {}, 1 << 30); // a pending inspector call does not keep the worker alive
const post = (m, p = {}) => new Promise((res, rej) => session.post(m, p, (e, r) => (e ? rej(e) : res(r))));

const scripts = new Map(); // scriptId -> { abs, rel, map: SourceMap|null }
const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const toAbs = (url) => (url.startsWith("file://") ? fileURLToPath(url) : url);
const isProject = (abs) => abs.startsWith(ROOT) && !abs.includes("/node_modules/");

let seq = 0, count = 0, buf = [], win = null, frameSeq = 0;
const stats = { pauses: 0, pauseMs: 0 };
function emit(ev) {
  if (count >= MAX_EVENTS) return;
  count++;
  ev.seq = ++seq; ev.pid = process.pid; ev.tid = 0;
  buf.push(JSON.stringify(ev));
  if (buf.length >= 200) flush();
}
function flush() { if (buf.length) { fs.appendFileSync(outFile, buf.join("\n") + "\n"); buf = []; } }

session.on("Debugger.scriptParsed", ({ params }) => {
  try {
    if (!params.url) return;
    if (process.env.VD_DEBUG && params.url.includes("pricing")) logErr("scriptParsed", `${params.url} map=${(params.sourceMapURL||"").slice(0,40)} ctx=${params.executionContextId}`);
    const abs = toAbs(params.url);
    if (!isProject(abs) && !params.sourceMapURL) return;
    let map = null;
    if (params.sourceMapURL && params.sourceMapURL.startsWith("data:")) {
      const json = Buffer.from(params.sourceMapURL.split(",")[1], "base64").toString();
      map = new SourceMap(JSON.parse(json));
    }
    const rec = { abs, rel: abs.startsWith(ROOT) ? abs.slice(ROOT.length) : abs, map, url: params.url };
    scripts.set(params.scriptId, rec);
    // compileFunction'd scripts (CJS under Jest) never get an instrumentation pause: set mapped
    // breakpoints now. Races only against module top-level code; function bodies run later.
    if (map) setupScript(params.scriptId).catch((e) => logErr("setupScript", e));
  } catch (e) { logErr("scriptParsed", e); }
});

const resolveSource = (s, src) => toAbs(new URL(src, s.url.startsWith("file://") ? s.url : "file://" + s.url).href);

// Generated location -> original { file(rel), line(1-based) }
function original(loc) {
  const s = scripts.get(loc.scriptId);
  if (!s) return null;
  if (!s.map) return { file: s.rel, line: loc.lineNumber + 1, abs: s.abs };
  const e = s.map.findEntry(loc.lineNumber, loc.columnNumber || 0);
  if (!e || e.originalLine === undefined) return null;
  const abs = resolveSource(s, e.originalSource);
  return { file: abs.startsWith(ROOT) ? abs.slice(ROOT.length) : abs, line: e.originalLine + 1, abs };
}

function ser(ro) {
  if (!ro) return { type: "undefined", repr: "undefined", truncated: false };
  let repr;
  if (ro.type === "string") repr = JSON.stringify(ro.value);
  else if ("value" in ro || ro.unserializableValue) repr = String(ro.unserializableValue ?? JSON.stringify(ro.value));
  else if (ro.preview) {
    const p = ro.preview;
    const props = p.properties.map((x) => (p.subtype === "array" ? "" : `${x.name}: `) + (REDACT.test(x.name) ? "[REDACTED]" : x.type === "string" ? JSON.stringify(x.value) : x.value));
    repr = p.subtype === "array" ? `[${props.join(", ")}${p.overflow ? ", …" : ""}]` : `${ro.className === "Object" ? "" : ro.className + " "}{${props.join(", ")}${p.overflow ? ", …" : ""}}`;
  } else repr = ro.description || ro.type;
  const truncated = repr.length > 200 || !!(ro.preview && ro.preview.overflow);
  return { type: ro.subtype || ro.className || ro.type, repr: repr.slice(0, 200), truncated };
}

async function locals(frame) {
  const out = {};
  for (const sc of frame.scopeChain) {
    if (sc.type !== "local" && sc.type !== "block") continue;
    const { result } = await post("Runtime.getProperties", { objectId: sc.object.objectId, ownProperties: true, generatePreview: true });
    for (const p of result) if (!(p.name in out)) out[p.name] = REDACT.test(p.name) ? { type: "redacted", repr: "[REDACTED]", truncated: false } : ser(p.value);
  }
  return out;
}

const projectFrames = (frames) => frames.filter((f) => { const s = scripts.get(f.location.scriptId); return s && isProject(s.abs) || (s && s.map); });

async function onPaused(params) {
  const t0 = process.hrtime.bigint();
  stats.pauses++;
  if (process.env.VD_DEBUG) logErr("paused", `${params.reason} data=${JSON.stringify(params.data || {}).slice(0, 200)} hit=${params.hitBreakpoints}`);
  try {
    if (params.reason === "instrumentation") {
      await setupScript(params.data.scriptId);
      return;
    }
    const frames = projectFrames(params.callFrames);
    const top = frames[0];
    const o = top && original(top.location);
    if (!o || !isProject(o.abs)) { if (win) return void (await step("stepInto")); return; }
    if (!win) {
      if (!(FOCUS[o.file] || []).some(([a, b]) => o.line >= a && o.line <= b)) return; // by-URL bp on a transformed file
      win = { base: frames.length, prevDepth: 0, ids: [] };
    }
    const depth = frames.length - win.base + 1;
    if (depth < 1) { win = null; flush(); return; }
    let kind = "line";
    if (top.returnValue) kind = "return";
    else if (depth > win.prevDepth) kind = "call";
    if (kind === "call") win.ids[depth] = ++frameSeq;
    win.prevDepth = kind === "return" ? depth - 1 : depth;
    const ev = { event: kind, file: o.file, line: o.line, function: top.functionName || "(anonymous)", frame_id: win.ids[depth], depth };
    if (kind === "return") ev.return_value = ser(top.returnValue);
    else ev.locals = await locals(top);
    emit(ev);
    if (kind === "return" && depth === 1) { win = null; flush(); return; }
    await step("stepInto");
  } catch (e) {
    logErr("paused", e);
  } finally {
    stats.pauseMs += Number(process.hrtime.bigint() - t0) / 1e6;
    if (!win || params.reason === "instrumentation") post("Debugger.resume").catch(() => {});
  }
}
let stepping = false;
async function step(kind) { await post(`Debugger.${kind}`); }

function setupScript(scriptId) {
  const s = scripts.get(scriptId);
  if (!s) return Promise.resolve();
  return (s.setup ||= setupScriptOnce(scriptId, s)); // instrumentation pause must await an in-flight setup
}
async function setupScriptOnce(scriptId, s) {
  if (!s.map) {
    for (const [a, b] of FOCUS[s.rel] || []) for (let l = a; l <= b; l++)
      await post("Debugger.setBreakpoint", { location: { scriptId, lineNumber: l - 1 } }).catch(() => {});
    return;
  }
  // Every breakable generated location, mapped back; first one per focus line wins.
  // (Works for minified one-line output like tsx/esbuild; line-based scanning does not.)
  const { scriptSource } = await post("Debugger.getScriptSource", { scriptId });
  const lines = scriptSource.split("\n");
  const { locations } = await post("Debugger.getPossibleBreakpoints", {
    start: { scriptId, lineNumber: 0, columnNumber: 0 },
    end: { scriptId, lineNumber: lines.length - 1, columnNumber: lines[lines.length - 1].length },
  });
  const seen = new Set();
  for (const loc of locations) {
    const e = s.map.findEntry(loc.lineNumber, loc.columnNumber || 0);
    if (!e || e.originalSource === undefined) continue;
    const rel = resolveSource(s, e.originalSource).slice(ROOT.length);
    const ol = e.originalLine + 1;
    if (seen.has(ol) || !(FOCUS[rel] || []).some(([a, b]) => ol >= a && ol <= b)) continue;
    seen.add(ol);
    await post("Debugger.setBreakpoint", { location: { scriptId, lineNumber: loc.lineNumber, columnNumber: loc.columnNumber } }).catch(() => {});
  }
}

session.on("Debugger.paused", ({ params }) => { onPaused(params); });

(async () => {
  try {
    await post("Debugger.enable");
    await post("Debugger.setBlackboxPatterns", { patterns: ["/node_modules/", "^node:", "^internal/"] });
    // Exact for untransformed files (CJS/ESM): V8 resolves by-URL breakpoints at compile time.
    for (const [rel, ranges] of Object.entries(FOCUS)) {
      const re = escapeRe(path.join(ROOT, rel)) + "$";
      for (const [a, b] of ranges) for (let l = a; l <= b; l++) await post("Debugger.setBreakpointByUrl", { urlRegex: re, lineNumber: l - 1 });
    }
    // Transformed scripts run via vm.Script / ESM (Vitest, tsx): pause before they execute, map, set bps.
    await post("Debugger.setInstrumentationBreakpoint", { instrumentation: "beforeScriptWithSourceMapExecution" });
  } catch (e) { logErr("setup", e); }
  Atomics.store(flag, 0, 1);
  Atomics.notify(flag, 0);
})();
process.on("exit", () => { flush(); if (process.env.VD_DEBUG) console.error(`[vd] pauses=${stats.pauses} pauseMs=${stats.pauseMs.toFixed(1)}`); });
