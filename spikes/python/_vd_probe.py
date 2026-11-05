# Phase 0 spike — throwaway. Loaded by sitecustomize.py (probe dir prepended to PYTHONPATH).
# Backends: sys.monitoring (3.12+), sys.settrace (3.11).
import atexit, itertools, json, os, re, sys, threading, time, traceback

_cfg = json.load(open(os.environ["VISUALDEBUG_CONFIG"]))
ROOT = _cfg["root"].rstrip("/") + "/"
OUT = _cfg["out_dir"]
FOCUS = _cfg.get("focus")  # None => all project files line-level
MAX_EVENTS = _cfg.get("max_events", 20000)
START_AT = _cfg.get("start_at")  # "Pricer.total" or "pricing.Pricer.total"
REDACT = re.compile(_cfg.get("redact") or "password|passwd|secret|token|api_key|apikey|authorization|cookie|session|private_key|credential", re.I)
MAX_REPR, MAX_ITEMS, MAX_DEPTH = 200, 10, 3
PROBE_DIR = os.path.dirname(os.path.abspath(__file__)) + "/"
SCALARS = (type(None), bool, int, float, complex)

_seq = itertools.count(1)
_frame_ids = itertools.count(1)
_count = 0
_buf = []
_io = None
_lock = threading.Lock()
_tls = threading.local()
_window_thread = None
_project_cache = {}


def _log_error(where):
    try:
        with open(os.path.join(OUT, "probe-errors.log"), "a") as f:
            f.write(f"{os.getpid()} {where}: {traceback.format_exc(limit=3)}\n")
    except Exception:
        pass


def project(path):
    r = _project_cache.get(path)
    if r is None:
        p = os.path.abspath(path) if path and not path.startswith("<") else ""
        r = (p.startswith(ROOT) and not p.startswith(PROBE_DIR)
             and "/site-packages/" not in p and "/.venv/" not in p and "/node_modules/" not in p)
        _project_cache[path] = r
    return r


def rel(path):
    return os.path.abspath(path)[len(ROOT):]


def focus_line(r, line):
    if FOCUS is None:
        return True
    return any(a <= line <= b for a, b in FOCUS.get(r, ()))


def emit(ev):
    global _count, _io
    if _count >= MAX_EVENTS:
        return
    _count += 1
    ev["seq"] = next(_seq)
    ev["t"] = time.monotonic_ns()
    ev["pid"] = os.getpid()
    ev["tid"] = threading.get_native_id()
    _buf.append(ev)
    if len(_buf) >= 500:
        flush()


def flush():
    global _io
    with _lock:
        if not _buf:
            return
        if _io is None:
            _io = open(os.path.join(OUT, f"trace-{os.getpid()}.jsonl"), "a")
        for e in _buf:
            _io.write(json.dumps(e, default=str) + "\n")
        _io.flush()
        _buf.clear()


def _after_fork():
    global _io, _count
    _io = None
    _buf.clear()


os.register_at_fork(after_in_child=_after_fork)
atexit.register(lambda: flush())


# ---- serialization: never call third-party code ----
_stdlib_prefix = os.path.dirname(os.__file__) + "/"
_type_policy = {}


def _type_name(t):
    m = t.__module__
    return t.__qualname__ if m == "builtins" else f"{m}.{t.__qualname__}"


def _repr_allowed(t):
    """Call the type's own __repr__ only for builtins, stdlib and project types."""
    r = _type_policy.get(t)
    if r is None:
        mod = sys.modules.get(t.__module__)
        f = getattr(mod, "__file__", None) or ""
        r = t.__module__ == "builtins" or (f.startswith(_stdlib_prefix) and "site-packages" not in f) or project(f)
        _type_policy[t] = r
    return r


def _cut(s):
    return (s[:MAX_REPR] + "…", True) if len(s) > MAX_REPR else (s, False)


def _repr(v, depth):
    t = type(v)
    if t in SCALARS:
        return repr(v)
    if t is str or t is bytes:
        return repr(v[:MAX_REPR + 1])
    if t in (list, tuple, set, frozenset):
        if depth >= MAX_DEPTH:
            return "[…]"
        items = [_repr(x, depth + 1) for x in itertools.islice(v, MAX_ITEMS)]
        if len(v) > MAX_ITEMS:
            items.append(f"…({len(v) - MAX_ITEMS} more)")
        o, c = {list: "[]", tuple: "()", set: "{}", frozenset: "{}"}[t]
        return o + ", ".join(items) + c
    if t is dict:
        if depth >= MAX_DEPTH:
            return "{…}"
        items = [f"{k!r}: " + ("[REDACTED]" if isinstance(k, str) and REDACT.search(k) else _repr(x, depth + 1))
                 for k, x in itertools.islice(v.items(), MAX_ITEMS)]
        if len(v) > MAX_ITEMS:
            items.append("…")
        return "{" + ", ".join(items) + "}"
    if t.__module__ == "django.db.models.query" and t.__name__ == "QuerySet" or any(
            b.__name__ == "QuerySet" and b.__module__ == "django.db.models.query" for b in t.__mro__):
        rc = object.__getattribute__(v, "_result_cache")
        model = object.__getattribute__(v, "model").__name__
        return f"<QuerySet {model} unloaded>" if rc is None else f"<QuerySet {model} loaded ({len(rc)})>"
    if _repr_allowed(t) and t.__repr__ is not object.__repr__:
        return repr(v)
    return _attrs(v, t, depth)


