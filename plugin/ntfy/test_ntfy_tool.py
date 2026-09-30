#!/usr/bin/env python3
"""Tests for the ntfy plugin, against a fake ntfy. Run: python3 -m unittest -v test_ntfy_tool.py

NTFY_LIVE_TEST=1 also runs check_config.py's read-only checks against the server in
NTFY_URL (ntfy.sh by default) and NTFY_TOPIC."""

import base64
import os
import tempfile
import unittest
from dataclasses import dataclass, field
from pathlib import Path

import ntfy_tool as tool
from check_config import live_checks, load_env
from ntfy_tool import Json, Server, ToolError

ENV = {"NTFY_URL": "https://ntfy.example/", "NTFY_TOPIC": "alerts"}


@dataclass(frozen=True)
class FakeServer(Server):
    """Answers from canned replies by path, and records what was asked."""

    replies: dict[str, Json | ToolError] = field(default_factory=dict)
    calls: list[tuple[str, Json | None]] = field(default_factory=list)

    def request(self, path: str, body: Json | None = None) -> Json:
        self.calls.append((path, body))
        reply = self.replies[path]
        if isinstance(reply, ToolError):
            raise reply
        return reply


class ServerTests(unittest.TestCase):

    def test_config_is_read_from_the_environment(self) -> None:
        server = Server.from_env(ENV)
        self.assertEqual(("https://ntfy.example", "alerts", "no login"), (server.url, server.topic, server.auth))
        self.assertEqual({"User-Agent": tool.USER_AGENT}, server.headers())

    def test_bad_config_is_refused(self) -> None:
        for env, error in (({"NTFY_TOPIC": "alerts"}, "NTFY_URL is not set"),
                           ({**ENV, "NTFY_URL": "ntfy.sh"}, "http"),
                           ({**ENV, "NTFY_TOPIC": ""}, "NTFY_TOPIC"),
                           ({**ENV, "NTFY_TOPIC": "a/b"}, "NTFY_TOPIC"),
                           ({**ENV, "NTFY_TOKEN": "tk_x", "NTFY_USERNAME": "u", "NTFY_PASSWORD": "p"}, "not both"),
                           ({**ENV, "NTFY_USERNAME": "u"}, "go together")):
            with self.assertRaisesRegex(ToolError, error):
                Server.from_env(env)

    def test_token_and_basic_authentication(self) -> None:
        token = Server.from_env({**ENV, "NTFY_TOKEN": "tk_s3cret"})
        self.assertEqual("Bearer tk_s3cret", token.headers()["Authorization"])
        self.assertEqual("access token", token.auth)
        basic = Server.from_env({**ENV, "NTFY_USERNAME": "joe", "NTFY_PASSWORD": "pw"})
        self.assertEqual(f"Basic {base64.b64encode(b'joe:pw').decode()}", basic.headers()["Authorization"])
        self.assertEqual("user joe", basic.auth)

    def test_secrets_are_scrubbed_from_errors(self) -> None:
        self.assertEqual("bad *** here", Server.from_env({**ENV,
                         "NTFY_TOKEN": "tk_s3cret"}).scrub("bad tk_s3cret here"))
        self.assertEqual(
            "x *** y", Server.from_env({**ENV, "NTFY_USERNAME": "u", "NTFY_PASSWORD": "pw"}).scrub("x pw y"))

    def test_error_reason_from_a_json_or_text_body(self) -> None:
        self.assertEqual("unauthorized", tool.reason(b'{"code":40101,"http":401,"error":"unauthorized"}'))
        self.assertEqual("Bad Gateway", tool.reason(b"Bad Gateway"))


