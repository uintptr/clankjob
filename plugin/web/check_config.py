#!/usr/bin/env python3
"""Check the web plugin: reading a page and a feed, refusing this server's own addresses,
and, when configured, a Google search.

Reads config.toml (optional) the way the server does (`{ env = ... }` and
`{ secret = ... }` references included). Read-only: it fetches two public pages and runs
one search, which counts toward Google's daily quota; the API key is never printed.

    export GOOGLE_CSE_API_KEY=...    # if config.toml uses { env = "GOOGLE_CSE_API_KEY" }
    ./check_config.py
"""

import argparse
import os
import sys
from dataclasses import dataclass
from pathlib import Path

import tomllib
from web_tool import Google, ToolError, feed_items, fetch, read_page, search

HERE = Path(__file__).resolve().parent
PAGE = "https://example.com/"
FEED = "https://github.com/python/cpython/releases.atom"


@dataclass(frozen=True)
class Check:
    ok: bool
    what: str
    detail: str = ""
    required: bool = True


def resolve(value: object, name: str, secrets_dir: Path) -> str:
    """A config value: a literal, `{ env = "NAME" }` or `{ secret = "name" }`. Errors never show values."""
    if isinstance(value, (str, int)):
        return str(value)
    if isinstance(value, dict) and set(value) == {"env"}:
        variable = str(value["env"])
        resolved = os.environ.get(variable, "").strip()
        if "" == resolved:
            raise ToolError(f"{name}: environment variable {variable} is not set in this shell")
        return resolved
    if isinstance(value, dict) and set(value) == {"secret"}:
        secret = str(value["secret"])
        if "/" in secret or secret.startswith(".") or len(secret) > 64:
            raise ToolError(f"{name}: the secret name should name a file, not hold the secret itself")
        path = secrets_dir / secret
        if not path.is_file():
            raise ToolError(f"{name}: secret file {path} not found")
        return path.read_text(encoding="utf-8").strip()
    raise ToolError(f"{name} must be a string, {{ env = \"NAME\" }} or {{ secret = \"name\" }}")


def load_env(config: Path, secrets_dir: Path) -> dict[str, str]:
    """The plugin's `[env]`, or nothing when there is no config.toml (it is optional)."""
    if not config.is_file():
        return {}
    with config.open("rb") as file:
        table = tomllib.load(file).get("env", {})
    if not isinstance(table, dict):
        raise ToolError(f"{config}: [env] must be a table")
    return {str(name): resolve(value, str(name), secrets_dir) for name, value in table.items()}


def live_checks(env: dict[str, str]) -> list[Check]:
    checks: list[Check] = []
    try:
        page = read_page(PAGE)
        checks.append(Check(bool(page.lines), f"web_page reads {PAGE} ({page.title!r}, {len(page.lines)} lines)"))
    except ToolError as error:
        checks.append(Check(False, f"web_page reads {PAGE}", str(error)))
    try:
        items = feed_items(fetch(FEED))
        checks.append(Check(bool(items), f"web_feed_item reads a feed ({len(items)} items in {FEED})"))
    except ToolError as error:
        checks.append(Check(False, "web_feed_item reads a feed", str(error)))
    try:
        read_page("http://localhost/")
        checks.append(Check(False, "this server's own addresses are refused", "localhost was fetched"))
    except ToolError as error:
        refused = "address of this server" in str(error)
        checks.append(Check(refused, "this server's own addresses are refused", "" if refused else str(error)))
    try:
        google = Google.from_env(env)
    except ToolError:
        checks.append(Check(False, "web_search: GOOGLE_CSE_API_KEY and GOOGLE_CSE_ID not set", "optional: see "
                            "config.example.toml; without them web_search only explains how to set it up", False))
        return checks
    try:
        found = search(env, "open-meteo weather api", 3)
        results = found.get("results")
        first = results[0].get("url") if isinstance(results, list) and results and isinstance(results[0], dict) else ""
        checks.append(Check(True, f"web_search through Google (engine {google.cx}): first result {first}"))
    except ToolError as error:
        checks.append(Check(False, f"web_search through Google (engine {google.cx})", google.scrub(str(error))))
    return checks


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--config", type=Path, default=HERE / "config.toml")
    parser.add_argument("--secrets-dir", type=Path, default=Path("/run/secrets"))
    args = parser.parse_args()
    try:
        env = load_env(args.config, args.secrets_dir)
    except (OSError, ValueError, ToolError) as error:
        print(f"failed: {error}", file=sys.stderr)
        return 1
    results = live_checks(env)
    for check in results:
        print(f"  {'ok  ' if check.ok else ('FAIL' if check.required else 'warn')} {check.what}")
        if check.detail and not check.ok:
            print(f"         {check.detail}")
    return 0 if all(check.ok or not check.required for check in results) else 1


if __name__ == "__main__":
    sys.exit(main())
