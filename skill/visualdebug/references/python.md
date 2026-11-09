# visualdebug: Python

Injected with a `sitecustomize.py` on `PYTHONPATH` (an existing `sitecustomize` still runs); no
requirements changes. Python 3.11+ (3.12+ uses `sys.monitoring`: zero cost outside the recording
window; 3.11 uses `sys.settrace`). Use the project's interpreter: the venv's `python`/`pytest`, or
`uv run`, `poetry run`.

## Commands
```
visualdebug run --focus-diff main -- pytest tests/test_pricing.py::test_bulk_discount
visualdebug run --focus-diff main --start-at pricing.apply_discount -- pytest tests/test_pricing.py -q
visualdebug run --focus-diff main -- python -m pytest -q -k bulk
visualdebug run -- python scripts/report.py --dry-run
visualdebug run --focus-diff main --start-at "orders.views.checkout" -- python manage.py test orders.tests.test_checkout
```

## `--start-at` forms
- `apply_discount`, `Pricer.total`, `pricing.Pricer.total` (module-qualified), or
  `pricing.py:35` (the function containing that line).
- Without it, pytest runs record **each test function body** (fixtures excluded); scripts record the
  whole program (the main script's top level + every project function).

## Reading Python traces
- Function names are module-qualified: `pricing.Pricer.total`.
- Library calls appear once as `→ json.dumps (library)`; builtins (`len`, `dict.get`) are omitted.
  On 3.12+ library return values are not available; the effect shows in the next step's variables.
- Generators/coroutines keep one frame across `yield`/`await` (a `yielded` return then a resumed call).
- Django: QuerySets show `unloaded` / `loaded (n)` and are never evaluated by the probe; model
  instances are shown from their fields (their `__str__` isn't called). SQL appears as `SQL` rows.

## Examples you write
`.visualdebug/examples/test_guest_discount.py` run with `pytest .visualdebug/examples/test_guest_discount.py`
(add `--rootdir .` / `-p no:cacheprovider` if the project's config interferes). Reuse the project's
fixtures by placing the file where `conftest.py` applies, or import factories directly. Mock network.

## Gotchas
- pytest-xdist: each worker is traced separately and merged; pass `-n 0` for a linear story.
- `python -I`/`-E` ignore `PYTHONPATH`, so the probe won't load. Don't use them for traced runs.
- coverage on 3.11 also uses `sys.settrace`: run without `--cov` when tracing.
