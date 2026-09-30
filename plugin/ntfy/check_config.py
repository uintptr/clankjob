#!/usr/bin/env python3
"""Check the ntfy plugin: the server answers, and accepts the credentials for the topic.

Reads config.toml the way the server does (`{ env = ... }` and `{ secret = ... }`
references included), then asks the ntfy server whether it is healthy and whether the
configured credentials may use the topic. Read-only: nothing is sent unless --notify is
given, and the token or password is never printed.

    ./check_config.py
    ./check_config.py --notify           # also send a test notification
    export NTFY_TOKEN=...                # if config.toml uses { env = "NTFY_TOKEN" }
"""

import argparse
import os
import sys
from dataclasses import dataclass
from pathlib import Path

import tomllib
from ntfy_tool import Server, ToolError, notification, send

HERE = Path(__file__).resolve().parent


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


def live_checks(server: Server) -> list[Check]:
    try:
        health = server.request("/v1/health")
    except ToolError as error:
        return [Check(False, f"ntfy server at {server.url}", str(error))]
    checks = [Check(True is health.get("healthy"), f"ntfy server at {server.url} is healthy",
                    f"it answered {health}")]
    try:
        # ntfy's own clients ask this before subscribing: it checks the credentials and
        # the right to read the topic. Publishing can only be tried by sending (--notify).
        reply = server.request(f"/{server.topic}/auth")
        checks.append(Check(True is reply.get("success"), f"topic {server.topic} accepts {server.auth}",
                            f"it answered {reply}"))
    except ToolError as error:
        fix = ("fix: check NTFY_TOKEN, or NTFY_USERNAME and NTFY_PASSWORD, and that the user may use the topic"
               if "HTTP 401" in str(error) or "HTTP 403" in str(error) else "")
        checks.append(Check(False, f"topic {server.topic} accepts {server.auth}",
                            str(error) + (f"\n         {fix}" if fix else "")))
    return checks


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
    parser.add_argument("--config",
                        type=Path,
                        default=default_config())
    parser.add_argument("--secrets-dir",
                        type=Path,
                        default=Path("/run/secrets"))
    parser.add_argument("--notify",
                        action="store_true",
                        help="also send a test notification, once the other checks passed")
    args = parser.parse_args()
    try:
        server = Server.from_env(load_env(args.config, args.secrets_dir))
    except (OSError, ValueError, ToolError) as error:
        print(f"failed: {error}", file=sys.stderr)
        return 1
    results = live_checks(server)
    passed = all(check.ok or not check.required for check in results)
    if args.notify and passed:
        try:
            body = notification(server.topic, "If you can read this, clankjob can notify you.",
                                "clankjob: ntfy check", None, "white_check_mark", None, False)
            sent = send(server, body)
            results.append(Check(True, f"test notification sent (id {sent.get('id')})"))
        except ToolError as error:
            results.append(Check(False, "test notification", str(error)))
    for check in results:
        print(f"  {'ok  ' if check.ok else ('FAIL' if check.required else 'warn')} {check.what}")
        if check.detail and not check.ok:
            print(f"         {server.scrub(check.detail)}")
    return 0 if all(check.ok or not check.required for check in results) else 1


if __name__ == "__main__":
    sys.exit(main())
