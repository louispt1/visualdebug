// visualdebug Node probe (main thread). Loaded with NODE_OPTIONS="--require <this>".
// Starts an inspector worker connected to this thread and blocks until its breakpoints are set.
// Must never change the program's behaviour: every failure is logged and swallowed.
"use strict";
(() => {
  const KEY = Symbol.for("visualdebug.probe");
  let wt;
  try {
    wt = require("node:worker_threads");
  } catch {
    return;
  }
  if (!wt.isMainThread || !process.env.VISUALDEBUG_CONFIG || globalThis[KEY]) return;
  globalThis[KEY] = true;
  const fs = require("node:fs");
  const path = require("node:path");
  let outDir = ".";
  const logErr = (where, e) => {
    try {
      fs.appendFileSync(path.join(outDir, "probe-errors.log"), `${process.pid} node ${where}: ${(e && e.stack) || e}\n`);
    } catch {}
  };
  try {
    const cfg = JSON.parse(fs.readFileSync(process.env.VISUALDEBUG_CONFIG, "utf8"));
    outDir = cfg.out_dir;
    // [0] ready, [1] exit requested, [2] worker done
    const flag = new Int32Array(new SharedArrayBuffer(12));
    const worker = new wt.Worker(path.join(__dirname, "visualdebug_worker.cjs"), {
      workerData: { flag: flag.buffer, configPath: process.env.VISUALDEBUG_CONFIG },
      env: { ...process.env, NODE_OPTIONS: "" },
      execArgv: [],
      stdout: false,
      stderr: false,
    });
    worker.unref();
    worker.on("error", (e) => logErr("worker", e));
    // The worker's inspector commands are serviced while this thread waits.
    if (Atomics.wait(flag, 0, 0, 10000) === "timed-out") {
      logErr("handshake", "inspector worker did not get ready within 10s; not tracing this process");
      worker.terminate();
      return;
    }
    process.on("exit", () => {
      // Let the worker flush and disconnect, so Node doesn't wait for "the debugger" at exit.
      try {
        worker.postMessage("exit");
        Atomics.wait(flag, 2, 0, 1000);
      } catch (e) {
        logErr("exit", e);
      }
    });
  } catch (e) {
    logErr("probe", e);
  }
})();
