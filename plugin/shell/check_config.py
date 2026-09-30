#!/usr/bin/env python3
"""Check the shell plugin: the sandbox answers, runs commands, and holds no secrets.

Reads config.toml the way the server does (`{ env = ... }` and `{ secret = ... }`
references included), then asks the sandbox's exec service to run a few read-only
commands: its user and work directory, the programs the tool description promises,
whether it can see anything of the server (secrets in its environment, /data, /run/secrets,
the server's processes), and whether it reaches the internet.

    ./check_config.py
    docker compose exec clankjob /plugins/shell/check_config.py
"""

import argparse
import os
import sys
from dataclasses import dataclass
from pathlib import Path

import tomllib
from shell_tool import ToolError, call, sandbox_url

HERE = Path(__file__).resolve().parent
PROGRAMS = ("bash", "curl", "wget", "ping", "dig", "nc", "nmap", "tcpdump", "git", "jq", "python3", "uv", "sqlite3", "pandoc", "pdftotext", "tesseract",
            "convert", "ffmpeg", "exiftool")
# One line per finding; empty when the sandbox sees nothing of the server.
LEAKS = r"""
env | cut -d= -f1 | grep -E 'OPENROUTER|OPENAI|DISCORD|EMAIL|_API_KEY|_TOKEN|_PASSWORD|_SECRET' \
  | sed 's/^/environment variable /'
# The image creates /data and /config empty; what matters is whether anything is in them.
for path in /data /run/secrets /config; do [ -n "$(ls -A "$path" 2>/dev/null)" ] && echo "$path is not empty"; done
# [c]: the pattern must not match this command's own command line.
grep -lsa '/usr/local/bin/[c]lankjob' /proc/[0-9]*/cmdline | head -1 | sed 's/.*/the clankjob server process is visible/'
true
"""


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
    with config.open("rb") as file:
        table = tomllib.load(file).get("env", {})
    if not isinstance(table, dict):
        raise ToolError(f"{config}: [env] must be a table")
    return {str(name): resolve(value, str(name), secrets_dir) for name, value in table.items()}


def shell(url: str, command: str) -> tuple[bool, str]:
    reply = call(url, "/run", {"command": command, "timeout": 60}, 90)
    return 0 == reply.get("exit_code"), str(reply.get("stdout") or "") + str(reply.get("stderr") or "")


def live_checks(url: str) -> list[Check]:
    try:
        health = call(url, "/health", None, 10)
    except ToolError as error:
        return [Check(False, f"sandbox at {url}", f"{error}\n         fix: docker compose up -d sandbox")]
    checks = [Check(True, f"sandbox at {url}: user {health.get('user')}, work directory {health.get('work')}")]
    try:
        ok, text = shell(url, "echo round trip && touch .clankjob-check && rm .clankjob-check")
        checks.append(Check(ok and "round trip" in text, "runs a command and writes to its work directory",
                            text.strip()[-300:]))
        _, text = shell(url, " ".join(f"command -v {program} >/dev/null || echo {program};" for program in PROGRAMS))
        missing = text.split()
        checks.append(Check(not missing, "programs: " + ", ".join(PROGRAMS),
                            f"missing: {', '.join(missing)} (an older image? docker compose pull)", False))
        _, text = shell(url, LEAKS)
        leaks = [line for line in text.splitlines() if line.strip()]
        checks.append(Check(not leaks, "sees nothing of the server (secrets, /data, its processes)",
                            "; ".join(leaks) + "\n         fix: give the sandbox service no env_file, no "
                            "/data or /run/secrets volume, and no shared pid namespace"))
        ok, text = shell(url, "curl -sS -o /dev/null -w '%{http_code}' --max-time 20 https://deb.debian.org/")
        checks.append(Check(ok, f"reaches the internet (deb.debian.org: HTTP {text.strip()})"
                            if ok else "reaches the internet", text.strip()[-300:], False))
    except ToolError as error:
        checks.append(Check(False, "runs commands", str(error)))
    return checks


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--config", type=Path, default=HERE / "config.toml")
    parser.add_argument("--secrets-dir", type=Path, default=Path("/run/secrets"))
    args = parser.parse_args()
    try:
        url = sandbox_url(load_env(args.config, args.secrets_dir))
    except (OSError, ValueError, ToolError) as error:
        print(f"failed: {error}", file=sys.stderr)
        return 1
    results = live_checks(url)
    for check in results:
        print(f"  {'ok  ' if check.ok else ('FAIL' if check.required else 'warn')} {check.what}")
        if check.detail and not check.ok:
            print(f"         {check.detail}")
    return 0 if all(check.ok or not check.required for check in results) else 1


if __name__ == "__main__":
    sys.exit(main())
