#!/usr/bin/env python3
"""Tests for the Home Assistant plugin, against a fake hacli. Run: python3 -m unittest -v test_ha_tool.py

HA_LIVE_TEST=1 also runs check_config.py's read-only checks against the Home Assistant in
HA_URL, with the token in HA_TOKEN (hacli must be on PATH)."""

import json
import os
import tempfile
import unittest
from pathlib import Path

import ha_tool as tool
from check_config import live_checks, plugin_env, unknown_entries
from ha_tool import Hacli, Settings, ToolError

ENV = {"HA_URL": "http://ha.local:8123/", "HA_TOKEN": "s3cret-token"}
SETTINGS = Settings.from_env({**ENV, "HA_NO_APPROVAL": "light, notify.phone"})


def state(entity: str, value: str, **attributes: object) -> tool.Json:
    return {"entity_id": entity, "state": value, "attributes": attributes, "last_changed": "2026-09-30T10:00:00Z"}


class FakeHacli(Hacli):
    """Answers from canned replies keyed by the arguments' first words, and records the calls."""

    def __init__(self, replies: dict[tuple[str, ...], object], settings: Settings = SETTINGS) -> None:
        super().__init__(settings, {})
        self.replies = replies
        self.calls: list[list[str]] = []

    def run(self, args: list[str]) -> str:
        self.calls.append(args)
        words = args[2:] if args[:2] == ["-o", "json"] else args
        for length in range(len(words), 0, -1):
            reply = self.replies.get(tuple(words[:length]))
            if isinstance(reply, ToolError):
                raise reply
            if reply is not None:
                return reply if isinstance(reply, str) else json.dumps(reply)
        raise AssertionError(f"unexpected hacli call {args}")


class SettingsTests(unittest.TestCase):

    def test_settings_from_the_environment(self) -> None:
        settings = Settings.from_env(ENV)
        self.assertEqual("http://ha.local:8123", settings.url)
        self.assertEqual(set(), settings.no_approval)
        self.assertIn("homeassistant.restart", settings.denied)
        self.assertEqual({"light", "notify.phone"}, SETTINGS.no_approval)
        self.assertEqual(set(), Settings.from_env({**ENV, "HA_DENIED_SERVICES": ""}).denied)

    def test_url_and_token_are_required(self) -> None:
        for env, error in (({"HA_TOKEN": "t"}, "HA_URL is not set"),
                           ({**ENV, "HA_URL": "ha.local"}, "http"),
                           ({"HA_URL": "http://ha.local"}, "HA_TOKEN is not set")):
            with self.assertRaisesRegex(ToolError, error):
                Settings.from_env(env)

    def test_hacli_gets_the_settings_and_the_token_is_scrubbed(self) -> None:
        hacli = Hacli(SETTINGS, {"PATH": "/bin"})
        self.assertEqual({"PATH": "/bin", "HA_URL": "http://ha.local:8123", "HA_TOKEN": "s3cret-token",
                          "NO_COLOR": "1"}, hacli.env)
        self.assertEqual("bad *** here", SETTINGS.scrub("bad s3cret-token here"))

    def test_hacli_errors_are_made_readable(self) -> None:
        stderr = ("\x1b[2m2026-09-30T19:42:05Z\x1b[0m \x1b[31mERROR\x1b[0m hacli: API error (HTTP 404): gone\n"
                  "Error: Home Assistant API error (HTTP 404): gone\n")
        self.assertEqual("Home Assistant API error (HTTP 404): gone", tool.hacli_error(stderr))
        self.assertIn("refused HA_TOKEN", tool.hacli_error("Error: API error (HTTP 401): 401: Unauthorized"))
        self.assertIn("is HA_URL right", tool.hacli_error("Error: HTTP request failed: error sending request"))
        self.assertEqual("hacli failed without saying why", tool.hacli_error(""))


