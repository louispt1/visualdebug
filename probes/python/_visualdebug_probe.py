"""visualdebug Python probe. Loaded by sitecustomize.py; configured by the JSON file at
$VISUALDEBUG_CONFIG. Zero dependencies. Backends: sys.monitoring (3.12+), sys.settrace (3.11).

A probe bug must never change the traced program's behaviour: every callback swallows its own
errors (logged to <out_dir>/probe-errors.log) and serialization never calls third-party code.
"""

import atexit
import itertools
import json
import os
import re
import sys
import threading
import time
import traceback
import types

VERSION = 1
_PROBE_FILES = (os.path.abspath(__file__), os.path.join(os.path.dirname(os.path.abspath(__file__)), "sitecustomize.py"))
_SCALARS = (type(None), bool, int, float, complex)
_REDACTED = {"type": "redacted", "repr": "[REDACTED]", "truncated": False}
_BUILTIN_CALLABLES = (types.BuiltinFunctionType, types.BuiltinMethodType, types.MethodWrapperType,
                      types.WrapperDescriptorType, types.MethodDescriptorType, types.ClassMethodDescriptorType)
_STDLIB = os.path.dirname(os.__file__) + os.sep


class Probe:
    def __init__(self, cfg):
        self.root = cfg["root"].rstrip("/") + "/"
        self.out = cfg["out_dir"]
        lf = cfg.get("line_files")
        self.line_files = None if lf is None else set(lf)
        self.start_at = cfg.get("start_at")
        self.max_events = cfg.get("max_events", 20000)
        self.redact = re.compile(cfg.get("redact") or "password|passwd|secret|token|api_key|apikey|authorization|cookie|session|private_key|credential", re.I)
        lim = cfg.get("limits") or {}
        self.max_repr, self.max_items, self.max_depth = lim.get("repr", 200), lim.get("items", 10), lim.get("depth", 3)
        self.seq = itertools.count(1)
        self.fids = itertools.count(1)
        self.count = 0
        self.truncated = False
        self.buf = []
        self.io = None
        self.lock = threading.Lock()
        self.tls = threading.local()
        self.project_cache = {}
        self.type_cache = {}
        self.redacted_ids = {}
        self.window_thread = None  # thread ident owning the open window (None: whole program)
        self.window_on = self.start_at is None
        self.closed = False
        self.target = self._parse_target(self.start_at)
        self.main_file = os.path.abspath(sys.argv[0]) if sys.argv and sys.argv[0] else None
        os.makedirs(self.out, exist_ok=True)
        os.register_at_fork(after_in_child=self._after_fork)
        atexit.register(self.flush)
        self.emit_meta(kind="start", language="python", runtime=sys.version.split()[0], probe_version=VERSION, argv=list(sys.argv))

    # ------------------------------------------------------------ helpers
    def log_error(self, where):
        try:
            with open(os.path.join(self.out, "probe-errors.log"), "a") as f:
                f.write(f"{os.getpid()} python {where}: {traceback.format_exc(limit=4)}\n")
        except Exception:
            pass

    def _after_fork(self):
        self.io = None
        self.buf = []
        self.tls = threading.local()

    def project(self, path):
        r = self.project_cache.get(path)
        if r is None:
            p = os.path.abspath(path) if path and not path.startswith("<") else ""
            r = bool(p) and p.startswith(self.root) and p not in _PROBE_FILES and not any(
                s in p for s in ("/site-packages/", "/dist-packages/", "/.venv/", "/venv/", "/node_modules/", "/.tox/")) and (
                "/.visualdebug/" not in p or "/.visualdebug/examples/" in p)
            self.project_cache[path] = r
        return r

    def rel(self, path):
        return os.path.abspath(path)[len(self.root):]

    def line_level(self, rel):
        return self.line_files is None or rel in self.line_files

    def stack(self):
        st = getattr(self.tls, "stack", None)
        if st is None:
            st = self.tls.stack = []
            self.tls.suspended = {}
        return st

    def fn_name(self, code, frame):
        mod = frame.f_globals.get("__name__", "?")
        if mod == "__main__":
            mod = os.path.splitext(os.path.basename(code.co_filename))[0]
        q = getattr(code, "co_qualname", code.co_name)
        return mod if q == "<module>" else f"{mod}.{q}"

    # ------------------------------------------------------------ windows
    def _parse_target(self, s):
        if not s or s == "auto:test":
            return None
        m = re.match(r"^(.+\.py):(\d+)$", s)
        if m:
            return ("line", os.path.abspath(os.path.join(self.root, m.group(1))), int(m.group(2)))
        return ("name", s)

    _code_lines = {}

    def is_trigger(self, code, frame):
        """Does entering `code` open a window?"""
        if self.start_at == "auto:test":
            name = code.co_name
            base = os.path.basename(code.co_filename)
            return name.startswith("test") and (base.startswith("test") or base.endswith("_test.py")) and self.project(code.co_filename)
        t = self.target
        if t is None:
            return False
        if t[0] == "line":
            if os.path.abspath(code.co_filename) != t[1] or code.co_name == "<module>":
                return False
            lines = self._code_lines.get(code)
            if lines is None:
                lines = self._code_lines[code] = {l for _, _, l in code.co_lines() if l}
            return t[2] in lines
        q = getattr(code, "co_qualname", code.co_name)
        name = t[1]
        return name == q or name == self.fn_name(code, frame) or name.endswith("." + q)

    def open_window(self):
        self.window_on = True
        self.window_thread = threading.get_ident()
        self.emit_meta(kind="window_start")
        self.install_sql_hooks()

    def close_window(self):
        self.window_on = False
        self.window_thread = None
        self.emit_meta(kind="window_end")
        self.flush()

    def in_window(self):
        return self.window_on and not self.closed and (self.window_thread is None or self.window_thread == threading.get_ident())

    # ------------------------------------------------------------ events (backend-independent)
    def recordable(self, code):
        """Functions, plus the main script's top level. Class bodies and other modules' top-level
        code (run at import) are not frames worth reading."""
        if code.co_flags & 0x1:  # CO_OPTIMIZED: a function body
            return True
        return code.co_name == "<module>" and self.main_file is not None and os.path.abspath(code.co_filename) == self.main_file

    def on_start(self, code, frame, resumed=False):
        if not self.recordable(code):
            return
        st = self.stack()
        susp = self.tls.suspended
        fid = susp.pop(id(frame), None) if resumed else None
        if fid is None:
            fid = next(self.fids)
        name = self.fn_name(code, frame)
        rel = self.rel(code.co_filename)
        ll = self.line_level(rel) or not st  # the window's root frame (e.g. a test body) is context
        st.append([fid, name, code, ll, frame])
        ev = {"event": "call", "file": rel, "line": frame.f_lineno or code.co_firstlineno, "function": name, "frame_id": fid, "depth": len(st)}
        if resumed:
            ev["resumed"] = True
        ev["locals"] = self.snapshot(frame, code)
        self.emit(ev)

    def on_return(self, code, frame, value, unwind=False, yielded=False):
        """Returns True when the window's root frame has finished."""
        st = self.stack()
        for i in range(len(st) - 1, -1, -1):
            if st[i][4] is frame:
                fid, name = st[i][0], st[i][1]
                del st[i:]
                ev = {"event": "return", "file": self.rel(code.co_filename), "line": frame.f_lineno, "function": name,
                      "frame_id": fid, "depth": len(st) + 1}
                if unwind:
                    ev["unwound"] = True
                else:
                    ev["return_value"] = self.ser(value)
                if yielded:
                    ev["yielded"] = True
                    self.tls.suspended[id(frame)] = fid
                self.emit(ev)
                return not st and self.start_at is not None and not yielded
        return False

    def on_line(self, code, frame, line):
        st = self.stack()
        if not st or st[-1][4] is not frame or not st[-1][3]:
            return
        fid, name = st[-1][0], st[-1][1]
        self.emit({"event": "line", "file": self.rel(code.co_filename), "line": line, "function": name,
                   "frame_id": fid, "depth": len(st), "locals": self.snapshot(frame, code)})

    def on_raise(self, code, frame, exc):
        st = self.stack()
        if not st or st[-1][4] is not frame:
            return
        self.emit({"event": "raise", "file": self.rel(code.co_filename), "line": frame.f_lineno, "function": st[-1][1],
                   "frame_id": st[-1][0], "depth": len(st),
                   "exception": {"type": self.type_name(type(exc)), "message": self.cut(self.safe_str(exc))}})

    def on_lib_call(self, name, value=None, returned=False):
        st = self.stack()
        if not st:
            return
        ev = {"event": "return" if returned else "call", "library": True, "function": name, "frame_id": st[-1][0], "depth": len(st) + 1}
        if returned:
            ev["return_value"] = self.ser(value)
        self.emit(ev)

    def callable_name(self, c):
        try:
            f = getattr(c, "__func__", c)
            mod = getattr(f, "__module__", None) or type(c).__module__
            q = getattr(f, "__qualname__", None) or type(c).__qualname__
            return q if mod in (None, "builtins") else f"{mod}.{q}"
        except Exception:
            return "?"

    def lib_callable(self, c):
        """Library (non-project, non-builtin) callables are recorded as one collapsed call."""
        if isinstance(c, _BUILTIN_CALLABLES):
            return False
        if (getattr(c, "__module__", None) or "").startswith(("_frozen_importlib", "importlib._bootstrap")):
            return False
        f = getattr(c, "__func__", c)
        code = getattr(f, "__code__", None)
        if code is not None:
            return not self.project(code.co_filename)
        if isinstance(c, type):
            return c.__module__ != "builtins" and not self.type_in_project(c)
        return False

    # ------------------------------------------------------------ output
    def emit(self, ev):
        if self.count >= self.max_events:
            if not self.truncated:
                self.truncated = True
                self.closed = True
                self.emit_meta(kind="truncated")
                self.on_truncate()
                self.flush()
            return
        self.count += 1
        self.stamp(ev)
        self.buf.append(ev)
        if len(self.buf) >= 500:
            self.flush()

    def on_truncate(self):
        pass

    def emit_meta(self, **kw):
        ev = {"event": "meta", **kw}
        self.stamp(ev)
        self.buf.append(ev)

    def stamp(self, ev):
        ev["seq"] = next(self.seq)
        ev["t"] = time.monotonic_ns()
        ev["pid"] = os.getpid()
        ev["tid"] = threading.get_native_id()

    def flush(self):
        try:
            with self.lock:
                if not self.buf:
                    return
                if self.io is None:
                    self.io = open(os.path.join(self.out, f"trace-{os.getpid()}.jsonl"), "a")
                buf, self.buf = self.buf, []
                for e in buf:
                    self.io.write(json.dumps(e, default=str) + "\n")
                self.io.flush()
        except Exception:
            self.log_error("flush")

    # ------------------------------------------------------------ SQL (Django)
    def install_sql_hooks(self):
        if getattr(self, "_sql_done", False) or "django.db" not in sys.modules:
            return
        self._sql_done = True
        try:
            from django.db import connections
            from django.db.backends.signals import connection_created
            probe = self

            def wrapper(execute, sql, params, many, context):
                t0 = time.perf_counter()
                try:
                    return execute(sql, params, many, context)
                finally:
                    try:
                        if probe.in_window() and not getattr(probe.tls, "busy", False):
                            st = probe.stack()
                            probe.emit({"event": "side_effect", "frame_id": st[-1][0] if st else None, "depth": len(st),
                                        "function": st[-1][1] if st else None,
                                        "side_effect": {"kind": "sql", "sql": probe.cut(str(sql)), "name": context.get("connection").alias if context.get("connection") else None,
                                                        "duration_ms": round((time.perf_counter() - t0) * 1000, 3),
                                                        "binds": [probe.ser(p)["repr"] for p in list(params or [])[: probe.max_items]] if not many else []}})
                    except Exception:
                        probe.log_error("sql")

            def add(connection, **kw):
                if wrapper not in connection.execute_wrappers:
                    connection.execute_wrappers.append(wrapper)

            try:
                existing = connections.all(initialized_only=True)  # Django 4.1+; don't open new connections
            except TypeError:
                existing = []
            for c in existing:
                add(c)
            connection_created.connect(add, weak=False)
        except Exception:
            self.log_error("install_sql_hooks")

    # ------------------------------------------------------------ serialization
    def type_name(self, t):
        m = getattr(t, "__module__", "?")
        return t.__qualname__ if m == "builtins" else f"{m}.{t.__qualname__}"

    def type_in_project(self, t):
        mod = sys.modules.get(t.__module__)
        f = getattr(mod, "__file__", None)
        return bool(f) and self.project(f)

    def repr_allowed(self, t):
        """Call a type's __repr__ only if the class *defining* it is builtin, stdlib or project code."""
        r = self.type_cache.get(t)
        if r is None:
            owner = next((k for k in t.__mro__ if "__repr__" in k.__dict__), object)
            if owner is object:
                r = "default"
            else:
                mod = sys.modules.get(owner.__module__)
                f = getattr(mod, "__file__", None) or ""
                std = owner.__module__ == "builtins" or (f.startswith(_STDLIB) and "site-packages" not in f)
                r = "call" if std or (f and self.project(f)) else "attrs"
            self.type_cache[t] = r
        return r

    def cut(self, s):
        return s if len(s) <= self.max_repr else s[: self.max_repr] + "…"

    def safe_str(self, e):
        try:
            return str(e) if type(e).__str__ in (BaseException.__str__,) or self.repr_allowed(type(e)) != "attrs" else type(e).__name__
        except Exception:
            return "?"

    def redact_value(self, v):
        if type(v) not in _SCALARS:
            if len(self.redacted_ids) > 10000:
                self.redacted_ids.clear()
            self.redacted_ids[id(v)] = True
        return _REDACTED

    def ser(self, v):
        t = type(v)
        if t not in _SCALARS and id(v) in self.redacted_ids:
            return _REDACTED
        self._cut_off = False
        try:
            s = self._repr(v, 0)
        except Exception:
            s = f"<{t.__qualname__} (repr failed)>"
        out = {"type": self.type_name(t), "repr": self.cut(s), "truncated": self._cut_off or len(s) > self.max_repr}
        if t not in _SCALARS and t is not str:
            out["id"] = id(v)
        return out

    def _repr(self, v, depth):
        t = type(v)
        if t in _SCALARS:
            return repr(v)
        if t is str or t is bytes:
            if len(v) > self.max_repr:
                self._cut_off = True
            return repr(v[: self.max_repr])
        if t in (list, tuple, set, frozenset) or isinstance(v, (list, tuple)) and self.repr_allowed(t) != "attrs":
            if depth >= self.max_depth:
                self._cut_off = True
                return "[…]"
            items = [self._repr(x, depth + 1) for x in itertools.islice(v, self.max_items)]
            if len(v) > self.max_items:
                self._cut_off = True
                items.append(f"…({len(v) - self.max_items} more)")
            o, c = ("(", ")") if isinstance(v, tuple) else ("{", "}") if isinstance(v, (set, frozenset)) else ("[", "]")
            if isinstance(v, tuple) and len(v) == 1:
                c = ",)"
            return o + ", ".join(items) + c
        if isinstance(v, dict) and (t is dict or self.repr_allowed(t) != "attrs"):
            if depth >= self.max_depth:
                self._cut_off = True
                return "{…}"
            items = []
            for k, x in itertools.islice(v.items(), self.max_items):
                red = isinstance(k, str) and self.redact.search(k)
                items.append(f"{self._repr(k, depth + 1)}: " + ("[REDACTED]" if red else self._repr(x, depth + 1)))
            if len(v) > self.max_items:
                self._cut_off = True
                items.append("…")
            return "{" + ", ".join(items) + "}"
        qs = self._queryset(v, t)
        if qs is not None:
            return qs
        how = self.repr_allowed(t)
        if how == "call":
            return repr(v)
        return self._attrs(v, t, depth)

    def _queryset(self, v, t):
        # Django QuerySet: repr() evaluates the query. Never call it.
        for k in t.__mro__:
            if k.__name__ == "QuerySet" and k.__module__ == "django.db.models.query":
                rc = object.__getattribute__(v, "_result_cache")
                model = object.__getattribute__(v, "model")
                name = getattr(model, "__name__", "?")
                return f"<QuerySet {name} unloaded>" if rc is None else f"<QuerySet {name} loaded ({len(rc)})>"
        return None

    def _attrs(self, v, t, depth):
        name = self.type_name(t)
        try:
            d = object.__getattribute__(v, "__dict__")
        except Exception:
            return f"<{name}>"
        if depth >= self.max_depth or not d:
            return f"<{name}>"
        own = self.type_in_project(t)
        parts = []
        for k, x in d.items():
            if k.startswith("__") or (not own and k.startswith("_")):
                continue  # library internals (e.g. Django's _state and relation caches)
            parts.append(f"{k}=" + ("[REDACTED]" if self.redact.search(k) else self._repr(x, depth + 1)))
            if len(parts) >= self.max_items:
                break
        return f"<{name} {', '.join(parts)}>" if parts else f"<{name}>"

    def snapshot(self, frame, code):
        out = {}
        module_like = code.co_name == "<module>" or (frame.f_locals is frame.f_globals) or "__module__" in frame.f_locals and "__qualname__" in frame.f_locals
        try:
            items = list(frame.f_locals.items())
        except Exception:
            return out
        for k, v in items:
            if k.startswith("@py_assert"):
                continue  # pytest assertion-rewrite temporaries
            if module_like and (k.startswith("__") or isinstance(v, (types.ModuleType, types.FunctionType, type))):
                continue  # module/class bodies: only data, not imports and definitions
            out[k] = self.redact_value(v) if self.redact.search(k) else self.ser(v)
            if len(out) >= 40:
                break
        return out


