#!/usr/bin/env python3
"""Check the Home Assistant plugin: hacli is installed, Home Assistant answers and accepts
the token, and states, templates and services can be read.

Reads config.toml the way the server does (`{ env = ... }` and `{ secret = ... }`
references included) and runs hacli through ha_tool.py, with the environment the server
gives the plugin. Read-only: no service is called, and the token is never printed.

    export HA_TOKEN=...                  # if config.toml uses { env = "HA_TOKEN" }
    ./check_config.py
    ./check_config.py --entity lock.front_door   # also read this entity
"""

import argparse
import os
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path

import tomllib
from ha_tool import Hacli, Settings, ToolError, as_dict, as_list, get_state

HERE = Path(__file__).resolve().parent
# What the server passes to plugin commands, besides the plugin's own [env].
PASSED_ENV = ("PATH", "HOME", "TZ", "LANG", "LC_ALL")


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


def plugin_env(config: Path, secrets_dir: Path) -> dict[str, str]:
    """The environment the server gives the plugin's commands."""
    env = {name: os.environ[name] for name in PASSED_ENV if name in os.environ}
    with config.open("rb") as file:
        table = tomllib.load(file).get("env", {})
    if not isinstance(table, dict):
        raise ToolError(f"{config}: [env] must be a table")
    env.update({str(name): resolve(value, str(name), secrets_dir) for name, value in table.items()})
    return env


def service_names(reply: object) -> set[str]:
    """Every `domain.service` in hacli's service list."""
    return {f"{entry.get('domain')}.{name}" for entry in as_list(reply) for name in as_dict(entry.get("services"))}


def unknown_entries(entries: set[str], services: set[str]) -> list[str]:
    """Settings entries that name no domain or service of this Home Assistant (typos)."""
    domains = {name.split(".")[0] for name in services}
    return sorted(entry for entry in entries if entry not in domains and entry not in services)


def live_checks(hacli: Hacli, entity: str | None) -> list[Check]:
    settings = hacli.settings
    try:
        hacli.json(["api", "ping"])
    except ToolError as error:
        return [Check(False, f"Home Assistant at {settings.url} accepts the token", str(error))]
    checks = [Check(True, f"Home Assistant at {settings.url} accepts the token")]
    try:
        config = as_dict(hacli.json(["api", "config"]))
        checks.append(Check(True, f"Home Assistant {config.get('version')}, home {config.get('location_name')!r}, "
                            f"time zone {config.get('time_zone')}"))
        states = as_list(hacli.json(["state", "list"]))
        checks.append(Check(0 < len(states), f"{len(states)} entities",
                            "the token's user sees no entity"))
        rendered = hacli.run(["template", "{{ 20 + 22 }}"]).strip()
        checks.append(Check("42" == rendered, "templates render", f"'{{{{ 20 + 22 }}}}' rendered {rendered!r}"))
        services = service_names(hacli.json(["service", "list"]))
        checks.append(Check(0 < len(services), f"{len(services)} services"))
    except ToolError as error:
        checks.append(Check(False, "Home Assistant answers", str(error)))
        return checks
    # Only HA_NO_APPROVAL: denying a service this Home Assistant does not have is harmless.
    unknown = unknown_entries(settings.no_approval, services)
    listing = ", ".join(sorted(settings.no_approval)) or "none"
    checks.append(Check(not unknown, f"HA_NO_APPROVAL names known domains and services ({listing})",
                        f"not in this Home Assistant: {', '.join(unknown)} (a typo, or an integration "
                        "not installed)", required=False))
    both = sorted(entry for entry in settings.no_approval
                  if entry in settings.denied or entry.split(".")[0] in settings.denied)
    if both:
        checks.append(Check(False, "HA_NO_APPROVAL and HA_DENIED_SERVICES do not overlap",
                            f"denied anyway: {', '.join(both)}", required=False))
    if entity:
        try:
            state = get_state(hacli, entity)
            checks.append(Check(True, f"{entity} is {state.get('state')}"))
        except ToolError as error:
            checks.append(Check(False, f"entity {entity}", str(error)))
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
    parser.add_argument("--entity",
                        help="also read this entity, e.g. lock.front_door")
    args = parser.parse_args()
    try:
        env = plugin_env(args.config, args.secrets_dir)
        settings = Settings.from_env(env)
    except (OSError, ValueError, ToolError) as error:
        print(f"failed: {error}", file=sys.stderr)
        return 1
    hacli_path = shutil.which("hacli", path=env.get("PATH"))
    results = [Check(hacli_path is not None, f"hacli on PATH ({hacli_path})" if hacli_path else "hacli on PATH",
                     "fix: install it: https://github.com/uintptr/hacli (the Docker image has it)")]
    if hacli_path:
        results += live_checks(Hacli(settings, env), args.entity)
    for check in results:
        print(f"  {'ok  ' if check.ok else ('FAIL' if check.required else 'warn')} {check.what}")
        if check.detail and not check.ok:
            print(f"         {settings.scrub(check.detail)}")
    return 0 if all(check.ok or not check.required for check in results) else 1


if __name__ == "__main__":
    sys.exit(main())