class NotificationTests(unittest.TestCase):

    def test_only_the_fields_given_are_sent(self) -> None:
        self.assertEqual({"topic": "alerts", "message": "done"},
                         tool.notification("alerts", " done ", None, None, None, None, False))

    def test_every_field(self) -> None:
        body = tool.notification("alerts", "**disk** full", "Server", "urgent", "warning, rotating_light,",
                                 "https://example.com/case/1", True)
        self.assertEqual({"topic": "alerts", "message": "**disk** full", "title": "Server", "priority": 5,
                          "tags": ["warning", "rotating_light"], "click": "https://example.com/case/1",
                          "markdown": True}, body)

    def test_bad_fields_are_refused(self) -> None:
        for args, error in ((("  ", None, None, None, None), "empty"),
                            (("x" * 4097, None, None, None, None), "4096 bytes"),
                            (("é" * 2049, None, None, None, None), "4096 bytes"),
                            (("m", "t" * 251, None, None, None), "title"),
                            (("m", None, "loud", None, None), "priority"),
                            (("m", None, None, ",".join("abcdefghijk"), None), "at most 10 tags"),
                            (("m", None, None, None, "javascript:alert(1)"), "click")):
            with self.assertRaisesRegex(ToolError, error):
                tool.notification("alerts", *args, False)

    def test_tapping_opens_the_case_unless_another_url_is_given(self) -> None:
        env = {"CLANKJOB_CASE_URL": "https://cj.example.com/#/cases/01J9"}
        self.assertEqual("https://cj.example.com/#/cases/01J9", tool.click_url(None, env))
        self.assertEqual("https://cj.example.com/#/cases/01J9", tool.click_url(" ", env))
        self.assertEqual("https://example.com/report", tool.click_url("https://example.com/report", env))
        self.assertIsNone(tool.click_url(None, {}))

    def test_send_posts_to_the_root_and_reports_the_id(self) -> None:
        server = FakeServer("https://ntfy.example", "alerts",
                            replies={"/": {"id": "abc", "time": 1700000000, "event": "message", "topic": "alerts"}})
        body = tool.notification("alerts", "done", None, None, None, None, False)
        self.assertEqual({"sent": True, "topic": "alerts", "id": "abc", "time": 1700000000}, tool.send(server, body))
        self.assertEqual([("/", body)], server.calls)


class CheckConfigTests(unittest.TestCase):

    def test_config_references_are_resolved(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "config.toml"
            (Path(directory) / "ntfy_password").write_text("pw\n", encoding="utf-8")
            config.write_text('[env]\nNTFY_URL = "https://ntfy.example"\nNTFY_TOPIC = "alerts"\n'
                              'NTFY_USERNAME = "joe"\nNTFY_PASSWORD = { secret = "ntfy_password" }\n',
                              encoding="utf-8")
            env = load_env(config, Path(directory))
        self.assertEqual("pw", env["NTFY_PASSWORD"])

    def test_healthy_server_and_accepted_credentials(self) -> None:
        server = FakeServer("https://ntfy.example", "alerts",
                            replies={"/v1/health": {"healthy": True}, "/alerts/auth": {"success": True}})
        checks = live_checks(server)
        self.assertTrue(all(check.ok for check in checks), checks)
        self.assertEqual("topic alerts accepts no login", checks[1].what)

    def test_refused_credentials_fail_with_a_fix(self) -> None:
        server = FakeServer("https://ntfy.example", "alerts", token="tk_x",
                            replies={"/v1/health": {"healthy": True},
                                     "/alerts/auth": ToolError("ntfy refused the request (HTTP 401): unauthorized")})
        checks = live_checks(server)
        self.assertFalse(checks[1].ok)
        self.assertIn("fix: check NTFY_TOKEN", checks[1].detail)

    def test_unreachable_server_stops_the_checks(self) -> None:
        server = FakeServer("https://ntfy.example", "alerts",
                            replies={"/v1/health": ToolError("cannot reach ntfy")})
        self.assertEqual(1, len(live_checks(server)))


@unittest.skipUnless("1" == os.environ.get("NTFY_LIVE_TEST"), "set NTFY_LIVE_TEST=1 to call an ntfy server")
class LiveTests(unittest.TestCase):

    def test_check_config_passes(self) -> None:
        env = {"NTFY_URL": "https://ntfy.sh", "NTFY_TOPIC": "clankjob-live-test", **os.environ}
        checks = live_checks(Server.from_env(env))
        self.assertTrue(all(check.ok for check in checks), checks)


if __name__ == "__main__":
    unittest.main()