class ToolTests(unittest.TestCase):

    def test_entities_are_listed_compactly_and_filtered(self) -> None:
        hacli = FakeHacli({("state", "list"): [
            state("sensor.kitchen_temperature", "21.5", unit_of_measurement="°C", friendly_name="Kitchen"),
            state("light.kitchen", "on", friendly_name="Kitchen light"),
            state("lock.front_door", "locked")]})
        self.assertEqual("3 entities:\nlight.kitchen = on  (Kitchen light)\nlock.front_door = locked\n"
                         "sensor.kitchen_temperature = 21.5 °C  (Kitchen)", tool.entities(hacli, None, None))
        self.assertEqual("1 entities:\nlight.kitchen = on  (Kitchen light)", tool.entities(hacli, "light.", "KITCHEN"))
        self.assertIn("No entity domain climate", tool.entities(hacli, "climate", None))

    def test_services_list_target_and_flattened_fields(self) -> None:
        hacli = FakeHacli({("service", "list", "light"): [{"domain": "light", "services": {"turn_on": {
            "target": {"entity": [{"domain": ["light"]}]},
            "fields": {"transition": {"selector": {"number": {}}},
                       "advanced": {"collapsed": True, "fields": {"flash": {"selector": {"select": {}}}}},
                       "color": {"required": True}}}}}]})
        self.assertEqual("Services (* = required field; pass a target as entity_id in data):\n"
                         "light.turn_on  target: entity  fields: transition (number), flash (select), color*",
                         tool.services(hacli, "light"))

    def test_history_is_flattened(self) -> None:
        hacli = FakeHacli({("history",): [[{"entity_id": "sun.sun", "state": "below_horizon", "attributes": {},
                                            "last_changed": "2026-09-29T04:00:00+00:00"},
                                           {"state": "above_horizon", "last_changed": "2026-09-29T11:00:00+00:00"}]]})
        result = tool.history(hacli, "sun.sun", "2026-09-29T00:00:00+00:00", None, False)
        self.assertEqual([{"state": "below_horizon", "when": "2026-09-29T04:00:00+00:00"},
                          {"state": "above_horizon", "when": "2026-09-29T11:00:00+00:00"}], result["changes"])
        self.assertEqual(["-o", "json", "history", "--entity-id", "sun.sun", "--from", "2026-09-29T00:00:00+00:00",
                          "--minimal", "--no-attributes"], hacli.calls[0])
        self.assertIn("no history", str(tool.history(FakeHacli({("history",): []}), "sun.sun", None, None, False)))

    def test_bad_entity_ids_and_times_are_refused(self) -> None:
        with self.assertRaisesRegex(ToolError, "not an entity id"):
            tool.entity_id("kitchen light")
        with self.assertRaisesRegex(ToolError, "not a date"):
            tool.timestamp("yesterday")
        self.assertEqual("2026-09-30T00:00:00+00:00", tool.timestamp("2026-09-30T00:00:00Z"))

    def test_missing_entity_names_the_tool_to_find_it(self) -> None:
        hacli = FakeHacli({("state", "get"): ToolError("Home Assistant API error (HTTP 404): Entity not found.")})
        with self.assertRaisesRegex(ToolError, "no entity 'light.nope': find it with ha_entities"):
            tool.get_state(hacli, "light.nope")

    def test_call_encodes_every_field_as_json(self) -> None:
        hacli = FakeHacli({("service", "call"): [state("light.kitchen", "on"), state("light.hall", "on")]})
        result = tool.call(hacli, "Light", "turn_on",
                           '{"entity_id": ["light.kitchen", "light.hall"], "brightness": 200, "color_name": "on"}',
                           False)
        self.assertEqual(["-o", "json", "service", "call", "light", "turn_on",
                          "--field", 'entity_id=["light.kitchen", "light.hall"]', "--field", "brightness=200",
                          "--field", 'color_name="on"'], hacli.calls[0])
        self.assertEqual({"called": "light.turn_on", "changed": [{"entity_id": "light.kitchen", "state": "on"},
                                                                 {"entity_id": "light.hall", "state": "on"}]}, result)

    def test_call_returns_a_service_response(self) -> None:
        hacli = FakeHacli({("service", "call"): {"changed_states": [], "service_response": {"weather.home": {}}}})
        result = tool.call(hacli, "weather", "get_forecasts", None, True)
        self.assertEqual({"called": "weather.get_forecasts", "changed": [], "response": {"weather.home": {}}}, result)
        self.assertEqual("--return-response", hacli.calls[0][-1])

    def test_call_refuses_denied_services_and_bad_data(self) -> None:
        hacli = FakeHacli({})
        for domain, service, data, error in (("homeassistant", "restart", None, "not allowed"),
                                             ("hassio", "addon_stop", None, "not allowed"),
                                             ("light", "turn on", None, "names like"),
                                             ("light", "turn_on", "[1]", "JSON object"),
                                             ("light", "turn_on", "{", "JSON object")):
            with self.assertRaisesRegex(ToolError, error):
                tool.call(hacli, domain, service, data, False)
        self.assertEqual([], hacli.calls)

    def test_approval_is_skipped_only_for_listed_services(self) -> None:
        def required(domain: str, service: str) -> object:
            return tool.needs_approval(SETTINGS, {"args": {"domain": domain, "service": service},
                                                  "trusted": []})["required"]
        self.assertFalse(required("light", "turn_off"))
        self.assertFalse(required("notify", "phone"))
        self.assertTrue(required("notify", "everyone"))
        self.assertTrue(required("lock", "unlock"))
        self.assertTrue(required("", ""))
        self.assertFalse(required("homeassistant", "restart"))


