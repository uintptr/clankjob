## Prefer the standard library

Scripts here get copied onto random machines — a laptop, a build box, a
container — and run directly. Every third-party import turns that into a
dependency install: a venv to create and remember to activate, a `uv`
that has to be present, a network fetch at first run, and a script that
fails at import time on a host that has none of it. Stdlib-only scripts
just run.

So: **when a task can be done with the standard library at comparable
effort, use the standard library.** Reach for a dependency only when it
buys something the stdlib genuinely does not have.

Common substitutions worth making:

| Instead of          | Use                                                 |
| ------------------- | --------------------------------------------------- |
| `requests`          | `urllib.request` (+ `urllib.error`, `urllib.parse`) |
| `python-dateutil`   | `datetime`, `datetime.fromisoformat`                |
| `PyYAML` for config | `json` / `tomllib` (3.11+)                          |
| `sh`, `plumbum`     | `subprocess`                                        |
| `tabulate`          | f-string width specifiers (`f"{k:<20}"`)            |
| `colorama`          | ANSI escapes in a couple of constants               |

```python
# Good — stdlib only, so the plain shebang works and there is no venv
#!/usr/bin/env python3
import json
import urllib.request

request = urllib.request.Request(url, data=body, headers=headers, method="POST")

with urllib.request.urlopen(request, timeout=10) as resp:
    ...

# Bad — pulls in a venv to save a few lines
#!/usr/bin/env -S uv run --script
# /// script
# dependencies = ["requests"]
# ///
import requests

resp = requests.post(url, data=body, headers=headers, timeout=10)
```

Two API differences to keep in mind when replacing `requests` with
`urllib`, both of which need handling explicitly:

- `urlopen` **raises** `HTTPError` for status >= 400 rather than
  returning a response. `HTTPError` is itself readable, so the body is
  still available via `e.read()`. A non-200 2xx comes back as a normal
  response, so a strict status check needs both paths.
- Failures are `OSError` subclasses (`URLError`, `TimeoutError`, socket
  errors), not a single `RequestException` — catch `OSError`. And
  `urlopen` takes one `timeout` covering connect and each read, not
  requests' `(connect, read)` pair.

A third-party dependency is still the right call when the stdlib
equivalent would mean reimplementing something substantial or
error-prone — an HTML or TOML _writer_, an async HTTP client with
connection pooling, a cloud provider's signed API, dataframes,
scientific computing. In that case use the `uv run --script` shebang
below and pin the dependency in the inline table. The rule is to stop
reaching for a library out of habit, not to hand-roll a protocol
implementation.

## Python script shebangs

Once the dependency question above is settled: scripts that import
third-party dependencies (e.g., `aiohttp`, `google-cloud-storage`) must
use the `uv run --script` shebang with an inline dependency table.
Scripts that only use the standard library and local project imports use
the plain `python3` shebang.

```python
# Good — script uses aiohttp (third-party)
#!/usr/bin/env -S uv run --script
# /// script
# dependencies = ["aiohttp"]
# ///

# Good — script only uses stdlib + local imports
#!/usr/bin/env python3
```

## Python argparse formatting

When calling `parser.add_argument`, put each keyword argument on its own
line, aligned under the first argument. Do not use implicit string
concatenation (keep long `help` text on a single line even if it exceeds
the line-length limit — autopep8 will not, and must not, split it).

```python
# Good
parser.add_argument("-d",
                    action="store_true",
                    dest="sign_only",
                    help="Only produce the .p7s detached signature")

# Bad
parser.add_argument("-d", action="store_true", dest="sign_only",
                    help="Only produce the .p7s detached signature")
```

## Type hints

Annotate every function signature in new Python code — all parameters and
the return type. Prefer `Optional[T]` (or `T | None`) for nullable values,
and avoid `Any` unless it is genuinely unavoidable. This applies to
standalone scripts and helpers too, not just the PyO3 binding code covered
in `rust/CLAUDE.md`.

