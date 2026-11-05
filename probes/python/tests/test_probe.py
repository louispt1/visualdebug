"""Run: python -m unittest discover -s probes/python/tests  (any Python >= 3.11; set PYTHON to pick one)."""
import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest

PROBE_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PYTHON = os.environ.get("PYTHON", sys.executable)


def trace(code, start_at=None, line_files=None, max_events=1000, files=None, env=None, backend=None):
    with tempfile.TemporaryDirectory() as d:
        root = os.path.join(d, "proj")
        out = os.path.join(root, ".visualdebug", "run")
        os.makedirs(out)
        for name, body in (files or {}).items():
            path = os.path.join(d, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as f:
                f.write(textwrap.dedent(body))
        with open(os.path.join(root, "app.py"), "w") as f:
            f.write(textwrap.dedent(code))
        cfg = {"root": root, "out_dir": out, "start_at": start_at, "line_files": line_files, "max_events": max_events}
        with open(os.path.join(out, "config.json"), "w") as f:
            json.dump(cfg, f)
        e = dict(os.environ, VISUALDEBUG_CONFIG=os.path.join(out, "config.json"), PYTHONPATH=PROBE_DIR)
        e.update(env or {})
        if backend:
            e["VISUALDEBUG_PY_BACKEND"] = backend
        r = subprocess.run([PYTHON, os.path.join(root, "app.py")], cwd=root, env=e, capture_output=True, text=True)
        events = []
        for name in os.listdir(out):
            if name.startswith("trace-"):
                with open(os.path.join(out, name)) as f:
                    events += [json.loads(l) for l in f]
        errs = ""
        if os.path.exists(os.path.join(out, "probe-errors.log")):
            errs = open(os.path.join(out, "probe-errors.log")).read()
        return [e for e in events if e["event"] != "meta"], r, errs, events


class Both:
    """Runs every test against both backends where they exist."""

    backend = None

    def trace(self, *a, **kw):
        kw.setdefault("backend", self.backend)
        return trace(*a, **kw)

    def test_behaviour_unchanged(self):
        ev, r, errs, _ = self.trace("def f(x):\n    return x * 2\nprint(f(21))\nimport sys\nsys.exit(3)\n")
        self.assertEqual(r.stdout, "42\n")
        self.assertEqual(r.returncode, 3)
        self.assertEqual(errs, "")
        self.assertTrue(any(e["event"] == "call" and e["function"] == "app.f" for e in ev))

    def test_exceptions_propagate_and_are_recorded(self):
        ev, r, _, _ = self.trace("def boom():\n    raise KeyError('k')\ntry:\n    boom()\nexcept KeyError as e:\n    print('caught')\n")
        self.assertEqual(r.stdout, "caught\n")
        self.assertTrue(any(e["event"] == "raise" and e["exception"]["type"] == "KeyError" for e in ev))

    def test_redaction_by_name_and_identity(self):
        ev, _, _, _ = self.trace("def login(password):\n    pw = password\n    return pw\nx = login('hunter2')\nd = {'api_key': 'kkk', 'n': 1}\nprint(len(d))\n")
        dump = json.dumps(ev)
        self.assertNotIn("hunter2", dump)
        self.assertNotIn("kkk", dump)

    def test_library_repr_never_called(self):
        files = {"lib/thing.py": """
            class Lazy:
                def __init__(self):
                    self.n = 1
                    self._internal = 2
                def __repr__(self):
                    open(__import__('os').environ['MARK'], 'w').write('called')
                    return 'x'
        """}
        with tempfile.TemporaryDirectory() as d:
            mark = os.path.join(d, "mark")
            ev, r, errs, _ = trace("import sys, os\nsys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'lib'))\nimport thing\nobj = thing.Lazy()\nprint(1)\n",
                                   files=files, env={"MARK": mark}, backend=self.backend)
            self.assertEqual(r.stdout, "1\n", r.stderr)
            self.assertFalse(os.path.exists(mark), "library __repr__ must not run")
            last = [e for e in ev if e["event"] == "line"][-1]
            self.assertEqual(last["locals"]["obj"]["repr"], "<thing.Lazy n=1>")

    def test_limits_and_truncation(self):
        ev, _, _, allev = self.trace("big = list(range(50))\ns = 'x' * 500\nprint(1)\n")
        loc = [e for e in ev if e["event"] == "line"][-1]["locals"]
        self.assertTrue(loc["big"]["truncated"])
        self.assertIn("40 more", loc["big"]["repr"])
        self.assertLessEqual(len(loc["s"]["repr"]), 205)
        ev, _, _, allev = self.trace("i = 0\nwhile i < 1000:\n    i += 1\n", max_events=40)
        self.assertTrue(any(e.get("kind") == "truncated" for e in allev))
        self.assertEqual(len(ev), 40)

    def test_generators_keep_frame_identity(self):
        ev, r, _, _ = self.trace("def gen():\n    yield 1\n    yield 2\nprint(list(gen()))\n")
        self.assertEqual(r.stdout, "[1, 2]\n")
        calls = [e for e in ev if e["event"] == "call" and e["function"] == "app.gen"]
        self.assertGreaterEqual(len(calls), 3)
        self.assertEqual(len({c["frame_id"] for c in calls}), 1)

    def test_start_at_and_auto_test(self):
        code = "def helper(x):\n    return x + 1\ndef test_one():\n    assert helper(1) == 2\ndef other():\n    helper(5)\nother()\ntest_one()\n"
        ev, _, _, _ = self.trace(code, start_at="helper")
        self.assertEqual([e["function"] for e in ev if e["event"] == "call"], ["app.helper", "app.helper"])
        self.assertEqual({e["depth"] for e in ev if e["event"] == "call"}, {1})


class MonitoringTests(Both, unittest.TestCase):
    backend = None


class SettraceTests(Both, unittest.TestCase):
    backend = "settrace"


class SitecustomizeTests(unittest.TestCase):
    def test_chains_existing_sitecustomize(self):
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "sitecustomize.py"), "w") as f:
                f.write("import os\nos.environ['USER_SITE_RAN'] = '1'\n")
            _, r, _, _ = trace("import os\nprint(os.environ.get('USER_SITE_RAN'))\n", env={"PYTHONPATH": PROBE_DIR + os.pathsep + d})
            self.assertEqual(r.stdout, "1\n", r.stderr)


