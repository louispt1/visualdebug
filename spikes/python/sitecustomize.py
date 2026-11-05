# Phase 0 spike. Installs the probe, then chains to any pre-existing sitecustomize further down sys.path.
import os, sys

_here = os.path.dirname(os.path.abspath(__file__))
try:
    import _vd_probe
    _vd_probe.install()
except Exception:
    import traceback
    traceback.print_exc()

_saved = sys.path[:]
try:
    sys.path[:] = [p for p in sys.path if os.path.abspath(p or ".") != _here]
    _me = sys.modules.pop("sitecustomize", None)
    try:
        import sitecustomize  # noqa: F401  — the user's own, if any
    except ImportError:
        pass
    finally:
        sys.modules["sitecustomize"] = sys.modules.get("sitecustomize") or _me
finally:
    sys.path[:] = _saved
