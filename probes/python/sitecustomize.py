# visualdebug: installs the probe, then chains to any sitecustomize further down sys.path.
import os
import sys

_here = os.path.dirname(os.path.abspath(__file__))
try:
    import _visualdebug_probe

    _visualdebug_probe.install()
except Exception as e:  # never break the program
    sys.stderr.write(f"visualdebug: python probe disabled ({type(e).__name__}: {e})\n")

_saved_path = sys.path[:]
_me = sys.modules.pop("sitecustomize", None)
try:
    sys.path[:] = [p for p in sys.path if os.path.abspath(p or ".") != _here]
    try:
        import sitecustomize  # noqa: F401  (the user's own, if any)
    except ImportError:
        pass
finally:
    sys.path[:] = _saved_path
    if "sitecustomize" not in sys.modules and _me is not None:
        sys.modules["sitecustomize"] = _me