```python
# Good
def run_command(binary: str, args: list[str], expect_success: bool = True) -> subprocess.CompletedProcess:
    ...

# Bad
def run_command(binary, args, expect_success=True):
    ...
```

## Formatting with autopep8

After writing or modifying a Python file, run it through `autopep8` before
committing so the layout is normalized:

```
autopep8 --in-place <file>
```

The line length comes from `pyproject.toml` (`[tool.autopep8]`), so the
plain command above is enough — no need to pass `--max-line-length`. A
re-run (`autopep8 --diff <file>`) should then show no further changes.
autopep8 will not split string literals, so long `help=` text and similar
strings can exceed the limit; that is expected and acceptable.

## Linting with ruff

After writing or modifying a Python file, run ruff on it and fix every
warning it reports before committing. ruff is not installed globally —
run it through uv:

```
uvx ruff check <file>
```

The lint configuration lives in `pyproject.toml` (`[tool.ruff.lint]`)
and ruff picks it up automatically from the file's location — no extra
flags needed.

New code must produce **zero** ruff warnings. When touching an existing
file, do not introduce any new warning; pre-existing warnings in
untouched lines may be left alone (or fixed while you're there), but do
not open standalone lint-cleanup patches across the tree.

Note that ruff's E712 rejects the explicit `True ==` / `False ==`
comparison style found in older scripts here (a carry-over from the C
rules in `CLAUDE_C.md`). That C rule does not apply to Python — new
Python code uses plain truthiness, which is what ruff enforces:

```python
# Good — plain truthiness, no ruff warning
if not os.path.exists(path):
    ...

# Bad — E712, C-style boolean comparison carried into Python
if False == os.path.exists(path):
    ...
```

## Type checking with basedpyright

After writing or modifying a Python file, type-check it with
basedpyright and fix every **error** it reports before committing.
ruff and basedpyright are complementary, not redundant: ruff lints
per-file patterns, basedpyright does whole-program type inference
(`Optional` narrowing, argument/return assignability, possibly-unbound
variables) that ruff cannot see. Like ruff, run it through uv:

```
uvx basedpyright <file>
```

When the file imports third-party dependencies (from its PEP 723
inline script table or a requirements.txt), make them visible to the
checker with `--with`, otherwise the imports fail to resolve:

```
uvx --with aiohttp basedpyright test/agent/agent_test.py
```

New and modified code must produce **zero errors**. Warnings are
advisory: basedpyright's strict defaults emit many
`reportUnknown*` warnings against untyped third-party libraries
(e.g. aiohttp) — do not churn existing code to silence those, and
never silence an _error_ with a blanket `# type: ignore`; fix the type
instead. When touching an existing file, do not introduce any new
error; pre-existing errors in untouched lines may be left alone (or
fixed while you're there).

Two recurring gotchas, both found in real code here:

- **`__exit__` / `__aexit__` must return `None` (not `bool`) when the
  context manager never suppresses exceptions.** A `bool` return tells
  the checker exceptions may be swallowed, which makes every variable
  bound inside a `with` body "possibly unbound" after it — at every
  call site.
- **Narrowing of `x.attr` does not survive into `finally` blocks or
  other functions.** If an `Optional` dataclass field has just been
  assigned a real value, bind it to a local
  (`oid = create(...); args.org_id = oid`) and use the local, rather
  than sprinkling `assert` or re-checks at each use.

## Tool order

Run the three tools in the order they appear above: autopep8 first,
then ruff, then basedpyright. autopep8 is the only one that rewrites
the file, so the two read-only checkers must run after it — on the
final formatted code, not before it. The order of ruff vs basedpyright
does not matter. When fixing a ruff or basedpyright finding means
editing the code, re-run autopep8 on the edit and check again until
all three pass in one round:

```
autopep8 --in-place <file> && uvx ruff check <file> && uvx basedpyright <file>
```
