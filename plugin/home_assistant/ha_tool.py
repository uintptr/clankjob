#!/usr/bin/env python3
"""clankjob Home Assistant plugin: states, history, templates and services through hacli.

A command plugin (design section 9.9): each tool call runs this script, which drives
hacli (https://github.com/uintptr/hacli, the Home Assistant REST API) and prints the
result compacted for the LLM. Standard library only.

    entities [--domain=D] [--search=TEXT]      entity ids, names and states, one per line
    state ENTITY_ID                            one entity: state and attributes (JSON)
    template --template=JINJA                  a template rendered against live state
    history ENTITY_ID [--start=T] [--end=T] [--attributes]
    logbook [--entity-id=E] [--start=T] [--end=T]
    services [--domain=D]                      services, their target and fields
    calendar-events CALENDAR_ID [--start=T] [--end=T]
    call DOMAIN SERVICE [--data=JSON] [--return-response]
    needs-approval                             (stdin: {"args", "trusted"}) approval_check
    check-state | check-changed | check-template
                                               wait conditions: stdin {"params", "cursor"},
                                               stdout {"status", "events", "cursor"}

Environment, set through `[env]` in config.toml:

    HA_URL               Home Assistant's URL, e.g. http://homeassistant.local:8123
    HA_TOKEN             a long-lived access token (profile page, Security tab)
    HA_NO_APPROVAL       domains or services called without asking, comma-separated,
                         e.g. `light, scene, notify.mobile_app_phone` (default: none)
    HA_DENIED_SERVICES   domains or services never called, comma-separated
                         (default: DEFAULT_DENIED below; set it to "" to allow them all)
"""

import argparse
import datetime
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field

Json = dict[str, object]

HACLI_TIMEOUT = 60
# Restarting or stopping Home Assistant, add-ons and backups, and the recorder's purges:
# nothing a case should do, even with the owner's approval of one call.
DEFAULT_DENIED = "homeassistant.stop, homeassistant.restart, hassio, backup, recorder"
TRUE_VALUES = {"true", "on", "yes", "1"}
MAX_EVENT_CHANGES = 20
ANSI = re.compile(r"\x1b\[[0-9;]*m")
NAME = re.compile(r"^[a-z0-9_]+$")
ENTITY_ID = re.compile(r"^[a-z0-9_]+\.[a-z0-9_]+$")


class ToolError(Exception):
    """A failure reported to the LLM (printed to stderr, exit status 1)."""


def names(value: str) -> set[str]:
    """`light, Scene ,notify.phone` -> {"light", "scene", "notify.phone"}."""
    return {part.strip().lower() for part in value.split(",") if part.strip()}


@dataclass(frozen=True)
class Settings:
    url: str
    token: str
    no_approval: set[str] = field(default_factory=set[str])
    denied: set[str] = field(default_factory=set[str])

    @classmethod
    def from_env(cls, env: dict[str, str]) -> "Settings":
        url = env.get("HA_URL", "").strip().rstrip("/")
        token = env.get("HA_TOKEN", "").strip()
        # Both are required here, so hacli never falls back to a ~/.config/hacli/config.toml
        # that happens to be on the machine.
        if not url:
            raise ToolError("HA_URL is not set: copy config.example.toml to config.toml and set it")
        if not re.match(r"^https?://[^/]+", url):
            raise ToolError(f"HA_URL must be an http(s) URL like http://homeassistant.local:8123, got {url!r}")
        if not token:
            raise ToolError("HA_TOKEN is not set: create a long-lived access token in Home Assistant")
        return cls(url, token, names(env.get("HA_NO_APPROVAL", "")),
                   names(env.get("HA_DENIED_SERVICES", DEFAULT_DENIED)))

    def scrub(self, text: str) -> str:
        return text.replace(self.token, "***") if self.token else text


def listed(domain: str, service: str, entries: set[str]) -> bool:
    """Whether a service is named in a setting, by itself or by its domain."""
    return domain in entries or f"{domain}.{service}" in entries