def _attrs(v, t, depth):
    try:
        d = object.__getattribute__(v, "__dict__")
    except Exception:
        return f"<{_type_name(t)}>"
    if depth >= MAX_DEPTH or not d:
        return f"<{_type_name(t)}>"
    body = ", ".join(f"{k}=" + ("[REDACTED]" if REDACT.search(k) else _repr(x, depth + 1))
                     for k, x in itertools.islice(d.items(), MAX_ITEMS))
    return f"<{_type_name(t)} {body}>"


def ser(v):
    t = type(v)
    try:
        s, trunc = _cut(_repr(v, 0))
    except Exception:
        s, trunc = f"<{t.__qualname__} (repr failed)>", False
    out = {"type": _type_name(t), "repr": s, "truncated": trunc}
    if t not in SCALARS and t is not str:
        out["id"] = id(v)
    return out


def snapshot(frame):
    out = {}
    for k, v in list(frame.f_locals.items()):
        if REDACT.search(k):
            out[k] = {"type": "redacted", "repr": "[REDACTED]", "truncated": False}
        else:
            out[k] = ser(v)
    return out


# ---- shared window/stack logic ----
def _stack():
    s = getattr(_tls, "stack", None)
    if s is None:
        s = _tls.stack = []
    return s


def _matches_start(code, frame):
    q = code.co_qualname if hasattr(code, "co_qualname") else code.co_name
    if START_AT == q:
        return True
    return START_AT == f"{frame.f_globals.get('__name__')}.{q}"


def on_start(code, frame, kind="call"):
    st = _stack()
    fid = next(_frame_ids)
    name = f"{frame.f_globals.get('__name__')}.{getattr(code, 'co_qualname', code.co_name)}"
    st.append((fid, name, code))
    r = rel(code.co_filename)
    ev = {"event": kind, "file": r, "line": frame.f_lineno, "function": name, "frame_id": fid, "depth": len(st)}
    if focus_line(r, frame.f_lineno):
        ev["locals"] = snapshot(frame)
    emit(ev)


def on_return(code, line, retval, unwind=False):
    """Returns True if the window just closed."""
    st = _stack()
    for i in range(len(st) - 1, -1, -1):
        if st[i][2] is code:
            fid, name, _ = st[i]
            del st[i:]
            ev = {"event": "return", "file": rel(code.co_filename), "line": line, "function": name,
                  "frame_id": fid, "depth": len(st) + 1}
            if unwind:
                ev["unwound"] = True
            else:
                ev["return_value"] = ser(retval)
            emit(ev)
            return START_AT is not None and not st
    return False


def on_line(code, frame, line):
    st = _stack()
    if not st or st[-1][2] is not code:
        return
    r = rel(code.co_filename)
    if focus_line(r, line):
        fid, name, _ = st[-1]
        emit({"event": "line", "file": r, "line": line, "function": name, "frame_id": fid, "depth": len(st),
              "locals": snapshot(frame)})


def on_raise(code, line, exc):
    st = _stack()
    if not st:
        return
    emit({"event": "raise", "file": rel(code.co_filename), "line": line, "frame_id": st[-1][0], "depth": len(st),
          "exception": {"type": _type_name(type(exc)), "message": _cut(str(exc))[0]}})


def on_lib_call(callee_name):
    st = _stack()
    if st:
        emit({"event": "call", "library": True, "function": callee_name, "frame_id": st[-1][0], "depth": len(st) + 1})


def _callable_name(c):
    try:
        mod = getattr(c, "__module__", None) or type(c).__module__
        q = getattr(c, "__qualname__", None) or type(c).__qualname__
        return f"{mod}.{q}" if mod and mod != "builtins" else q
    except Exception:
        return "?"


def _callable_is_project(c):
    code = getattr(c, "__code__", None) or getattr(getattr(c, "__func__", None), "__code__", None)
    if code is not None:
        return project(code.co_filename)
    if isinstance(c, type):  # class: constructor of a project class runs project __init__ (PY_START covers it)
        return _repr_allowed(c) and c.__module__ != "builtins" and not (sys.modules.get(c.__module__) and (getattr(sys.modules[c.__module__], "__file__", "") or "").startswith(_stdlib_prefix))
    return False