class ConditionTests(unittest.TestCase):

    def test_state_tests(self) -> None:
        self.assertTrue(tool.StateTest.from_params({"state": "ON"}).matches("on"))
        self.assertTrue(tool.StateTest.from_params({"state": "home", "not": True}).matches("not_home"))
        freezer = tool.StateTest.from_params({"above": -10})
        self.assertTrue(freezer.matches("-8.5"))
        self.assertFalse(freezer.matches("-12"))
        self.assertFalse(freezer.matches("unavailable"))
        self.assertTrue(tool.StateTest.from_params({"above": 1, "below": 3}).matches("2"))
        cases: list[tuple[tool.Json, str]] = [({}, "give `state`"), ({"state": "on", "above": 1}, "not both"),
                                              ({"above": "warm"}, "must be a number")]
        for params, error in cases:
            with self.assertRaisesRegex(ToolError, error):
                tool.StateTest.from_params(params)

    def test_state_is_fires_on_the_current_state(self) -> None:
        hacli = FakeHacli({("state", "get"): state("lock.front_door", "unlocked", friendly_name="Front door")})
        result = tool.check_state(hacli, {"entity_id": "lock.front_door", "state": "unlocked"}, None)
        self.assertEqual("fired", result["status"])
        self.assertEqual({"entity_id": "lock.front_door", "name": "Front door", "state": "unlocked",
                          "when": "2026-09-30T10:00:00Z", "current_state": "unlocked"}, tool.as_list(result["events"])[0])
        # The first check reads no history.
        self.assertEqual(1, len(hacli.calls))

    def test_state_is_catches_a_state_between_two_checks(self) -> None:
        hacli = FakeHacli({("state", "get"): state("binary_sensor.door", "off"),
                           ("history",): [[{"state": "off", "last_changed": "2026-09-30T09:00:00Z"},
                                           {"state": "on", "last_changed": "2026-09-30T09:01:00Z"},
                                           {"state": "off", "last_changed": "2026-09-30T09:02:00Z"}]]})
        params: tool.Json = {"entity_id": "binary_sensor.door", "state": "on"}
        pending = tool.check_state(hacli, params, None)
        self.assertEqual("pending", pending["status"])
        fired = tool.check_state(hacli, params, pending["cursor"])
        self.assertEqual("fired", fired["status"])
        self.assertEqual("2026-09-30T09:01:00Z", tool.as_list(fired["events"])[0]["when"])
        self.assertEqual(tool.as_dict(pending["cursor"])["since"], hacli.calls[-1][6])

    def test_state_changed_records_first_then_fires_with_the_changes(self) -> None:
        replies: dict[tuple[str, ...], object] = {("state", "get"): state("climate.home", "heat"),
                                                  ("history",): [[{"state": "heat", "last_changed": "t0"}]]}
        hacli = FakeHacli(replies)
        first = tool.check_changed(hacli, {"entity_id": "climate.home"}, None)
        self.assertEqual(("pending", "heat"), (first["status"], tool.as_dict(first["cursor"])["state"]))
        self.assertEqual("pending", tool.check_changed(hacli, {"entity_id": "climate.home"}, first["cursor"])["status"])
        replies[("history",)] = [[{"state": "heat", "last_changed": "t0"}, {"state": "off", "last_changed": "t1"},
                                  {"state": "heat", "last_changed": "t2"}]]
        fired = tool.check_changed(hacli, {"entity_id": "climate.home"}, first["cursor"])
        self.assertEqual("fired", fired["status"])
        self.assertEqual({"entity_id": "climate.home", "name": None, "from": "heat", "to": "heat",
                          "changes": [{"state": "off", "when": "t1"}]}, tool.as_list(fired["events"])[0])

    def test_template_true(self) -> None:
        for rendered, status in (("True\n", "fired"), ("on", "fired"), ("False", "pending"), ("", "pending")):
            hacli = FakeHacli({("template",): rendered})
            self.assertEqual(status, tool.check_template(hacli, {"template": "{{ x }}"}, None)["status"])
        with self.assertRaisesRegex(ToolError, "missing parameter `template`"):
            tool.check_template(FakeHacli({}), {}, None)