class Hacli:
    """Runs hacli with the plugin's settings; `run` is replaced by a fake in the tests."""

    def __init__(self, settings: Settings, env: dict[str, str]) -> None:
        self.settings = settings
        self.env = {**env, "HA_URL": settings.url, "HA_TOKEN": settings.token, "NO_COLOR": "1"}

    def run(self, args: list[str]) -> str:
        try:
            done = subprocess.run(["hacli", *args], env=self.env, capture_output=True, text=True,
                                  timeout=HACLI_TIMEOUT, check=False)
        except FileNotFoundError:
            raise ToolError("hacli is not installed or not on PATH (https://github.com/uintptr/hacli)") from None
        except subprocess.TimeoutExpired:
            raise ToolError(f"Home Assistant did not answer within {HACLI_TIMEOUT} s") from None
        if 0 != done.returncode:
            raise ToolError(self.settings.scrub(hacli_error(done.stderr)))
        return done.stdout

    def json(self, args: list[str]) -> object:
        output = self.run(["-o", "json", *args])
        try:
            return json.loads(output)
        except ValueError:
            raise ToolError(f"hacli printed something that is not JSON: {output[:200]!r}") from None


def hacli_error(stderr: str) -> str:
    """The error hacli reports (its `Error: ...` line), made readable."""
    lines = [ANSI.sub("", line).strip() for line in stderr.splitlines() if line.strip()]
    errors = [line[len("Error: "):] for line in lines if line.startswith("Error: ")]
    message = errors[-1] if errors else (lines[-1] if lines else "hacli failed without saying why")
    if "HTTP 401" in message:
        return f"{message} (Home Assistant refused HA_TOKEN: create a new long-lived access token)"
    if "error sending request" in message:
        return f"{message} (is HA_URL right, and is Home Assistant running?)"
    return message


def entity_id(value: str) -> str:
    value = value.strip().lower()
    if not ENTITY_ID.match(value):
        raise ToolError(f"{value!r} is not an entity id like `light.kitchen`: find it with ha_entities")
    return value


def get_state(hacli: Hacli, entity: str) -> Json:
    try:
        return as_dict(hacli.json(["state", "get", entity_id(entity)]))
    except ToolError as error:
        if "HTTP 404" in str(error):
            raise ToolError(f"no entity {entity!r}: find it with ha_entities") from None
        raise


def timestamp(value: str | None, default: datetime.datetime | None = None) -> str | None:
    """An ISO 8601 time for hacli; a date or a time without offset is in local time."""
    if not value or not value.strip():
        return default.isoformat(timespec="seconds") if default else None
    try:
        parsed = datetime.datetime.fromisoformat(value.strip())
    except ValueError:
        raise ToolError(f"{value!r} is not a date or time like 2026-09-30 or 2026-09-30T14:00") from None
    return (parsed if parsed.tzinfo else parsed.astimezone()).isoformat(timespec="seconds")


def now() -> datetime.datetime:
    return datetime.datetime.now(datetime.UTC)


def as_list(value: object) -> list[Json]:
    return [item for item in value if isinstance(item, dict)] if isinstance(value, list) else []


def as_dict(value: object) -> Json:
    return value if isinstance(value, dict) else {}


# ---- tools ------------------------------------------------------------------------


def describe(state: Json) -> str:
    """`sensor.kitchen_temperature = 21.5 °C  (Kitchen temperature)`."""
    attributes = as_dict(state.get("attributes"))
    line = f"{state.get('entity_id')} = {state.get('state')}"
    if attributes.get("unit_of_measurement"):
        line += f" {attributes['unit_of_measurement']}"
    if attributes.get("friendly_name"):
        line += f"  ({attributes['friendly_name']})"
    return line


def entities(hacli: Hacli, domain: str | None, search: str | None) -> str:
    states = as_list(hacli.json(["state", "list"]))
    wanted = (domain or "").strip().lower().rstrip(".")
    needle = (search or "").strip().casefold()
    lines = sorted(describe(state) for state in states
                   if (not wanted or str(state.get("entity_id", "")).startswith(f"{wanted}."))
                   and (not needle or needle in describe(state).casefold()))
    if not lines:
        where = " and ".join(part for part in (f"domain {wanted}" if wanted else "",
                                               f"matching {search!r}" if needle else "") if part)
        return f"No entity {where}. Try a shorter search, or no domain." if where else "No entity."
    return f"{len(lines)} entities:\n" + "\n".join(lines)


