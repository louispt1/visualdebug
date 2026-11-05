// Phase 0 spike — throwaway. Loaded via NODE_OPTIONS="--require <this>".
// Spawns an inspector worker connected to the main thread and blocks until it has set breakpoints.
"use strict";
(() => {
  const wt = require("node:worker_threads");
  if (!wt.isMainThread || !process.env.VISUALDEBUG_CONFIG || globalThis.__visualdebugProbe) return;
  globalThis.__visualdebugProbe = true;
  try {
    const sab = new SharedArrayBuffer(4);
    const flag = new Int32Array(sab);
    const w = new wt.Worker(require("node:path").join(__dirname, "worker.cjs"), {
      workerData: { sab, configPath: process.env.VISUALDEBUG_CONFIG },
      env: { ...process.env, NODE_OPTIONS: "" }, // don't recurse into ourselves
      execArgv: [],
    });
    w.unref();
    const t0 = process.hrtime.bigint();
    const r = Atomics.wait(flag, 0, 0, 10000);
    if (process.env.VD_DEBUG) console.error(`[vd] handshake ${r} in ${Number(process.hrtime.bigint() - t0) / 1e6}ms`);
  } catch (e) {
    try { require("node:fs").appendFileSync(".visualdebug/probe-errors.log", `${process.pid} probe: ${e.stack}\n`); } catch {}
  }
})();