class CheckConfigTests(unittest.TestCase):

    def test_config_is_resolved_like_the_server_does(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            config, secrets = Path(tmp) / "config.toml", Path(tmp) / "secrets"
            secrets.mkdir()
            (secrets / "ha_token").write_text("from-a-file\n")
            config.write_text('[env]\nHA_URL = "http://ha.local:8123"\nHA_TOKEN = { secret = "ha_token" }\n')
            env = plugin_env(config, secrets)
            self.assertEqual(("http://ha.local:8123", "from-a-file"), (env["HA_URL"], env["HA_TOKEN"]))
            config.write_text('[env]\nHA_TOKEN = { env = "CLANKJOB_TEST_NOT_SET" }\n')
            with self.assertRaisesRegex(ToolError, "CLANKJOB_TEST_NOT_SET is not set"):
                plugin_env(config, secrets)

    def test_unknown_no_approval_entries_are_typos(self) -> None:
        self.assertEqual(["lihgt", "notify.nope"],
                         unknown_entries({"light", "lihgt", "notify.phone", "notify.nope"},
                                         {"light.turn_on", "notify.phone"}))

    def test_live_checks_against_a_fake(self) -> None:
        hacli = FakeHacli({("api", "ping"): {"message": "API running."},
                           ("api", "config"): {"version": "2026.9.3", "location_name": "Home", "time_zone": "UTC"},
                           ("state", "list"): [state("sun.sun", "above_horizon")],
                           ("template",): "42\n",
                           ("service", "list"): [{"domain": "light", "services": {"turn_on": {}}},
                                                 {"domain": "notify", "services": {"phone": {}}}]})
        checks = live_checks(hacli, None)
        self.assertTrue(all(check.ok for check in checks), checks)
        refused = FakeHacli({("api", "ping"): ToolError("HTTP 401 refused HA_TOKEN")})
        self.assertEqual([False], [check.ok for check in live_checks(refused, None)])


@unittest.skipUnless("1" == os.environ.get("HA_LIVE_TEST"), "set HA_LIVE_TEST=1, HA_URL and HA_TOKEN")
class LiveTests(unittest.TestCase):

    def test_check_config_passes(self) -> None:
        settings = Settings.from_env(dict(os.environ))
        checks = live_checks(Hacli(settings, dict(os.environ)), "sun.sun")
        self.assertTrue(all(check.ok or not check.required for check in checks), checks)


if __name__ == "__main__":
    unittest.main()