def service_fields(fields: Json) -> list[str]:
    """Field names, `*` for required ones, flattening collapsed sections ("advanced")."""
    found: list[str] = []
    for name, spec in fields.items():
        spec = as_dict(spec)
        if isinstance(spec.get("fields"), dict):
            found.extend(service_fields(as_dict(spec["fields"])))
            continue
        selector = next(iter(as_dict(spec.get("selector"))), "")
        found.append(f"{name}{'*' if spec.get('required') is True else ''}{f' ({selector})' if selector else ''}")
    return found


def services(hacli: Hacli, domain: str | None) -> str:
    args = ["service", "list"] + ([domain.strip().lower()] if domain and domain.strip() else [])
    lines: list[str] = []
    for entry in as_list(hacli.json(args)):
        for name, spec in sorted(as_dict(entry.get("services")).items()):
            spec = as_dict(spec)
            line = f"{entry.get('domain')}.{name}"
            target = as_dict(spec.get("target"))
            if target:
                line += f"  target: {', '.join(sorted(target))}"
            fields = service_fields(as_dict(spec.get("fields")))
            if fields:
                line += f"  fields: {', '.join(fields)}"
            lines.append(line)
    if not lines:
        return f"No service in domain {domain!r}." if domain else "No service."
    return "\n".join(["Services (* = required field; pass a target as entity_id in data):", *lines])


def history(hacli: Hacli, entity: str, start: str | None, end: str | None, attributes: bool) -> Json:
    args = ["history", "--entity-id", entity_id(entity), "--from",
            timestamp(start, now() - datetime.timedelta(days=1)) or ""]
    end_time = timestamp(end)
    if end_time:
        args += ["--to", end_time]
    if not attributes:
        args += ["--minimal", "--no-attributes"]
    series = hacli.json(args)
    changes: list[Json] = []
    for entry in as_list(series[0] if isinstance(series, list) and series else []):
        change: Json = {"state": entry.get("state"), "when": entry.get("last_changed")}
        if attributes and entry.get("attributes"):
            change["attributes"] = entry["attributes"]
        changes.append(change)
    if not changes:
        return {"entity_id": entity, "changes": [],
                "note": "no history: the entity does not exist, or the recorder does not keep it"}
    return {"entity_id": entity, "changes": changes,
            "note": "the first change is the state the entity already had at the start"}


def logbook(hacli: Hacli, entity: str | None, start: str | None, end: str | None) -> object:
    args = ["logbook", "--from", timestamp(start, now() - datetime.timedelta(days=1)) or ""]
    if entity and entity.strip():
        args += ["--entity-id", entity_id(entity)]
    end_time = timestamp(end)
    if end_time:
        args += ["--to", end_time]
    return hacli.json(args)


def calendar_events(hacli: Hacli, calendar: str, start: str | None, end: str | None) -> object:
    start_time = timestamp(start, now()) or ""
    end_time = timestamp(end, datetime.datetime.fromisoformat(start_time) + datetime.timedelta(days=14)) or ""
    try:
        return hacli.json(["calendar", "events", entity_id(calendar), "--start", start_time, "--end", end_time])
    except ToolError as error:
        if "HTTP 404" in str(error):
            raise ToolError(f"no calendar {calendar!r}: list calendars with ha_entities, domain `calendar`") \
                from None
        raise


def service_data(data: str | None) -> Json:
    if not data or not data.strip():
        return {}
    try:
        parsed = json.loads(data)
    except ValueError as error:
        raise ToolError(f"data must be a JSON object like {{\"entity_id\": \"light.kitchen\"}}: {error}") from None
    if not isinstance(parsed, dict):
        raise ToolError("data must be a JSON object like {\"entity_id\": \"light.kitchen\"}")
    return parsed


def call(hacli: Hacli, domain: str, service: str, data: str | None, return_response: bool) -> Json:
    domain, service = domain.strip().lower(), service.strip().lower()
    if not NAME.match(domain) or not NAME.match(service):
        raise ToolError("domain and service are names like `light` and `turn_on`: find them with ha_services")
    if listed(domain, service, hacli.settings.denied):
        raise ToolError(f"{domain}.{service} is not allowed here (HA_DENIED_SERVICES in the plugin's config)")
    args = ["service", "call", domain, service]
    for key, value in service_data(data).items():
        # hacli reads each value as JSON first, so JSON-encoding it keeps its type:
        # "on" stays a string, 200 a number, ["light.a"] a list.
        args += ["--field", f"{key}={json.dumps(value, ensure_ascii=False)}"]
    if return_response:
        args.append("--return-response")
    reply = hacli.json(args)
    changed = reply.get("changed_states") if isinstance(reply, dict) else reply
    result: Json = {"called": f"{domain}.{service}",
                    "changed": [{"entity_id": state.get("entity_id"), "state": state.get("state")}
                                for state in as_list(changed)]}
    if isinstance(reply, dict) and "service_response" in reply:
        result["response"] = reply["service_response"]
    return result


