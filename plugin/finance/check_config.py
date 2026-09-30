#!/usr/bin/env python3
"""Check the finance plugin on this machine, the way the server runs it.

Checks that `uv` and `python3` are on PATH, that the scripts are executable and run (the
first run installs their dependencies), and that Yahoo Finance, SEC EDGAR and FRED answer
this machine. Only public, read-only requests; config.toml `[env]` (FRED_API_KEY,
SEC_USER_AGENT) is passed like the server does, and secret values are never printed.

    ./check_config.py
    ./check_config.py --ticker SHOP.TO
"""

import argparse
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

import tomllib

HERE = Path(__file__).resolve().parent
SCRIPTS = HERE / "scripts"
DEFAULT_TICKER = "AAPL"
# The first run of a uv script installs its dependencies (pandas, yfinance, statsmodels).
TIMEOUT = 300
# What the server passes to plugin commands, besides the plugin's own [env].
PASSED_ENV = ("PATH", "HOME", "TZ", "LANG", "LC_ALL", "UV_CACHE_DIR", "XDG_CACHE_HOME")


class CheckError(Exception):
    """The check cannot start (bad config.toml, missing secret)."""


@dataclass(frozen=True)
class Check:
    mark: str  # "ok  ", "FAIL" or "warn"
    what: str
    detail: str = ""


def resolve(value: object, name: str, secrets_dir: Path) -> str:
    """A config value: a literal, `{ env = "NAME" }` or `{ secret = "name" }`. Errors never show values."""
    if isinstance(value, (str, int)):
        return str(value)
    if isinstance(value, dict) and set(value) == {"env"}:
        variable = str(value["env"])
        resolved = os.environ.get(variable, "").strip()
        if "" == resolved:
            raise CheckError(f"{name}: environment variable {variable} is not set in this shell")
        return resolved
    if isinstance(value, dict) and set(value) == {"secret"}:
        secret = str(value["secret"])
        if "/" in secret or secret.startswith(".") or len(secret) > 64:
            raise CheckError(f"{name}: the secret name should name a file, not hold the secret itself")
        path = secrets_dir / secret
        if not path.is_file():
            raise CheckError(f"{name}: secret file {path} not found")
        return path.read_text(encoding="utf-8").strip()
    raise CheckError(f"{name} must be a string, {{ env = \"NAME\" }} or {{ secret = \"name\" }}")


def plugin_env(config: Path, secrets_dir: Path) -> dict[str, str]:
    """The environment the server gives the plugin's commands."""
    env = {name: os.environ[name] for name in PASSED_ENV if name in os.environ}
    if config.is_file():
        with config.open("rb") as file:
            table = tomllib.load(file).get("env", {})
        if not isinstance(table, dict):
            raise CheckError(f"{config}: [env] must be a table")
        env.update({str(name): resolve(value, str(name), secrets_dir) for name, value in table.items()})
    return env


def run_script(script: str, args: list[str], env: dict[str, str]) -> tuple[bool, str]:
    try:
        done = subprocess.run([str(SCRIPTS / script), *args], cwd=HERE, env=env, capture_output=True, text=True,
                              timeout=TIMEOUT, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        return False, str(error)
    if 0 == done.returncode:
        return True, done.stdout
    return False, (done.stderr.strip() or done.stdout.strip())[-600:]


def service(what: str, script: str, args: list[str], env: dict[str, str], fix: str = "") -> Check:
    ok, output = run_script(script, args, env)
    if ok:
        return Check("ok  ", what)
    return Check("FAIL", what, f"{output} (fix: {fix})" if fix else output)


def checks(ticker: str, env: dict[str, str]) -> list[Check]:
    found: list[Check] = []
    missing = [program for program in ("uv", "python3") if shutil.which(program, path=env.get("PATH")) is None]
    for program in ("uv", "python3"):
        fix = "fix: install uv, https://docs.astral.sh/uv/ (it runs the scripts)" if "uv" == program else "fix: install Python 3.11+"
        found.append(Check("FAIL" if program in missing else "ok  ", f"{program} on PATH",
                           fix if program in missing else ""))
    scripts = sorted(path.name for path in SCRIPTS.glob("*.py"))
    blocked = [name for name in scripts if not os.access(SCRIPTS / name, os.X_OK)]
    found.append(Check("FAIL" if blocked else "ok  ", f"{len(scripts)} scripts are executable",
                       f"fix: chmod +x {' '.join(f'scripts/{name}' for name in blocked)}" if blocked else ""))
    if missing or blocked:
        return found

    found.append(service(f"Yahoo Finance quote for {ticker} (scripts/yf.py)", "yf.py", [ticker, "fast-info"], env))
    found.append(service(f"SEC EDGAR filings for {ticker} (scripts/sec.py)", "sec.py",
                         ["filings", ticker, "--count", "1"], env))
    found.append(service("SEC EDGAR 13F filings (scripts/13f.py)", "13f.py", ["filings", "berkshire", "--count", "1"],
                         env))
    if "SEC_USER_AGENT" not in env:
        found.append(Check("warn", "SEC_USER_AGENT set",
                           "fix: optional: set SEC_USER_AGENT = \"Your Name you@example.com\" under [env]; "
                           "SEC rate-limits generic agents"))
    if "FRED_API_KEY" in env:
        found.append(service("FRED observations (scripts/fred.py)", "fred.py", ["series", "FEDFUNDS", "--count", "1"],
                             env, "check FRED_API_KEY"))
    else:
        found.append(Check("warn", "FRED_API_KEY set",
                           "fix: optional: macro_snapshot, fred_series and fred_search need it; free key at "
                           "https://fred.stlouisfed.org/docs/api/api_key.html, then FRED_API_KEY under [env]"))
    found.append(service("DCF model runs (scripts/dcf.py)", "dcf.py", ["--fcf", "100", "--shares", "10"], env))
    return found


def default_config() -> Path:
    """Where the server reads this plugin's settings, the first that exists of: `<id>.toml`
    in CLANKJOB_PLUGIN_CONFIG_DIR (the image's /config/plugins), `<id>/config.toml` in
    CLANKJOB_PLUGINS_DIR (a checkout or an older setup mounted there), else config.toml
    next to this script."""
    candidates = [Path(os.environ[name].strip()) / relative
                  for name, relative in (("CLANKJOB_PLUGIN_CONFIG_DIR", f"{HERE.name}.toml"),
                                         ("CLANKJOB_PLUGINS_DIR", f"{HERE.name}/config.toml"))
                  if os.environ.get(name, "").strip()]
    return next((path for path in candidates if path.is_file()), HERE / "config.toml")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--config", type=Path, default=default_config())
    parser.add_argument("--secrets-dir", type=Path, default=Path("/run/secrets"))
    parser.add_argument("--ticker", default=DEFAULT_TICKER, help="US-listed ticker to test with")
    args = parser.parse_args()
    try:
        env = plugin_env(args.config, args.secrets_dir)
    except (CheckError, OSError, tomllib.TOMLDecodeError) as error:
        print(f"failed: {error}", file=sys.stderr)
        return 1
    secrets = [value for name, value in env.items() if name not in PASSED_ENV and value]
    results = checks(args.ticker, env)
    for check in results:
        print(f"  {check.mark} {check.what}")
        if check.detail:
            detail = check.detail
            for secret in secrets:
                detail = detail.replace(secret, "<secret>")
            print(f"         {detail}")
    return 0 if all("FAIL" != check.mark for check in results) else 1


if __name__ == "__main__":
    sys.exit(main())
