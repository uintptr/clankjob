#!/usr/bin/env python3
"""Tests for the Discord intake, against a fake Discord and a fake clankjob server.
Run: python3 -m unittest -v test_discord_intake.py"""

import json
import tempfile
import unittest
import unittest.mock
from pathlib import Path
from typing import ClassVar

import discord_intake as intake
from discord_intake import Discord, Intake, IntakeError, Json, Server, Settings

BOT = "900"
BOT_ROLE = "777"
JOE = "234"
CHANNEL = "555"
GUILD = "111"


def message(message_id: int, content: str, author: str = JOE, mention: bool = True, bot: bool = False,
            attachments: list[Json] | None = None, roles: list[str] | None = None) -> Json:
    return {"id": str(message_id), "content": content,
            "author": {"id": author, "username": "joe", "global_name": "Joe", "bot": bot},
            "mentions": [{"id": BOT}] if mention else [], "mention_roles": roles or [],
            "attachments": attachments or []}


class Fakes:
    """Discord's channel and the server's case list, answering the intake's HTTP calls."""

    def __init__(self) -> None:
        self.messages: list[Json] = []
        self.replies: list[Json] = []
        self.cases: list[Json] = []
        self.server_status = 201
        self.roles: list[Json] = [{"id": "1", "name": "@everyone"},
                                  {"id": BOT_ROLE, "name": "clankbot", "managed": True, "tags": {"bot_id": BOT}}]

    def discord(self, method: str, url: str, headers: dict[str, str], body: bytes | None) -> tuple[int, bytes]:
        assert headers["Authorization"] == "Bot discord-token"
        path = url.removeprefix(intake.DISCORD_API)
        if "/users/@me" == path:
            return 200, json.dumps({"id": BOT, "username": "clankbot"}).encode()
        if f"/channels/{CHANNEL}" == path:
            return 200, json.dumps({"id": CHANNEL, "name": "general", "guild_id": GUILD}).encode()
        if f"/guilds/{GUILD}/roles" == path:
            return 200, json.dumps(self.roles).encode()
        if method == "POST":
            self.replies.append(json.loads(body or b"{}"))
            return 200, b"{}"
        after = path.split("after=")[1].split("&")[0] if "after=" in path else None
        found = [m for m in self.messages if after is None or int(str(m["id"])) > int(after)]
        # Discord lists the newest first.
        found = sorted(found, key=lambda m: -int(str(m["id"])))
        return 200, json.dumps(found if after else found[:1]).encode()

    def server(self, method: str, url: str, headers: dict[str, str], body: bytes | None) -> tuple[int, bytes]:
        assert (method, url, headers.get("Authorization")) == ("POST", "http://clankjob:8080/api/v1/cases",
                                                               "Bearer api-token")
        if 201 != self.server_status:
            return self.server_status, json.dumps({"error": {"message": "`title` must not be empty"}}).encode()
        case: Json = {"id": f"case{len(self.cases) + 1}", **json.loads(body or b"{}")}
        self.cases.append(case)
        return 201, json.dumps(case).encode()


