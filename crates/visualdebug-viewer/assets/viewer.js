"use strict";
// visualdebug viewer. Plain JS, no build step, no network. Data is inlined by the Rust bundler.
(function () {
  const DATA = JSON.parse(document.getElementById("vd-data").textContent);
  const $ = (id) => document.getElementById(id);
  const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  const CALLS = new Set(["call", "block_call"]);
  const RETURNS = new Set(["return", "block_return"]);

  // ------------------------------------------------------------------ preparation
  // Everything here is O(steps) once per trace; per-step rendering stays cheap.
  function prepare(raw, index) {
    const steps = raw.steps;
    const frames = new Map(); // frame -> {fn, file, parent, idx:[step indices], loc:[indices with locals], line:[indices with a line]}
    const lineSteps = new Map(); // file -> Map(line -> [indices])
    const effects = [];
    const stacks = new Map(); // pid -> [{frame, depth}]
    const frame = (f, s, parent) => {
      let fr = frames.get(f);
      if (!fr) {
        fr = { id: f, fn: s.function || "?", file: s.file, parent: parent ?? null, idx: [], loc: [], line: [] };
        frames.set(f, fr);
      }
      return fr;
    };
    for (let i = 0; i < steps.length; i++) {
      const s = steps[i];
      let st = stacks.get(s.pid);
      if (!st) stacks.set(s.pid, (st = []));
      if (CALLS.has(s.kind) && !s.library) {
        while (st.length && st[st.length - 1].depth >= s.depth) st.pop();
        const parent = st.length ? st[st.length - 1].frame : null;
        frame(s.frame, s, parent);
        st.push({ frame: s.frame, depth: s.depth });
      } else if (!frames.has(s.frame)) {
        while (st.length && st[st.length - 1].depth >= s.depth) st.pop();
        frame(s.frame, s, st.length ? st[st.length - 1].frame : null);
        if (!s.library) st.push({ frame: s.frame, depth: s.depth });
      }
      const fr = frames.get(s.frame);
      fr.idx.push(i);
      if (s.locals) fr.loc.push(i);
      if (s.file && s.line && !s.library) {
        fr.line.push(i);
        if (s.kind === "line" || CALLS.has(s.kind)) {
          let m = lineSteps.get(s.file);
          if (!m) lineSteps.set(s.file, (m = new Map()));
          let a = m.get(s.line);
          if (!a) m.set(s.line, (a = []));
          a.push(i);
        }
      }
      if (RETURNS.has(s.kind) && !s.library) {
        const k = st.findIndex((x) => x.frame === s.frame);
        if (k >= 0) st.length = k;
      }
      if (s.kind === "side_effect") effects.push(i);
    }
    const focus = raw.focus && raw.focus.files ? raw.focus.files : {};
    const lineLevel = (f) => Object.keys(focus).length === 0 || f in focus;
    const ann = raw.annotations || null;
    const stopAt = new Map();
    const notesAt = new Map();
    if (ann) {
      (ann.tour || []).forEach((t, k) => stopAt.set(t.step - 1, k));
      (ann.notes || []).forEach((n) => {
        const a = notesAt.get(n.step - 1) || [];
        a.push(n);
        notesAt.set(n.step - 1, a);
      });
    }
    return { raw, index, steps, frames, lineSteps, effects, lineLevel, ann, stopAt, notesAt,
             cur: 0, file: null, tourOn: !!ann, tourIdx: ann ? -1 : null, quiz: {},
             ids: { code: "code", fileName: "fileName", where: "where", vars: "vars" } };
  }

  // last element of sorted `arr` that is <= i
  function lastLE(arr, i) {
    let lo = 0, hi = arr.length - 1, ans = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (arr[mid] <= i) { ans = arr[mid]; lo = mid + 1; } else hi = mid - 1;
    }
    return ans;
  }
  function firstGT(arr, i) {
    let lo = 0, hi = arr.length - 1, ans = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (arr[mid] > i) { ans = arr[mid]; hi = mid - 1; } else lo = mid + 1;
    }
    return ans;
  }

  const traces = DATA.traces.map(prepare);
  // compare: a HEAD trace carrying an alignment with its base-ref twin (which gets no tab of its own)
  for (const t of traces) {
    const c = t.raw.compare;
    const base = c && traces.find((x) => x.raw.name === c.base_trace);
    if (base) {
      const pairs = c.alignment;
      const lastB = [], lastH = [], kOfHead = new Map();
      let lb = 0, lh = 0;
      pairs.forEach(([b, h], k) => {
        if (b) lb = b - 1;
        if (h) { lh = h - 1; if (!kOfHead.has(lh)) kOfHead.set(lh, k); }
        lastB.push(lb);
        lastH.push(lh);
      });
      t.cmp = { base, pairs, lastB, lastH, kOfHead, div: c.divergence, ref: c.base_ref, k: 0 };
      base.isBase = true;
      base.ids = { code: "code2", fileName: "fileName2", where: "where2", vars: "vars2" };
    }
  }
  const tabs = traces.filter((t) => !t.isBase);
  let T = tabs[0];

  // ------------------------------------------------------------------ helpers
  const step = (i = T.cur) => T.steps[i];
  const fileOf = (i) => {
    const s = step(i);
    if (s.file && !s.library) return { file: s.file, line: s.line };
    const fr = T.frames.get(s.frame);
    const j = fr ? lastLE(fr.line, i) : -1;
    if (j >= 0) return { file: T.steps[j].file, line: T.steps[j].line };
    return { file: s.file || null, line: s.line || null };
  };
  const localsAt = (i) => {
    const s = step(i);
    const fr = T.frames.get(s.frame);
    const j = fr ? lastLE(fr.loc, i) : -1;
    return j >= 0 ? { at: j, locals: T.steps[j].locals } : { at: -1, locals: null };
  };
  const isChanged = (file, line) => {
    const r = T.raw.focus && T.raw.focus.files && T.raw.focus.files[file];
    return !!r && r.some(([a, b]) => line >= a && line <= b);
  };
  const short = (s, n) => (s.length > n ? s.slice(0, n) + "…" : s);
  const fnName = (s) => s.function || "?";

  // Tiny, safe markdown: escape first, then a few inline/block forms. `#123` links to step 123.
  function md(src) {
    if (!src) return "";
    const inline = (t) =>
      esc(t)
        .replace(/`([^`]+)`/g, "<code>$1</code>")
        .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
        .replace(/(^|[^*])\*([^*\s][^*]*)\*/g, "$1<em>$2</em>")
        .replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>')
        .replace(/(^|[\s(])#(\d+)\b/g, '$1<a data-step="$2">#$2</a>');
    const out = [];
    const parts = String(src).split(/```[a-z]*\n?/);
    parts.forEach((part, k) => {
      if (k % 2 === 1) { out.push(`<pre><code>${esc(part.replace(/\n$/, ""))}</code></pre>`); return; }
      for (const block of part.split(/\n\s*\n/)) {
        const lines = block.split("\n").filter((l) => l.trim());
        if (!lines.length) continue;
        if (lines.every((l) => /^\s*([-*]|\d+\.)\s+/.test(l))) {
          const tag = /^\s*\d+\./.test(lines[0]) ? "ol" : "ul";
          out.push(`<${tag}>${lines.map((l) => `<li>${inline(l.replace(/^\s*([-*]|\d+\.)\s+/, ""))}</li>`).join("")}</${tag}>`);
        } else if (/^#{1,4}\s/.test(lines[0])) {
          out.push(`<h4>${inline(lines[0].replace(/^#+\s*/, ""))}</h4>`);
          if (lines.length > 1) out.push(`<p>${inline(lines.slice(1).join(" "))}</p>`);
        } else out.push(`<p>${inline(lines.join(" "))}</p>`);
      }
    });
    return out.join("");
  }

  // ------------------------------------------------------------------ navigation
  function go(i, opts = {}) {
    if (!T.steps.length) return;
    if (T.cmp && !opts.raw) {
      const k = T.cmp.kOfHead.get(Math.max(0, Math.min(T.steps.length - 1, i)));
      if (k !== undefined) return cmpGo(k);
    }
    T.cur = Math.max(0, Math.min(T.steps.length - 1, i));
    if (T.tourOn && T.ann && !opts.fromTour) {
      const k = T.stopAt.get(T.cur);
      if (k !== undefined) T.tourIdx = k;
    }
    render();
  }
  // compare mode walks the alignment: both sides advance together, one waits where the other has extra steps
  function cmpGo(k) {
    const c = T.cmp;
    c.k = Math.max(0, Math.min(c.pairs.length - 1, k));
    c.base.cur = c.lastB[c.k];
    go(c.lastH[c.k], { raw: true, fromTour: true });
  }
  const nav = {
    fwd: () => (T.cmp ? cmpGo(T.cmp.k + 1) : go(T.cur + 1)),
    back: () => (T.cmp ? cmpGo(T.cmp.k - 1) : go(T.cur - 1)),
    start: () => (T.cmp ? cmpGo(0) : go(0)),
    end: () => (T.cmp ? cmpGo(T.cmp.pairs.length - 1) : go(T.steps.length - 1)),
    over: () => {
      const d = step().depth;
      for (let j = T.cur + 1; j < T.steps.length; j++) if (T.steps[j].depth <= d) return go(j);
      go(T.steps.length - 1);
    },
    backOver: () => {
      const d = step().depth;
      for (let j = T.cur - 1; j >= 0; j--) if (T.steps[j].depth <= d) return go(j);
      go(0);
    },
    out: () => {
      const d = step().depth;
      for (let j = T.cur + 1; j < T.steps.length; j++) if (T.steps[j].depth < d) return go(j);
      go(T.steps.length - 1);
    },
  };
  function gotoLine(file, line, backwards) {
    const m = T.lineSteps.get(file);
    const arr = m && m.get(line);
    if (!arr || !arr.length) return flash(`line ${line} never ran in this trace`);
    let j;
    if (backwards) { j = lastLE(arr, T.cur - 1); if (j < 0) j = arr[arr.length - 1]; }
    else { j = firstGT(arr, T.cur); if (j < 0) j = arr[0]; }
    go(j);
  }
  let flashTimer;
  function flash(msg) {
    $("status").textContent = msg;
    clearTimeout(flashTimer);
    flashTimer = setTimeout(renderStatus, 1600);
  }

  // ------------------------------------------------------------------ rendering
  function renderTabs() {
    const bar = $("tabs");
    bar.innerHTML = "";
    if (tabs.length < 2) return;
    tabs.forEach((t, k) => {
      const b = document.createElement("button");
      b.className = "tab";
      b.setAttribute("role", "tab");
      b.setAttribute("aria-selected", String(t === T));
      b.textContent = `${k + 1}. ${t.raw.name}`;
      b.onclick = () => switchTrace(k);
      bar.appendChild(b);
    });
  }
  function switchTrace(k) {
    if (!tabs[k] || tabs[k] === T) return;
    T = tabs[k];
    T.file = null;
    renderTabs();
    renderMeta();
    drawTimelineBase();
    render();
  }
  function renderMeta() {
    const r = T.raw;
    let h = esc(r.command && r.command.length ? r.command.join(" ") : r.name);
    if (r.exit_code) h += `<span class="badge fail" title="The traced command failed; the trace shows what happened.">exit ${r.exit_code}</span>`;
    if (r.truncated) h += `<span class="badge warn" title="Recording stopped at --max-events">truncated</span>`;
    $("meta").innerHTML = h;
    $("meta").title = $("meta").textContent;
    $("tourToggle").hidden = !T.ann;
    $("tourToggle").setAttribute("aria-pressed", String(T.tourOn));
  }

  function renderCode() {
    const { file, line } = fileOf(T.cur);
    const code = $(T.ids.code);
    if (file !== T.file) {
      T.file = file;
      $(T.ids.fileName).textContent = file || "(no source location)";
      const fd = file && T.raw.files[file];
      if (!fd) {
        code.innerHTML = `<div class="banner">${file ? "Source not available for " + esc(file) : "This step has no source location."}</div>`;
      } else {
        const counts = T.lineSteps.get(file) || new Map();
        const ll = T.lineLevel(file);
        const html = [];
        fd.lines.forEach((src, k) => {
          const n = k + 1;
          const c = counts.get(n);
          const cls = ["ln"];
          if (isChanged(file, n)) cls.push("chg");
          if (ll && !c) cls.push("dim");
          html.push(`<div class="${cls.join(" ")}" data-line="${n}" role="listitem"><span class="gut"><span class="cnt">${c ? "×" + c.length : ""}</span><span class="num">${n}</span></span><span class="src">${src || " "}</span></div>`);
        });
        code.innerHTML = html.join("");
      }
      T.curEl = null;
    }
    if (T.curEl) { T.curEl.classList.remove("cur", "ret", "raise"); const old = T.curEl.querySelector(".inline"); if (old) old.remove(); }
    const el = line ? code.querySelector(`.ln[data-line="${line}"]`) : null;
    T.curEl = el;
    if (!el) return;
    const s = step();
    el.classList.add("cur");
    if (RETURNS.has(s.kind) && !s.library) el.classList.add("ret");
    if (s.kind === "raise") el.classList.add("raise");
    const chip = inlineChip(s);
    if (chip) el.querySelector(".src").insertAdjacentHTML("beforeend", `<span class="inline">${chip}</span>`);
    const top = el.offsetTop, h = code.clientHeight;
    if (top < code.scrollTop + 20 || top > code.scrollTop + h - 60) code.scrollTop = top - h / 3;
  }
  function inlineChip(s) {
    if (s.library) return `→ ${esc(fnName(s))}${s.return_value ? " ⇒ <b>" + esc(short(s.return_value.repr, 60)) + "</b>" : ""}`;
    if (RETURNS.has(s.kind) && s.return_value) return `returns <b>${esc(short(s.return_value.repr, 60))}</b>`;
    if (s.kind === "raise" && s.exception) return `raises <b>${esc(s.exception.type)}</b>`;
    if (s.kind === "side_effect") return `<b>${esc(effectLabel(s))}</b>`;
    if (s.diff && s.locals) {
      const parts = [];
      for (const k of s.diff.added || []) parts.push(`${esc(k)} = <b>${esc(short(s.locals[k].repr, 40))}</b>`);
      for (const k of Object.keys(s.diff.changed || {})) parts.push(`${esc(k)} = <b>${esc(short(s.locals[k].repr, 40))}</b>`);
      if (parts.length) return parts.slice(0, 3).join(", ") + (parts.length > 3 ? " …" : "");
    }
    return "";
  }
  function effectLabel(s) {
    const e = s.side_effect || {};
    if (e.kind === "sql") return "SQL " + short((e.sql || "").replace(/\s+/g, " "), 90);
    return (e.kind || "effect") + " " + short(JSON.stringify(e), 90);
  }

  function renderVars() {
    const s = step();
    const hl = new Set();
    const stop = T.tourOn && T.ann && T.tourIdx >= 0 ? T.ann.tour[T.tourIdx] : null;
    if (stop && stop.step - 1 === T.cur) (stop.highlight_vars || []).forEach((v) => hl.add(v));
    const rows = [];
    const special = (k, v, title) =>
      rows.push(`<div class="var special" title="${esc(title || "")}"><span class="k">${esc(k)}</span><span class="v">${esc(v)}</span></div>`);
    if (s.library) special("→ call", fnName(s) + " (library)" + (s.return_value ? " ⇒ " + s.return_value.repr : ""));
    if (RETURNS.has(s.kind) && s.return_value && !s.library) special("return", s.return_value.repr, s.return_value.type);
    if (s.kind === "raise" && s.exception) special("raise", `${s.exception.type}: ${s.exception.message}`);
    if (s.kind === "side_effect") special(s.side_effect && s.side_effect.kind || "effect", effectLabel(s).replace(/^SQL /, ""));
    const { at, locals } = localsAt(T.cur);
    const diff = at === T.cur ? s.diff : null;
    if (locals) {
      for (const [k, v] of Object.entries(locals)) {
        const cls = ["var"];
        let val = esc(v.repr);
        if (diff && (diff.added || []).includes(k)) cls.push("new");
        if (diff && diff.changed && k in diff.changed) {
          cls.push("chg");
          val = `<del>${esc(diff.changed[k])}</del> → ${val}`;
          if ((diff.mutated || []).includes(k)) val += `<span class="mut">mutated</span>`;
        }
        if (hl.has(k)) cls.push("hl");
        rows.push(`<div class="${cls.join(" ")}" data-var="${esc(k)}" title="${esc(v.type)}${v.truncated ? " (truncated)" : ""}}. Click the name for value history"><span class="k">${esc(k)}</span><span class="v${openVars.has(k) ? " open" : ""}">${val}</span></div>`);
      }
    }
    $(T.ids.vars).innerHTML = rows.length ? rows.join("") : `<div class="empty">No variables recorded at this step${T.lineLevel(fileOf(T.cur).file || "") ? "" : " (file outside focus: calls only)"}.</div>`;
  }

  function renderStack() {
    const s = step();
    const items = [];
    if (s.library) items.push(`<li class="lib">→ ${esc(fnName(s))} (library)</li>`);
    let f = s.frame, first = true, guard = 0;
    while (f != null && T.frames.has(f) && guard++ < 200) {
      const fr = T.frames.get(f);
      const j = first ? T.cur : lastLE(fr.line, T.cur);
      const at = j >= 0 ? T.steps[j] : null;
      const loc = at && at.file ? `${at.file}:${at.line}` : "";
      items.push(`<li data-step="${j}" title="${esc(loc)}">${esc(fr.fn)}<span class="loc">${esc(loc)}</span></li>`);
      f = fr.parent;
      first = false;
    }
    $("stack").innerHTML = items.join("") || `<li class="empty">none</li>`;
  }

  // Virtualized fixed-height list.
  function vlist(container, count, render) {
    container.innerHTML = "";
    const inner = document.createElement("div");
    inner.style.height = count * 20 + "px";
    inner.style.position = "relative";
    container.appendChild(inner);
    const draw = () => {
      const top = Math.floor(container.scrollTop / 20), n = Math.ceil(container.clientHeight / 20) + 2;
      let h = "";
      for (let k = top; k < Math.min(count, top + n); k++) h += render(k).replace("<div class=\"row", `<div style="top:${k * 20}px" class="row`);
      inner.innerHTML = h;
    };
    container.onscroll = draw;
    draw();
    return { draw, scrollTo: (k) => { const h = container.clientHeight; if (k * 20 < container.scrollTop || k * 20 > container.scrollTop + h - 20) container.scrollTop = Math.max(0, k * 20 - h / 2); draw(); } };
  }

  function renderEffects() {
    const n = T.effects.length;
    $("effectsCount").textContent = n ? `(${n})` : "";
    if (!n) { $("effects").innerHTML = `<div class="empty">None recorded.</div>`; effOwner = null; return; }
    const nearest = lastLE(T.effects, T.cur);
    if (effOwner !== T) {
      effOwner = T;
      effList = vlist($("effects"), n, (k) => {
        const i = T.effects[k];
        const s = T.steps[i];
        const e = s.side_effect || {};
        const cls = i > T.cur ? " future" : i === nearestNow() ? " at" : "";
        const label = e.kind === "sql" ? `<span class="k-sql">SQL</span>${esc((e.sql || "").replace(/\s+/g, " "))}${e.duration_ms != null ? ` <span class="muted">(${(+e.duration_ms).toFixed(1)}ms)</span>` : ""}` : esc(effectLabel(s));
        return `<div class="row${cls}" data-step="${i}" title="${esc(effectLabel(s))}"><span class="id">#${i + 1}</span>${label}</div>`;
      });
    }
    effList.scrollTo(Math.max(0, T.effects.indexOf(nearest)));
  }
  let effOwner = null, effList = null;
  const openVars = new Set();
  const nearestNow = () => lastLE(T.effects, T.cur);

  function showHistory(name, fn) {
    const hist = [];
    let prev = null, prevFrame = null;
    for (let i = 0; i < T.steps.length; i++) {
      const x = T.steps[i];
      if (!x.locals || !(name in x.locals)) continue;
      const fr = T.frames.get(x.frame);
      if (!fr || fr.fn !== fn) continue;
      const r = x.locals[name].repr;
      if (r !== prev || x.frame !== prevFrame) hist.push({ i, r, newFrame: x.frame !== prevFrame });
      prev = r;
      prevFrame = x.frame;
    }
    $("historyPanel").hidden = false;
    $("historyTitle").textContent = `${name} in ${fn}: ${hist.length} value${hist.length === 1 ? "" : "s"}`;
    const list = vlist($("history"), hist.length, (k) => {
      const h = hist[k];
      const s2 = T.steps[h.i];
      return `<div class="row${h.i > T.cur ? " future" : ""}" data-step="${h.i}" title="${esc(h.r)}"><span class="id">#${h.i + 1}</span>${h.newFrame && k > 0 ? "↪ " : ""}${esc(h.r)} <span class="muted">${esc(s2.file || "")}:${s2.line || ""}</span></div>`;
    });
    const k = hist.findIndex((h) => h.i > T.cur);
    list.scrollTo(k < 0 ? hist.length - 1 : Math.max(0, k - 1));
  }

  // ------------------------------------------------------------------ tour & quiz
  function renderTour() {
    const panel = $("tour");
    if (!T.ann || !T.tourOn) { panel.hidden = true; return; }
    panel.hidden = false;
    const a = T.ann, stops = a.tour || [], quiz = a.quiz || [];
    const total = stops.length;
    let h = "";
    const notes = (T.notesAt.get(T.cur) || []).map((n) => `<div class="note">${md(n.body)}</div>`).join("");
    if (T.tourIdx === -1) {
      h += `<div class="progress"><span>Overview</span><span>${total} stops${quiz.length ? ` · ${quiz.length}-question quiz` : ""}</span></div>`;
      h += `<h3>${esc(a.title || "Guided tour")}</h3><div class="body">${md(a.overview)}</div>`;
      h += `<div class="nav"><button class="btn primary" data-tour="next">${total ? "Start tour →" : "Quiz →"}</button></div>`;
    } else if (T.tourIdx < total) {
      const st = stops[T.tourIdx];
      h += `<div class="progress"><span>Stop ${T.tourIdx + 1} of ${total} · step #${st.step}</span><button class="linkish" data-tour="overview">overview</button></div>`;
      if (st.step - 1 !== T.cur) h += `<div class="away">You're exploring freely (step #${T.cur + 1}). <button class="linkish" data-tour="return">Back to stop ${T.tourIdx + 1}</button></div>`;
      h += `<h3>${esc(st.title || "")}</h3><div class="body">${md(st.body)}</div>`;
      h += `<div class="nav"><button class="btn" data-tour="prev">← Prev</button><button class="btn primary" data-tour="next">${T.tourIdx + 1 < total ? "Next →" : quiz.length ? "Quiz →" : "Finish"}</button><span class="muted">n / p</span></div>`;
    } else {
      h += `<div class="progress"><span>Check your understanding</span><button class="linkish" data-tour="overview">overview</button></div>`;
      h += renderQuiz(quiz);
      h += `<div class="nav"><button class="btn" data-tour="prev">← Back to the tour</button></div>`;
    }
    if (notes && T.tourIdx < total) h += notes;
    panel.innerHTML = h;
  }
  function renderQuiz(quiz) {
    if (!quiz.length) return `<p>That's the whole tour. Keep scrubbing freely, or press <kbd>t</kbd> to hide this panel.</p>`;
    let score = 0, answered = 0;
    const qs = quiz.map((q, qi) => {
      const chosen = T.quiz[qi];
      if (chosen != null) { answered++; if (chosen === q.answer) score++; }
      const opts = q.options.map((o, oi) => {
        let cls = "opt";
        if (chosen != null && oi === q.answer) cls += " right";
        else if (chosen === oi) cls += " wrong";
        return `<button class="${cls}" data-q="${qi}" data-o="${oi}" ${chosen != null ? "disabled" : ""}>${md(o).replace(/^<p>|<\/p>$/g, "")}</button>`;
      }).join("");
      const ex = chosen != null
        ? `<div class="explain">${chosen === q.answer ? "✓ Right." : "✗ Not quite."} ${md(q.explanation).replace(/^<p>|<\/p>$/g, "")}${q.step ? ` <button class="linkish" data-step="${q.step - 1}">Show me (#${q.step})</button>` : ""}</div>`
        : "";
      return `<div class="q"><strong>${qi + 1}. </strong>${md(q.question).replace(/^<p>|<\/p>$/g, "")}${opts}${ex}</div>`;
    });
    return `<div class="quiz">${qs.join("")}${answered === quiz.length ? `<p><strong>${score} / ${quiz.length}</strong> correct.</p>` : ""}</div>`;
  }
  function tourGo(k) {
    if (!T.ann) return;
    const stops = T.ann.tour || [];
    T.tourOn = true;
    T.tourIdx = Math.max(-1, Math.min(stops.length, k));
    if (T.tourIdx >= 0 && T.tourIdx < stops.length) go(stops[T.tourIdx].step - 1, { fromTour: true });
    else render();
    $("tourToggle").setAttribute("aria-pressed", "true");
  }

  // ------------------------------------------------------------------ compare
  function renderCompare() {
    const on = !!T.cmp;
    $("main").classList.toggle("compare", on);
    $("basePane").hidden = !on;
    $("vars2Panel").hidden = !on;
    $("cmpPanel").hidden = !on;
    $("headLabel").hidden = !on;
    $("varsTitle").textContent = on ? "Variables (HEAD)" : "Variables";
    if (!on) return;
    const c = T.cmp, [b, h] = c.pairs[c.k];
    const head = T;
    T = c.base;
    renderCode();
    renderVars();
    T = head;
    $("baseLabel").textContent = c.ref;
    $("basePane").classList.toggle("missing", !b);
    $("headPane").classList.toggle("missing", !h);
    const d = c.div;
    let html = `<div class="progress"><span>Comparing ${esc(c.ref)} ↔ HEAD</span><span class="pair">${c.k + 1} / ${c.pairs.length}</span></div>`;
    html += `<div class="pair">base ${b ? "#" + b : "none"} ↔ HEAD ${h ? "#" + h : "none"}</div>`;
    if (d) {
      const here = d.at === c.k;
      html += `<div class="div"><strong>${here ? "This is where behaviour diverges" : "First divergence"} (${esc(d.kind)})</strong><br>${esc(d.description)}</div>`;
      if (!here) html += `<button class="btn primary" data-cmp="${d.at}">Jump to it</button>`;
    } else html += `<p>No divergence: both versions ran the same code with the same values.</p>`;
    $("cmpPanel").innerHTML = html;
  }

  // ------------------------------------------------------------------ timeline
  const tl = $("tl");
  let tlBase = null;
  // ponytail: names hash into 8 colours, so neighbours can share one; the divider and hover tell them apart
  const PALETTE = 8;
  const hash = (str) => { let h = 0; for (const c of str) h = (h * 31 + c.charCodeAt(0)) | 0; return h >>> 0; };
  // null = grey: library calls and files outside the focus
  const colourKey = (s) => (s.library || !s.file || !T.lineLevel(s.file) ? null : (T.frames.get(s.frame) || {}).fn || fnName(s));
  const stepAtX = (ev) => {
    const r = tl.getBoundingClientRect();
    return Math.min(T.steps.length - 1, Math.max(0, Math.floor(((ev.clientX - r.left) / r.width) * T.steps.length)));
  };
  function drawTimelineBase() {
    const rect = tl.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const W = Math.max(1, Math.floor(rect.width * dpr)), H = Math.max(1, Math.floor(rect.height * dpr));
    tl.width = W; tl.height = H;
    const base = document.createElement("canvas");
    base.width = W; base.height = H;
    const g = base.getContext("2d");
    const css = getComputedStyle(document.documentElement);
    const col = (v) => css.getPropertyValue(v).trim();
    const n = T.steps.length;
    if (!n) { tlBase = base; drawTimeline(); return; }
    const maxD = Math.max(1, ...T.steps.map((s) => s.depth));
    const minD = Math.min(...T.steps.map((s) => s.depth));
    const top = 12 * dpr, bottom = H - 8 * dpr, span = bottom - top;
    const pal = Array.from({ length: PALETTE }, (_, k) => col(`--f${k}`)), grey = col("--depth"), gap = col("--panel");
    const sqlMax = Math.max(0, ...T.effects.map((i) => ms(T.steps[i])));
    const marks = { sql: [], raise: [], chg: [] };
    let prevKey;
    for (let x = 0; x < W; x++) {
      const a = Math.floor((x / W) * n), b = Math.max(a + 1, Math.floor(((x + 1) / W) * n));
      let deep = null;
      for (let i = a; i < b && i < n; i++) {
        const s = T.steps[i];
        if (!deep || s.depth > deep.depth) deep = s;
        if (s.kind === "side_effect") marks.sql.push([x, ms(s)]);
        else if (s.kind === "raise") marks.raise.push(x);
        else if (s.file && s.line && s.kind === "line" && isChanged(s.file, s.line)) marks.chg.push(x);
      }
      if (!deep) continue;
      const key = colourKey(deep);
      const hgt = ((deep.depth - minD + 1) / (maxD - minD + 1)) * span;
      g.fillStyle = key == null ? grey : pal[hash(key) % PALETTE];
      g.fillRect(x, bottom - hgt, 1, hgt);
      if (x && key !== prevKey) { g.fillStyle = gap; g.fillRect(x, bottom - hgt, Math.max(1, dpr), hgt); }
      prevKey = key;
    }
    const tick = (xs, color, y, h) => { g.fillStyle = col(color); for (const x of new Set(xs)) g.fillRect(x, y, Math.max(1, dpr), h); };
    tick(marks.chg, "--chg", H - 6 * dpr, 6 * dpr);
    g.fillStyle = col("--sql");
    for (const [x, d] of marks.sql) {
      const h = 6 * dpr + (sqlMax ? (d / sqlMax) * (span - 6 * dpr) : 0); // taller = slower query
      g.fillRect(x, H - h, Math.max(1, dpr), h);
    }
    tick(marks.raise, "--raise", top, span);
    g.fillStyle = col("--accent");
    const dot = (i, r) => { g.beginPath(); g.arc(((i + 0.5) / n) * W, 6 * dpr, r * dpr, 0, Math.PI * 2); g.fill(); };
    if (T.ann) { (T.ann.tour || []).forEach((t) => dot(t.step - 1, 3.5)); (T.ann.notes || []).forEach((t) => dot(t.step - 1, 2)); }
    if (T.cmp && T.cmp.div) { g.fillStyle = col("--raise"); dot(T.cmp.lastH[T.cmp.div.at], 4); }
    tlBase = base;
    drawTimeline();
  }
  function drawTimeline() {
    if (!tlBase) return;
    const g = tl.getContext("2d");
    g.clearRect(0, 0, tl.width, tl.height);
    g.drawImage(tlBase, 0, 0);
    const n = T.steps.length;
    if (!n) return;
    const x = ((T.cur + 0.5) / n) * tl.width;
    const dpr = window.devicePixelRatio || 1;
    g.fillStyle = getComputedStyle(document.documentElement).getPropertyValue("--fg").trim();
    g.fillRect(Math.round(x - dpr), 0, 2 * dpr, tl.height);
    $("timeline").setAttribute("aria-valuemax", String(n));
    $("timeline").setAttribute("aria-valuenow", String(T.cur + 1));
  }
  function scrubTo(ev) { go(stepAtX(ev)); }
  function showTip(ev) {
    if (!T.steps.length) return;
    const s = T.steps[stepAtX(ev)], tip = $("tlTip");
    let text = `#${s.id} ${s.library ? "→ " + fnName(s) + " (library)" : (T.frames.get(s.frame) || {}).fn || fnName(s)}`;
    if (s.file) text += ` · ${s.file}:${s.line}`;
    if (s.kind === "side_effect") text += ` · ${effectLabel(s)}${ms(s) ? ` (${ms(s).toFixed(1)}ms)` : ""}`;
    if (s.kind === "raise" && s.exception) text += ` · raises ${s.exception.type}`;
    tip.textContent = text;
    tip.hidden = false;
    const r = tl.getBoundingClientRect(), half = tip.offsetWidth / 2;
    tip.style.left = Math.min(r.width - half, Math.max(half, ev.clientX - r.left)) + "px";
  }
  const ms = (s) => +((s.side_effect || {}).duration_ms) || 0;

  // ------------------------------------------------------------------ status + main render
  function renderStatus() {
    const s = step();
    if (!s) { $("status").textContent = "empty trace"; return; }
    const { file, line } = fileOf(T.cur);
    const kind = s.library ? "library call" : s.kind.replace("_", " ");
    $("status").textContent = `step ${T.cur + 1} / ${T.steps.length} · ${kind} · ${fnName(s)} · depth ${s.depth}` + (T.cmp ? ` · aligned ${T.cmp.k + 1}/${T.cmp.pairs.length}` : "");
    $(T.ids.where).textContent = file ? `line ${line} · ${(T.frames.get(s.frame) || {}).fn || fnName(s)}` : "";
  }
  function render() {
    if (!T.steps.length) {
      $("code").innerHTML = `<div class="banner">This trace recorded no steps.${T.raw.start_at ? " Did execution reach " + esc(T.raw.start_at) + "?" : ""}</div>`;
      ["vars", "stack", "effects"].forEach((id) => ($(id).innerHTML = ""));
      $("status").textContent = "empty trace";
      renderTour();
      return;
    }
    renderCode();
    renderVars();
    renderCompare();
    renderStack();
    renderEffects();
    renderTour();
    renderStatus();
    drawTimeline();
    if (!$("historyPanel").hidden && T.histVar) showHistory(T.histVar, T.histFn);
    const hash = T.cmp ? `cmp-${T.cmp.k + 1}` : (tabs.length > 1 ? `t${tabs.indexOf(T) + 1}/` : "") + `step-${T.cur + 1}`;
    if (location.hash !== "#" + hash) history.replaceState(null, "", "#" + hash);
  }

  function fromHash() {
    const c = /^#cmp-(\d+)$/.exec(location.hash);
    if (c && T.cmp) { cmpGo(+c[1] - 1); return true; }
    const m = /^#(?:t(\d+)\/)?step-(\d+)$/.exec(location.hash);
    if (!m) return false;
    if (m[1]) switchTrace(+m[1] - 1);
    go(+m[2] - 1);
    return true;
  }

  // ------------------------------------------------------------------ events
  document.addEventListener("click", (e) => {
    const t = e.target.closest("[data-act],[data-step],[data-tour],[data-q],[data-var],[data-cmp],.ln");
    if (!t || t.closest("#basePane")) return;
    if (t.dataset.cmp != null) return cmpGo(+t.dataset.cmp);
    if (t.dataset.act) return nav[t.dataset.act]();
    if (t.dataset.q != null) { T.quiz[+t.dataset.q] = +t.dataset.o; return renderTour(); }
    if (t.dataset.tour) {
      const stops = (T.ann && T.ann.tour) || [];
      const a = t.dataset.tour;
      if (a === "next") return tourGo(T.tourIdx + 1);
      if (a === "prev") return tourGo(T.tourIdx - 1);
      if (a === "overview") return tourGo(-1);
      if (a === "return") return tourGo(T.tourIdx);
      return;
    }
    if (t.dataset.step != null) {
      const k = +t.dataset.step;
      // links written in markdown use 1-based step ids; internal rows use 0-based indices
      return go(t.tagName === "A" ? k - 1 : k);
    }
    if (t.dataset.var) {
      // a value clamped to 3 lines expands/collapses; anything else opens the value history
      const v = e.target.closest(".v");
      if (v && (v.classList.contains("open") || v.scrollHeight > v.clientHeight + 1)) {
        v.classList.toggle("open");
        return v.classList.contains("open") ? openVars.add(t.dataset.var) : openVars.delete(t.dataset.var);
      }
      const fr = T.frames.get(step().frame);
      T.histVar = t.dataset.var;
      T.histFn = fr ? fr.fn : null;
      return showHistory(T.histVar, T.histFn);
    }
    if (t.classList.contains("ln") && T.file) return gotoLine(T.file, +t.dataset.line, e.shiftKey);
  });
  $("historyClose").onclick = () => { $("historyPanel").hidden = true; T.histVar = null; };
  $("helpBtn").onclick = () => ($("help").hidden = false);
  $("help").onclick = (e) => { if (e.target === $("help")) $("help").hidden = true; };
  $("tourToggle").onclick = () => toggleTour();
  function toggleTour() {
    if (!T.ann) return;
    T.tourOn = !T.tourOn;
    $("tourToggle").setAttribute("aria-pressed", String(T.tourOn));
    renderTour();
    renderVars();
  }
  let dragging = false;
  tl.addEventListener("mousedown", (e) => { dragging = true; scrubTo(e); });
  tl.addEventListener("mousemove", showTip);
  tl.addEventListener("mouseleave", () => ($("tlTip").hidden = true));
  window.addEventListener("mousemove", (e) => { if (dragging) scrubTo(e); });
  window.addEventListener("mouseup", () => (dragging = false));
  window.addEventListener("resize", drawTimelineBase);
  window.addEventListener("hashchange", fromHash);
  matchMedia("(prefers-color-scheme: dark)").addEventListener?.("change", drawTimelineBase);

  document.addEventListener("keydown", (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.target.closest && e.target.closest("input, textarea, select")) return;
    const k = e.key;
    const map = {
      ArrowRight: e.shiftKey ? nav.over : nav.fwd, j: nav.fwd,
      ArrowLeft: e.shiftKey ? nav.backOver : nav.back, k: nav.back,
      u: nav.out, Home: nav.start, End: nav.end,
      n: () => T.ann && tourGo(T.tourIdx + 1), p: () => T.ann && tourGo(T.tourIdx - 1),
      t: toggleTour,
      "?": () => ($("help").hidden = !$("help").hidden),
      Escape: () => { $("help").hidden = true; $("historyPanel").hidden = true; T.histVar = null; },
    };
    if (/^[1-9]$/.test(k)) { switchTrace(+k - 1); e.preventDefault(); return; }
    const f = map[k];
    if (f) { f(); e.preventDefault(); }
  });

  // ------------------------------------------------------------------ boot
  renderTabs();
  renderMeta();
  requestAnimationFrame(() => {
    drawTimelineBase();
    if (!fromHash()) {
      if (T.cmp) cmpGo(T.cmp.div ? Math.max(0, T.cmp.div.at - 1) : 0); // just before the divergence
      else {
        if (T.ann && (T.ann.tour || []).length) tourGo(-1);
        go(T.ann && (T.ann.tour || []).length ? T.ann.tour[0].step - 1 : 0, { fromTour: true });
      }
    }
  });
})();