# ---- backend A: sys.monitoring (3.12+) ----
def install_monitoring():
    global _window_thread
    mon = sys.monitoring
    E = mon.events
    TOOL = next(i for i in (3, 4, 2) if mon.get_tool(i) is None)
    mon.use_tool_id(TOOL, "visualdebug")
    DISABLE = mon.DISABLE
    window = {"on": START_AT is None}
    WINDOW_EVENTS = E.PY_START | E.PY_RETURN | E.PY_UNWIND | E.LINE | E.RAISE | E.CALL

    def in_window():
        return window["on"] and (_window_thread is None or _window_thread == threading.get_ident())

    def py_start(code, offset):
        global _window_thread
        try:
            if not window["on"]:
                frame = sys._getframe(1)
                if not _matches_start(code, frame):
                    return DISABLE
                window["on"] = True
                _window_thread = threading.get_ident()
                mon.set_events(TOOL, WINDOW_EVENTS)
                mon.restart_events()  # re-arm locations DISABLEd before the window
                on_start(code, frame)
                return
            if not project(code.co_filename):
                return DISABLE
            if in_window():
                on_start(code, sys._getframe(1))
        except Exception:
            _log_error("py_start")

    def py_return(code, offset, retval):
        try:
            if not project(code.co_filename):
                return DISABLE
            if in_window() and on_return(code, sys._getframe(1).f_lineno, retval):
                close_window()
        except Exception:
            _log_error("py_return")

    def py_unwind(code, offset, exc):
        try:
            if project(code.co_filename) and in_window() and on_return(code, sys._getframe(1).f_lineno, None, unwind=True):
                close_window()
        except Exception:
            _log_error("py_unwind")

    def line(code, line_no):
        try:
            if not project(code.co_filename):
                return DISABLE
            if in_window():
                on_line(code, sys._getframe(1), line_no)
        except Exception:
            _log_error("line")

    def raise_(code, offset, exc):
        try:
            if project(code.co_filename) and in_window():
                on_raise(code, sys._getframe(1).f_lineno, exc)
        except Exception:
            _log_error("raise")

    def call(code, offset, callable_, arg0):
        try:
            if not project(code.co_filename):
                return DISABLE
            if in_window() and not _callable_is_project(callable_):
                on_lib_call(_callable_name(callable_))
        except Exception:
            _log_error("call")

    def close_window():
        window["on"] = False
        mon.set_events(TOOL, 0)
        flush()

    mon.register_callback(TOOL, E.PY_START, py_start)
    mon.register_callback(TOOL, E.PY_RETURN, py_return)
    mon.register_callback(TOOL, E.PY_UNWIND, py_unwind)
    mon.register_callback(TOOL, E.LINE, line)
    mon.register_callback(TOOL, E.RAISE, raise_)
    mon.register_callback(TOOL, E.CALL, call)
    mon.set_events(TOOL, E.PY_START if START_AT else WINDOW_EVENTS)


# ---- backend B: sys.settrace (3.11) ----
def install_settrace():
    global _window_thread
    window = {"on": START_AT is None}

    def local(frame, event, arg):
        try:
            code = frame.f_code
            if event == "line":
                on_line(code, frame, frame.f_lineno)
            elif event == "return":
                # settrace can't tell unwind from return(None); 3.11 sets arg=None on unwind.
                if on_return(code, frame.f_lineno, arg):
                    window["on"] = False
                    sys.settrace(None)
                    flush()
            elif event == "exception":
                on_raise(code, frame.f_lineno, arg[1])
        except Exception:
            _log_error("settrace.local")
        return local

    def lib_local(frame, event, arg):
        return lib_local

    def global_(frame, event, arg):
        global _window_thread
        try:
            code = frame.f_code
            if not window["on"]:
                if not _matches_start(code, frame):
                    return None
                window["on"] = True
                _window_thread = threading.get_ident()
            elif _window_thread is not None and _window_thread != threading.get_ident():
                return None
            if project(code.co_filename):
                on_start(code, frame)
                if not focus_line(rel(code.co_filename), frame.f_lineno) and FOCUS is not None and rel(code.co_filename) not in FOCUS:
                    frame.f_trace_lines = False
                return local
            back = frame.f_back
            if back is not None and project(back.f_code.co_filename) and _stack():
                on_lib_call(f"{frame.f_globals.get('__name__')}.{getattr(code, 'co_qualname', code.co_name)}")
            return None
        except Exception:
            _log_error("settrace.global")
            return None

    sys.settrace(global_)
    threading.settrace(global_)


def install():
    os.makedirs(OUT, exist_ok=True)
    if sys.version_info >= (3, 12) and os.environ.get("VISUALDEBUG_BACKEND") != "settrace":
        install_monitoring()
    else:
        install_settrace()