def needs_approval(settings: Settings, request: Json) -> Json:
    """The server's approval check (`approval_check` in plugin.toml): no approval for the
    domains and services the owner listed in HA_NO_APPROVAL."""
    args = as_dict(request.get("args"))
    domain = str(args.get("domain", "")).strip().lower()
    service = str(args.get("service", "")).strip().lower()
    if listed(domain, service, settings.denied):
        return {"required": False, "reason": f"{domain}.{service} is denied: the call will be refused"}
    if domain and service and listed(domain, service, settings.no_approval):
        return {"required": False, "reason": f"{domain}.{service} is in HA_NO_APPROVAL"}
    return {"required": True, "reason": f"{domain}.{service} is not in HA_NO_APPROVAL"}


# ---- wait conditions ----------------------------------------------------------------


def text_param(params: Json, name: str, required: bool = True) -> str:
    value = params.get(name)
    if isinstance(value, str) and value.strip():
        return value.strip()
    if required:
        raise ToolError(f"missing parameter `{name}`")
    return ""


def number_param(params: Json, name: str) -> float | None:
    value = params.get(name)
    if value is None or "" == value:
        return None
    try:
        return float(str(value))
    except ValueError:
        raise ToolError(f"`{name}` must be a number") from None


@dataclass(frozen=True)
class StateTest:
    """What `ha_state_is` waits for: a state (or any other, with `not`), or a number range."""

    state: str = ""
    negate: bool = False
    above: float | None = None
    below: float | None = None

    @classmethod
    def from_params(cls, params: Json) -> "StateTest":
        test = cls(text_param(params, "state", required=False), params.get("not") is True,
                   number_param(params, "above"), number_param(params, "below"))
        if not test.state and test.above is None and test.below is None:
            raise ToolError("give `state`, or `above` and/or `below` for a number")
        if test.state and (test.above is not None or test.below is not None):
            raise ToolError("give either `state` or `above`/`below`, not both")
        return test

    def matches(self, value: object) -> bool:
        text = str(value)
        if self.state:
            return (text.casefold() == self.state.casefold()) != self.negate
        try:
            number = float(text)
        except ValueError:
            # unavailable, unknown: neither above nor below anything.
            return False
        return (self.above is None or number > self.above) and (self.below is None or number < self.below)


def changes_since(hacli: Hacli, entity: str, cursor: object) -> list[Json]:
    """State changes since the previous check, so a door opened and closed between two
    checks is not missed; nothing on the first check."""
    since = as_dict(cursor).get("since")
    if not isinstance(since, str):
        return []
    series = hacli.json(["history", "--entity-id", entity, "--from", since, "--minimal", "--no-attributes"])
    return as_list(series[0] if isinstance(series, list) and series else [])


def check_state(hacli: Hacli, params: Json, cursor: object) -> Json:
    """Fires when the entity has the state (or number) wanted, now or at any moment since
    the previous check. Checked right away too."""
    entity = entity_id(text_param(params, "entity_id"))
    test = StateTest.from_params(params)
    checked_at = now().isoformat(timespec="seconds")
    current = get_state(hacli, entity)
    earlier = changes_since(hacli, entity, cursor)
    candidates = [(change.get("state"), change.get("last_changed")) for change in earlier]
    candidates.append((current.get("state"), current.get("last_changed")))
    for state, when in candidates:
        if test.matches(state):
            name = as_dict(current.get("attributes")).get("friendly_name")
            event: Json = {"entity_id": entity, "name": name, "state": state, "when": when,
                           "current_state": current.get("state")}
            return {"status": "fired", "events": [event], "cursor": {"since": checked_at}}
    return {"status": "pending", "events": [], "cursor": {"since": checked_at}}