# ---------------------------------------------------------------- backend: sys.monitoring (3.12+)
def install_monitoring(p):
    mon = sys.monitoring
    E = mon.events
    tool = next((i for i in (3, 4, 2, 5) if mon.get_tool(i) is None), None)
    if tool is None:
        p.log_error("no free sys.monitoring tool id")
        return
    mon.use_tool_id(tool, "visualdebug")
    DISABLE = mon.DISABLE
    WINDOW = E.PY_START | E.PY_RESUME | E.PY_RETURN | E.PY_YIELD | E.PY_UNWIND | E.LINE | E.RAISE | E.CALL
    ARM = E.PY_START

    def guard(fn):
        def cb(*a):
            try:
                return fn(*a)
            except Exception:
                p.log_error(fn.__name__)
        return cb

    @guard
    def py_start(code, offset):
        if p.closed:
            return DISABLE
        if not p.window_on:
            frame = sys._getframe(2)
            if not p.is_trigger(code, frame):
                return DISABLE
            p.open_window()
            mon.set_events(tool, WINDOW)
            mon.restart_events()  # re-arm locations DISABLEd before the window
            p.on_start(code, frame)
            return None
        if not p.project(code.co_filename):
            return DISABLE
        if p.in_window():
            p.on_start(code, sys._getframe(2))

    @guard
    def py_resume(code, offset):
        if not p.project(code.co_filename):
            return DISABLE
        if p.in_window():
            p.on_start(code, sys._getframe(2), resumed=True)

    def make_ret(yielded):
        @guard
        def py_return(code, offset, value):
            if not p.project(code.co_filename):
                return DISABLE
            if p.in_window() and p.on_return(code, sys._getframe(2), value, yielded=yielded):
                close()
        py_return.__name__ = "py_yield" if yielded else "py_return"
        return py_return

    @guard
    def py_unwind(code, offset, exc):
        if p.project(code.co_filename) and p.in_window() and p.on_return(code, sys._getframe(2), None, unwind=True):
            close()

    @guard
    def line(code, line_no):
        if not p.project(code.co_filename):
            return DISABLE
        if p.in_window():
            p.on_line(code, sys._getframe(2), line_no)

    @guard
    def raise_(code, offset, exc):
        if p.project(code.co_filename) and p.in_window():
            p.on_raise(code, sys._getframe(2), exc)

    @guard
    def call(code, offset, callable_, arg0):
        if not p.project(code.co_filename):
            return DISABLE
        if p.in_window() and p.lib_callable(callable_):
            p.on_lib_call(p.callable_name(callable_))

    def close():
        p.close_window()
        mon.set_events(tool, 0 if p.closed else ARM)

    def on_truncate():
        mon.set_events(tool, 0)

    p.on_truncate = on_truncate
    for ev, cb in ((E.PY_START, py_start), (E.PY_RESUME, py_resume), (E.PY_RETURN, make_ret(False)), (E.PY_YIELD, make_ret(True)),
                   (E.PY_UNWIND, py_unwind), (E.LINE, line), (E.RAISE, raise_), (E.CALL, call)):
        mon.register_callback(tool, ev, cb)
    mon.set_events(tool, WINDOW if p.window_on else ARM)