class IntakeTests(unittest.TestCase):

    def make(self, fakes: Fakes, public_url: str = "https://clank.example") -> Intake:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        settings = Settings("discord-token", CHANNEL, frozenset({JOE}), "http://clankjob:8080", "api-token",
                            public_url, 20, Path(directory.name) / "state.json")
        return Intake(settings, Discord("discord-token", fakes.discord), Server(settings.server_url, "api-token",
                                                                                fakes.server))

    def test_the_first_poll_starts_from_now_not_from_history(self) -> None:
        fakes = Fakes()
        fakes.messages = [message(1, f"<@{BOT}> old request")]
        bot = self.make(fakes)

        self.assertEqual(0, bot.poll())
        fakes.messages.append(message(2, f"<@{BOT}> Get 3 quotes for a 200A panel\nBefore November."))
        self.assertEqual(1, bot.poll())

        self.assertEqual([{"id": "case1", "title": "Get 3 quotes for a 200A panel",
                           "goal": "Get 3 quotes for a 200A panel\nBefore November.", "owner": "Joe"}], fakes.cases)
        reply = fakes.replies[0]
        self.assertEqual("Started **Get 3 quotes for a 200A panel**: <https://clank.example/#/cases/case1>",
                         reply["content"])
        self.assertEqual({"message_id": "2", "fail_if_not_exists": False}, reply["message_reference"])
        self.assertEqual([], reply["allowed_mentions"]["parse"])  # pyright: ignore[reportIndexIssue]
        self.assertEqual(0, bot.poll(), "a message starts one case only")

    def test_only_mentions_from_allowed_people_count(self) -> None:
        fakes = Fakes()
        bot = self.make(fakes)
        bot.poll()
        fakes.messages = [message(10, "no mention", mention=False), message(11, f"<@{BOT}> hi", author="999"),
                          message(12, f"<@{BOT}> from a bot", bot=True), message(13, f"<@!{BOT}>   ")]

        self.assertEqual(0, bot.poll())

        self.assertEqual([], fakes.cases)
        self.assertEqual(["Tell me what to do after the mention, and I will start a case for it."],
                         [reply["content"] for reply in fakes.replies])

    def test_a_mention_of_the_bots_role_counts_as_one_of_the_bot(self) -> None:
        fakes = Fakes()
        bot = self.make(fakes)
        bot.poll()
        fakes.messages = [message(30, f"<@&{BOT_ROLE}> tell me a joke", mention=False, roles=[BOT_ROLE]),
                          message(31, "<@&42> another role", mention=False, roles=["42"])]

        with unittest.mock.patch.object(intake, "log") as log:
            self.assertEqual(1, bot.poll())

        self.assertEqual([{"id": "case1", "title": "tell me a joke", "goal": "tell me a joke", "owner": "Joe"}],
                         fakes.cases)
        log.assert_any_call("ignored message 31: it mentions a role, not the bot")

    def test_a_role_mention_whose_text_discord_hides_is_explained(self) -> None:
        fakes = Fakes()
        bot = self.make(fakes)
        bot.poll()
        fakes.messages = [message(40, "", mention=False, roles=[BOT_ROLE])]

        self.assertEqual(0, bot.poll())

        self.assertEqual([], fakes.cases)
        self.assertIn("Mention me directly", str(fakes.replies[-1]["content"]))

    def test_without_a_role_for_the_bot_only_its_own_mentions_count(self) -> None:
        fakes = Fakes()
        fakes.roles = [{"id": "1", "name": "@everyone"}]
        bot = self.make(fakes)
        bot.poll()
        fakes.messages = [message(50, "<@&777> hi", mention=False, roles=["777"]), message(51, f"<@{BOT}> hi")]

        self.assertEqual(1, bot.poll())
        self.assertEqual("hi", fakes.cases[0]["goal"])

    def test_a_mention_from_someone_not_allowed_is_logged(self) -> None:
        fakes = Fakes()
        bot = self.make(fakes)
        bot.poll()
        fakes.messages = [message(60, f"<@{BOT}> hi", author="999")]

        with unittest.mock.patch.object(intake, "log") as log:
            self.assertEqual(0, bot.poll())

        log.assert_called_once_with("ignored message 60: author 999 is not in DISCORD_INTAKE_ALLOWED_USERS")

    def test_a_refused_case_is_explained_and_a_server_outage_is_retried(self) -> None:
        fakes = Fakes()
        bot = self.make(fakes, public_url="")
        bot.poll()
        fakes.messages = [message(20, f"<@{BOT}> first")]
        fakes.server_status = 400
        bot.poll()
        self.assertIn("I could not start a case: the server refused the case: HTTP 400", str(fakes.replies[-1]))

        fakes.messages.append(message(21, f"<@{BOT}> second"))
        fakes.server_status = 503
        with self.assertRaises(IntakeError) as caught:
            bot.poll()
        self.assertTrue(caught.exception.retryable)
        fakes.server_status = 201
        self.assertEqual(1, bot.poll(), "the message is tried again once the server is back")
        self.assertEqual("Started **second**: case `case1`", fakes.replies[-1]["content"])

    def test_long_titles_and_attachments(self) -> None:
        attachment: Json = {"filename": "panel.jpg", "url": "https://cdn.discordapp.com/panel.jpg"}
        title, goal = intake.request_of(message(1, f"<@{BOT}> {'x' * 100}", attachments=[attachment]), BOT)
        self.assertEqual(intake.MAX_TITLE, len(title))
        self.assertTrue(title.endswith("…"))
        self.assertIn("- panel.jpg: https://cdn.discordapp.com/panel.jpg", goal)


class SettingsTests(unittest.TestCase):
    ENV: ClassVar[dict[str, str]] = {"DISCORD_INTAKE_TOKEN": "t", "DISCORD_INTAKE_CHANNEL_ID": "555", "DISCORD_INTAKE_ALLOWED_USERS": "1, 2",
           "CLANKJOB_URL": "http://clankjob:8080/"}

    def test_settings_come_from_the_environment(self) -> None:
        settings = Settings.from_env(self.ENV)
        self.assertEqual((frozenset({"1", "2"}), "http://clankjob:8080", 20.0, ""),
                         (settings.allowed_users, settings.server_url, settings.poll_seconds, settings.server_token))
        self.assertEqual(5.0, Settings.from_env({**self.ENV, "DISCORD_INTAKE_POLL": "1"}).poll_seconds)
        self.assertIsNone(settings.human_channels)
        self.assertEqual(("discord_joe", "discord_bob"), Settings.from_env(
            {**self.ENV, "DISCORD_INTAKE_HUMAN_CHANNELS": " discord_joe, discord_bob "}).human_channels)

    def test_cases_ask_on_the_configured_channels(self) -> None:
        sent: list[Json] = []

        def server(method: str, url: str, headers: dict[str, str], body: bytes | None) -> tuple[int, bytes]:
            sent.append(json.loads(body or b"{}"))
            return 201, json.dumps({"id": "case1"}).encode()

        client = Server("http://clankjob:8080", "", server)
        _ = client.create_case("t", "g", None)
        _ = client.create_case("t", "g", None, ("discord_joe",))
        self.assertEqual([None, ["discord_joe"]], [body.get("human_channels") for body in sent])

    def test_bad_settings_are_named(self) -> None:
        for env, error in (({**self.ENV, "DISCORD_INTAKE_TOKEN": ""}, "DISCORD_INTAKE_TOKEN is not set"),
                           ({**self.ENV, "DISCORD_INTAKE_CHANNEL_ID": "#general"}, "not a Discord id"),
                           ({**self.ENV, "CLANKJOB_URL": "clankjob:8080"}, "must start with http")):
            with self.assertRaisesRegex(IntakeError, error):
                Settings.from_env(env)


if __name__ == "__main__":
    unittest.main()
