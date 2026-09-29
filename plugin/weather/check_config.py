#!/usr/bin/env python3
"""Check the weather plugin against Open-Meteo: a place lookup, a forecast, past weather.

Reads config.toml (optional) the way the server does (`{ env = ... }` and
`{ secret = ... }` references included), then runs the plugin's own code for each tool.
Read-only; the API key, if any, is never printed.

    ./check_config.py
    ./check_config.py --location "Paris, France"
    export OPEN_METEO_API_KEY=...    # if config.toml uses { env = "OPEN_METEO_API_KEY" }
"""

import argparse
import os
import sys
from dataclasses import dataclass
from datetime import date, timedelta
from pathlib import Path

import tomllib
from weather_tool import Api, ToolError, forecast, history, locate, utc_today

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
    """The plugin's `[env]`, or nothing when there is no config.toml (it is optional)."""
    if not config.is_file():
        return {}
    with config.open("rb") as file:
        table = tomllib.load(file).get("env", {})
    if not isinstance(table, dict):
        raise ToolError(f"{config}: [env] must be a table")
    return {str(name): resolve(value, str(name), secrets_dir) for name, value in table.items()}


def live_checks(api: Api, location: str, today: date) -> list[Check]:
    checks = [Check(True, f"Open-Meteo {'customer servers (API key)' if api.key else 'free servers'}")]
    try:
        place, _, note = locate(api, location)
    except ToolError as error:
        return [*checks, Check(False, f"place lookup {location!r}", str(error))]
    where = ", ".join(part for part in (place.name, place.region, place.country) if part)
    checks.append(Check(not note, f"{location!r} is {where} ({place.latitude}, {place.longitude})", note, False))
    coordinates = f"{place.latitude},{place.longitude}"
    try:
        ahead = forecast(api, coordinates, 2, False, "metric")
        days = ahead.get("daily")
        current = ahead.get("current")
        now = current.get("temperature_2m") if isinstance(current, dict) else None
        checks.append(Check(isinstance(days, list) and 2 == len(days),
                            f"forecast: {now} °C now, {len(days) if isinstance(days, list) else 0} days ahead"))
    except ToolError as error:
        checks.append(Check(False, "forecast", str(error)))
    week_ago = (today - timedelta(days=7)).isoformat()
    try:
        past = history(api, coordinates, week_ago, None, False, "metric", today)
        days = past.get("daily")
        checks.append(Check(isinstance(days, list) and 1 == len(days), f"past weather on {week_ago}",
                            "" if days else str(past.get("note", "no data"))))
    except ToolError as error:
        checks.append(Check(False, f"past weather on {week_ago}", str(error)))
    return checks


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--config", type=Path, default=HERE / "config.toml")
    parser.add_argument("--secrets-dir", type=Path, default=Path("/run/secrets"))
    parser.add_argument("--location", default="Montreal, Quebec", help="place to look up (default: %(default)s)")
    args = parser.parse_args()
    try:
        env = load_env(args.config, args.secrets_dir)
    except (OSError, ValueError, ToolError) as error:
        print(f"failed: {error}", file=sys.stderr)
        return 1
    api = Api.from_env(env)
    results = live_checks(api, args.location, utc_today())
    for check in results:
        print(f"  {'ok  ' if check.ok else ('FAIL' if check.required else 'warn')} {check.what}")
        if check.detail:
            print(f"         {api.scrub(check.detail)}")
    return 0 if all(check.ok or not check.required for check in results) else 1


if __name__ == "__main__":
    sys.exit(main())
