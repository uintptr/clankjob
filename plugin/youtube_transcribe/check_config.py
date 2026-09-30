#!/usr/bin/env python3
"""Check the YouTube transcripts plugin on this machine, the way the server runs it.

Checks that `uv` is on PATH, that scripts/yt.py runs (its first run installs its
dependencies), and that YouTube serves this machine video details and caption tracks:
many cloud and VPN addresses are blocked, which a proxy fixes. Only public, read-only
requests; the optional config.toml `[env]` (e.g. YT_PROXY_URL) is passed like the
server does, and secret values are never printed.

    ./check_config.py
    ./check_config.py --video dQw4w9WgXcQ
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
SCRIPT = HERE / "scripts" / "yt.py"
# A long-lived public video with captions.
DEFAULT_VIDEO = "dQw4w9WgXcQ"
# The first run installs the script's dependencies.
TIMEOUT = 180
# What the server passes to plugin commands, besides the plugin's own [env].
PASSED_ENV = ("PATH", "HOME", "TZ", "LANG", "LC_ALL")


class CheckError(Exception):
    """The check cannot start (bad config.toml, missing secret)."""


@dataclass(frozen=True)
class Check:
    ok: bool
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


def run_script(args: list[str], env: dict[str, str]) -> tuple[bool, str]:
    try:
        done = subprocess.run([str(SCRIPT), *args], cwd=HERE, env=env, capture_output=True, text=True,
                              timeout=TIMEOUT, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        return False, str(error)
    if 0 == done.returncode:
        return True, done.stdout
    return False, (done.stderr.strip() or done.stdout.strip())[-600:]


def checks(video: str, env: dict[str, str]) -> list[Check]:
    found: list[Check] = []
    uv = shutil.which("uv", path=env.get("PATH"))
    found.append(Check(uv is not None, "uv on PATH",
                       "" if uv else "install uv: https://docs.astral.sh/uv/ (it runs scripts/yt.py)"))
    runnable = SCRIPT.is_file() and os.access(SCRIPT, os.X_OK)
    found.append(Check(runnable, "scripts/yt.py is executable", "" if runnable else f"chmod +x {SCRIPT}"))
    if not (uv and runnable):
        return found
    ok, output = run_script(["info", video], env)
    title = next((line.split(None, 1)[-1].strip() for line in output.splitlines() if line.startswith("Title")), "")
    found.append(
        Check(ok, f"video details from YouTube: {title}" if ok else "video details from YouTube", "" if ok else output))
    ok, output = run_script(["langs", video], env)
    tracks = sum(1 for line in output.splitlines() if line[:3].strip().isalpha() and "  " in line) if ok else 0
    fix = output
    if "RequestBlocked" in output or "IpBlocked" in output:
        fix = "YouTube blocks this machine's address: set YT_PROXY_URL under [env] in config.toml"
    found.append(
        Check(ok, f"caption tracks from YouTube ({tracks} found)" if ok else "caption tracks from YouTube", "" if ok else fix))
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
    parser.add_argument("--video", default=DEFAULT_VIDEO, help="video to test with (URL or id)")
    args = parser.parse_args()
    try:
        env = plugin_env(args.config, args.secrets_dir)
    except (CheckError, OSError, tomllib.TOMLDecodeError) as error:
        print(f"failed: {error}", file=sys.stderr)
        return 1
    secrets = [value for name, value in env.items() if name not in PASSED_ENV and value]
    results = checks(args.video, env)
    for check in results:
        print(f"  {'ok  ' if check.ok else 'FAIL'} {check.what}")
        if check.detail:
            detail = check.detail
            for secret in secrets:
                detail = detail.replace(secret, "<secret>")
            print(f"         fix: {detail}")
    return 0 if all(check.ok for check in results) else 1


if __name__ == "__main__":
    sys.exit(main())