def check_changed(hacli: Hacli, params: Json, cursor: object) -> Json:
    """Fires when the entity's state differs from when the wait started, or changed and
    changed back in between. The first check only records it."""
    entity = entity_id(text_param(params, "entity_id"))
    checked_at = now().isoformat(timespec="seconds")
    current = get_state(hacli, entity)
    before = as_dict(cursor).get("state")
    if "state" not in as_dict(cursor):
        return {"status": "pending", "events": [], "cursor": {"state": current.get("state"), "since": checked_at}}
    changes = [{"state": change.get("state"), "when": change.get("last_changed")}
               for change in changes_since(hacli, entity, cursor) if change.get("state") != before]
    if not changes and current.get("state") == before:
        return {"status": "pending", "events": [], "cursor": {"state": before, "since": checked_at}}
    event: Json = {"entity_id": entity, "name": as_dict(current.get("attributes")).get("friendly_name"),
                   "from": before, "to": current.get("state"), "changes": changes[:MAX_EVENT_CHANGES]}
    return {"status": "fired", "events": [event], "cursor": {"state": current.get("state"), "since": checked_at}}


def check_template(hacli: Hacli, params: Json, cursor: object) -> Json:
    """Fires when the template renders true (`True`, `on`, `yes`, `1`). Checked right away
    too; only the moments of the checks count."""
    template = text_param(params, "template")
    value = hacli.run(["template", template]).strip()
    if value.casefold() not in TRUE_VALUES:
        return {"status": "pending", "events": [], "cursor": None}
    return {"status": "fired", "events": [{"template": template, "value": value}], "cursor": None}


CHECKS = {"check-state": check_state, "check-changed": check_changed, "check-template": check_template}


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    command = sub.add_parser("entities")
    command.add_argument("--domain")
    command.add_argument("--search")
    command = sub.add_parser("state")
    command.add_argument("entity_id")
    command = sub.add_parser("template")
    command.add_argument("--template",
                         required=True)
    command = sub.add_parser("history")
    command.add_argument("entity_id")
    command.add_argument("--start")
    command.add_argument("--end")
    command.add_argument("--attributes",
                         action="store_true")
    command = sub.add_parser("logbook")
    command.add_argument("--entity-id")
    command.add_argument("--start")
    command.add_argument("--end")
    command = sub.add_parser("services")
    command.add_argument("--domain")
    command = sub.add_parser("calendar-events")
    command.add_argument("calendar_id")
    command.add_argument("--start")
    command.add_argument("--end")
    command = sub.add_parser("call")
    command.add_argument("domain")
    command.add_argument("service")
    command.add_argument("--data")
    command.add_argument("--return-response",
                         action="store_true")
    sub.add_parser("needs-approval")
    for name in CHECKS:
        sub.add_parser(name)
    return parser


def run(args: argparse.Namespace, settings: Settings, hacli: Hacli) -> object:
    if "entities" == args.command:
        return entities(hacli, args.domain, args.search)
    if "state" == args.command:
        return get_state(hacli, args.entity_id)
    if "template" == args.command:
        return hacli.run(["template", args.template]).rstrip("\n")
    if "history" == args.command:
        return history(hacli, args.entity_id, args.start, args.end, args.attributes)
    if "logbook" == args.command:
        return logbook(hacli, args.entity_id, args.start, args.end)
    if "services" == args.command:
        return services(hacli, args.domain)
    if "calendar-events" == args.command:
        return calendar_events(hacli, args.calendar_id, args.start, args.end)
    if "call" == args.command:
        return call(hacli, args.domain, args.service, args.data, args.return_response)
    request = json.loads(sys.stdin.read() or "{}")
    request = request if isinstance(request, dict) else {}
    if "needs-approval" == args.command:
        return needs_approval(settings, request)
    return CHECKS[args.command](hacli, as_dict(request.get("params")), request.get("cursor"))


def main() -> int:
    args = build_parser().parse_args()
    settings: Settings | None = None
    try:
        env = dict(os.environ)
        settings = Settings.from_env(env)
        result = run(args, settings, Hacli(settings, env))
    except ToolError as error:
        message = settings.scrub(str(error)) if settings else str(error)
        print(f"Error: {message}", file=sys.stderr)
        return 1
    print(result if isinstance(result, str) else json.dumps(result, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