# ---------------------------------------------------------------- backend: sys.settrace (3.11)
def install_settrace(p):
    def local(frame, event, arg):
        try:
            code = frame.f_code
            if event == "line":
                p.on_line(code, frame, frame.f_lineno)
            elif event == "return":
                # arg is None both for `return None` and for unwinding; settrace can't tell them apart.
                if p.on_return(code, frame, arg, yielded=_yielded(frame)):
                    p.close_window()
            elif event == "exception":
                exc = arg[1]
                if getattr(p.tls, "last_exc", None) is not exc:
                    p.tls.last_exc = exc
                    p.on_raise(code, frame, exc)
        except Exception:
            p.log_error("settrace.local")
        return local

    def lib_return(frame, event, arg):
        if event == "return":
            try:
                p.on_lib_call(p.fn_name(frame.f_code, frame), arg, returned=True)
            except Exception:
                p.log_error("settrace.lib_return")
        return None if event == "return" else lib_return

    def global_(frame, event, arg):
        if event != "call" or p.closed:
            return None
        try:
            code = frame.f_code
            if not p.window_on:
                if not p.is_trigger(code, frame):
                    return None
                p.open_window()
            elif not p.in_window():
                return None
            if p.project(code.co_filename):
                p.stack()  # ensures tls.suspended exists
                p.on_start(code, frame, resumed=id(frame) in p.tls.suspended)
                if not p.stack()[-1][3]:
                    frame.f_trace_lines = False
                return local
            st = p.stack()
            back = frame.f_back
            if st and back is st[-1][4] and not code.co_filename.startswith("<") and not frame.f_globals.get("__name__", "").startswith(("_frozen_importlib", "importlib._bootstrap")):
                p.on_lib_call(p.fn_name(code, frame))
                frame.f_trace_lines = False
                return lib_return
        except Exception:
            p.log_error("settrace.global")
        return None

    def on_truncate():
        sys.settrace(None)
        threading.settrace(None)

    p.on_truncate = on_truncate
    sys.settrace(global_)
    threading.settrace(global_)