HAVE_DJANGO = subprocess.run([PYTHON, "-c", "import django"], capture_output=True).returncode == 0


@unittest.skipUnless(HAVE_DJANGO, "django not installed")
class DjangoTests(unittest.TestCase):
    def test_queryset_not_evaluated_and_sql_captured(self):
        code = """
            import django
            from django.conf import settings
            settings.configure(INSTALLED_APPS=["django.contrib.contenttypes"], DATABASES={"default": {"ENGINE": "django.db.backends.sqlite3", "NAME": ":memory:"}})
            django.setup()
            from django.db import connection
            from django.contrib.contenttypes.models import ContentType
            with connection.schema_editor() as e:
                e.create_model(ContentType)
            def run():
                qs = ContentType.objects.filter(app_label="x")
                before = len(connection.queries_log)
                ContentType.objects.create(app_label="x", model="y")
                n = qs.count()
                return n
            print(run())
        """
        ev, r, errs, _ = trace(code, start_at="run")
        self.assertEqual(r.stdout.strip(), "1", r.stderr)
        qs = [e for e in ev if e["event"] == "line" and "qs" in e.get("locals", {})]
        self.assertTrue(qs and all("unloaded" in e["locals"]["qs"]["repr"] for e in qs), [e["locals"]["qs"] for e in qs])
        sql = [e["side_effect"]["sql"] for e in ev if e["event"] == "side_effect"]
        self.assertTrue(any(s.startswith("INSERT") for s in sql), sql)
        self.assertTrue(any("COUNT" in s for s in sql), sql)


if __name__ == "__main__":
    unittest.main()
