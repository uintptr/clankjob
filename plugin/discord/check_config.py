#!/usr/bin/env python3
"""Check a Discord instance from config.toml against Discord, before the server uses it.

Resolves `bot_token` the way the server will ({ env = ... } or { secret = ... }),
then checks, with read-only calls, everything the plugin needs: the token, the
channel, the Message Content intent, the bot's permissions in the channel, and that
each allowed responder can reply there. The server runs the same checks at startup.
The token is never printed.

    DISCORD_BOT_TOKEN=... ./check_config.py                 # read-only check
    DISCORD_BOT_TOKEN=... ./check_config.py --notify        # also post a test message
    ./check_config.py --instance discord_joe --secrets-dir /run/secrets
"""

import argparse
import os
import sys
from pathlib import Path

import tomllib
from discord_plugin import Json, Plugin, PluginError

HERE = Path(__file__).resolve().parent


def resolve_secret(reference: object, secrets_dir: Path) -> str:
    """Resolve a `{ env = ... }` or `{ secret = ... }` reference; errors never include the value."""
    if isinstance(reference, str):
        print("warning: bot_token is written in the file; use { env = \"DISCORD_BOT_TOKEN\" } instead")
        return reference
    if isinstance(reference, dict) and set(reference) == {"env"}:
        name = str(reference["env"])
        value = os.environ.get(name, "").strip()
        if "" == value:
            raise SystemExit(f"environment variable {name} is not set")
        return value
    if isinstance(reference, dict) and set(reference) == {"secret"}:
        name = str(reference["secret"])
        if "/" in name or "." in name or len(name) > 64:
            raise SystemExit("bot_token's secret name looks like a token, not a file name; "
                             "use { env = \"DISCORD_BOT_TOKEN\" } and keep the token out of the file")
        path = secrets_dir / name
        if not path.is_file():
            raise SystemExit(f"secret file {path} not found")
        return path.read_text(encoding="utf-8").strip()
    raise SystemExit("bot_token must be { env = \"NAME\" } or { secret = \"name\" }")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--config", type=Path, default=HERE / "config.toml")
    parser.add_argument("--instance", help="instance name (default: the only one)")
    parser.add_argument("--secrets-dir", type=Path, default=Path("/run/secrets"))
    parser.add_argument("--notify", action="store_true", help="post a test message to the channel")
    args = parser.parse_args()

    with args.config.open("rb") as file:
        instances = tomllib.load(file).get("instances", {})
    if not isinstance(instances, dict) or 0 == len(instances):
        raise SystemExit(f"{args.config} has no [instances.<name>]")
    name = args.instance or next(iter(instances))
    config = instances.get(name)
    if not isinstance(config, dict):
        raise SystemExit(f"no instance {name!r}; found {', '.join(instances)}")

    token = resolve_secret(config.get("bot_token"), args.secrets_dir)
    params: Json = {"instance": name, "config": {**config, "bot_token": token}}
    plugin = Plugin()
    try:
        found = plugin.check(params)
    except PluginError as error:
        print(f"failed: {str(error).replace(token, '<token>')}", file=sys.stderr)
        return 1
    print(f"bot {found.get('bot')!r} in #{found.get('channel')}")
    failed_required = False
    raw_findings = found.get("findings")
    for raw in raw_findings if isinstance(raw_findings, list) else []:
        if not isinstance(raw, dict):
            continue
        ok, required = raw.get("ok") is True, raw.get("required") is True
        failed_required = failed_required or (required and not ok)
        mark = "ok  " if ok else ("FAIL" if required else "warn")
        print(f"  {mark} {raw.get('what')}")
        if not ok and raw.get("fix"):
            print(f"         fix: {raw.get('fix')}")
    if failed_required:
        print("Fix the FAIL lines, then run this again.", file=sys.stderr)
        return 1
    if args.notify:
        try:
            plugin.notify({**params, "notification": {"case_title": "clankjob",
                                                      "text": "Test message from check_config.py."}})
        except PluginError as error:
            print(f"failed to post: {str(error).replace(token, '<token>')}", file=sys.stderr)
            return 1
        print("ok: test message posted")
    return 0


if __name__ == "__main__":
    sys.exit(main())