_YIELD_OPS = None


def _yielded(frame):
    """settrace reports a generator/coroutine suspension as 'return': detect it by the opcode."""
    global _YIELD_OPS
    if not frame.f_code.co_flags & 0x2A0:  # CO_GENERATOR | CO_COROUTINE | CO_ASYNC_GENERATOR
        return False
    try:
        if _YIELD_OPS is None:
            import dis
            _YIELD_OPS = (dis.opmap.get("YIELD_VALUE"), dis.opmap.get("RESUME"))
        co, i = frame.f_code.co_code, frame.f_lasti
        # 3.11: stopped on YIELD_VALUE; 3.12+: on the RESUME after it (oparg != 0 = after a yield/await)
        return co[i] == _YIELD_OPS[0] or (co[i] == _YIELD_OPS[1] and co[i + 1] != 0)
    except Exception:
        return False


def install():
    path = os.environ.get("VISUALDEBUG_CONFIG")
    if not path or getattr(sys, "_visualdebug_installed", False):
        return
    sys._visualdebug_installed = True
    with open(path) as f:
        cfg = json.load(f)
    p = Probe(cfg)
    if sys.version_info >= (3, 12) and os.environ.get("VISUALDEBUG_PY_BACKEND") != "settrace":
        install_monitoring(p)
    else:
        install_settrace(p)
    return p
