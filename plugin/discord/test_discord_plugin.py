#!/usr/bin/env python3
"""Tests for the Discord plugin against a fake Discord. Run: python3 -m unittest -v test_discord_plugin.py"""

import io
import json
import unittest

import discord_plugin as plugin
from discord_plugin import APPROVE, REJECT, Json, Plugin, PluginError

CHANNEL = "100"
OWNER = "200"
STRANGER = "300"
BOT = "999"

CONFIG: Json = {"bot_token": "secret-token", "channel_id": CHANNEL, "allowed_responders": [OWNER]}


class FakeDiscord:
    """Records every call; answers from `routes`, keyed by (method, path)."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str, Json | None]] = []
        self.routes: dict[tuple[str, str], object] = {}
        self.next_id = 1000

    def call(self, method: str, path: str, payload: Json | None = None) -> object:
        self.calls.append((method, path, payload))
        answer = self.routes.get((method, path))
        if isinstance(answer, PluginError):
            raise answer
        if answer is not None:
            return answer
        if "POST" == method:
            self.next_id += 1
            return {"id": str(self.next_id), "channel_id": CHANNEL}
        return None

    def paths(self, method: str) -> list[str]:
        return [path for called, path, _ in self.calls if called == method]


def dig(value: object, *path: str | int) -> object:
    """Reach into nested JSON; a missing step gives None instead of an exception."""
    for step in path:
        if isinstance(step, str) and isinstance(value, dict):
            value = value.get(step)
        elif isinstance(step, int) and isinstance(value, list) and step < len(value):
            value = value[step]
        else:
            return None
    return value


def make_plugin(fake: FakeDiscord) -> Plugin:
    return Plugin(client_for=lambda _token: fake)


def params(**extra: object) -> Json:
    return {"instance": "discord_joe", "config": dict(CONFIG), **extra}


def user_message(message_id: str, author: str, content: str, bot: bool = False) -> Json:
    return {"id": message_id, "author": {"id": author, "bot": bot}, "content": content, "attachments": []}


class DeliverTests(unittest.TestCase):

    def test_question_is_posted_with_a_thread_and_can_only_mention_the_owner(self) -> None:
        # Arrange
        fake = FakeDiscord()
        request = {"kind": "question", "case_title": "Electrician quote",
                   "text": "Photo of the panel? @everyone", "case_url": "https://cj.example/#/cases/1"}

        # Act
        result = make_plugin(fake).handle("deliver", params(request=request))

        # Assert
        _, _, payload = fake.calls[0]
        assert payload is not None
        self.assertEqual(payload["allowed_mentions"], {"parse": [], "users": [OWNER]})
        content = str(payload["content"])
        self.assertTrue(content.startswith(f"<@{OWNER}>"))
        self.assertIn("Photo of the panel?", content)
        self.assertIn("<https://cj.example/#/cases/1>", content)
        self.assertEqual(fake.paths("POST")[1], f"/channels/{CHANNEL}/messages/1001/threads")
        self.assertEqual(result["delivery"], {"kind": "question", "channel_id": CHANNEL,
                                              "message_id": "1001", "thread_id": "1002"})

    def test_question_is_taken_down_when_no_thread_can_be_opened(self) -> None:
        fake = FakeDiscord()
        fake.routes[("POST", f"/channels/{CHANNEL}/messages/1001/threads")] = PluginError("403", status=403)

        with self.assertRaises(PluginError):
            make_plugin(fake).handle("deliver", params(request={"kind": "question", "text": "?"}))

        self.assertEqual(fake.paths("DELETE"), [f"/channels/{CHANNEL}/messages/1001"])

    def test_approval_is_posted_with_both_reactions(self) -> None:
        fake = FakeDiscord()
        request = {"kind": "approval", "case_title": "Quote", "text": "Send an email to Bob",
                   "details": {"to": ["bob@sparky.ca"], "subject": "Quote"}}

        result = make_plugin(fake).handle("deliver", params(request=request))

        reactions = fake.paths("PUT")
        self.assertEqual(len(reactions), 2)
        self.assertTrue(reactions[0].endswith("/reactions/%E2%9C%85/@me"))
        self.assertIn("bob@sparky.ca", str(fake.calls[0][2]))
        self.assertEqual(result["delivery"], {"kind": "approval", "channel_id": CHANNEL,
                                              "message_id": "1001", "thread_id": None})

    def test_mentions_can_be_turned_off(self) -> None:
        fake = FakeDiscord()
        call = params(request={"kind": "question", "text": "?"})
        call["config"] = {**CONFIG, "mention": False}

        make_plugin(fake).handle("deliver", call)

        payload = fake.calls[0][2]
        assert payload is not None
        self.assertEqual(payload["allowed_mentions"], {"parse": [], "users": []})
        self.assertNotIn("<@", str(payload["content"]))


class PollTests(unittest.TestCase):

    def question(self) -> Json:
        return {"request_id": "req-1", "delivery": {"kind": "question", "channel_id": CHANNEL,
                                                    "message_id": "500", "thread_id": "500"}}

    def test_first_reply_from_the_owner_answers_and_advances_the_cursor(self) -> None:
        # Arrange: newest first, as Discord returns them.
        fake = FakeDiscord()
        answer = user_message("503", OWNER, "Here it is")
        answer["attachments"] = [{"url": "https://cdn/p.jpg", "filename": "p.jpg",
                                  "content_type": "image/jpeg", "size": 10}]
        fake.routes[("GET", "/channels/500/messages?after=500&limit=100")] = [
            user_message("504", OWNER, "and another"),
            answer,
            user_message("502", STRANGER, "I'm not the owner"),
            user_message("501", BOT, "bot chatter", bot=True),
        ]

        # Act
        result = make_plugin(fake).handle("poll", params(open=[self.question()]))

        # Assert
        self.assertEqual(result["replies"], [{"external_id": "503", "responder": OWNER, "text": "Here it is",
                                              "attachments": [{"url": "https://cdn/p.jpg", "filename": "p.jpg",
                                                               "content_type": "image/jpeg", "size": 10}],
                                              "request_id": "req-1"}])
        self.assertEqual(result["cursor"], {"500": "503"})

    def test_cursor_from_the_last_poll_is_used(self) -> None:
        fake = FakeDiscord()
        fake.routes[("GET", "/channels/500/messages?after=777&limit=100")] = []

        result = make_plugin(fake).handle("poll", params(open=[self.question()], cursor={"500": "777"}))

        self.assertEqual(result["replies"], [])
        self.assertEqual(result["cursor"], {"500": "777"})

    def test_empty_reply_warns_about_the_message_content_intent(self) -> None:
        fake = FakeDiscord()
        fake.routes[("GET", "/channels/500/messages?after=500&limit=100")] = [user_message("501", OWNER, "")]

        result = make_plugin(fake).handle("poll", params(open=[self.question()]))

        self.assertEqual(result["replies"], [])
        self.assertIn("Message Content", str(result["warnings"]))

    def test_one_failing_request_does_not_hide_the_others(self) -> None:
        # Arrange
        fake = FakeDiscord()
        broken = {"request_id": "req-2", "delivery": {"kind": "question", "channel_id": CHANNEL,
                                                      "message_id": "600", "thread_id": "600"}}
        fake.routes[("GET", "/channels/600/messages?after=650&limit=100")] = PluginError("boom", retryable=True)
        fake.routes[("GET", "/channels/500/messages?after=500&limit=100")] = [user_message("501", OWNER, "yes")]

        # Act
        result = make_plugin(fake).handle("poll", params(open=[broken, self.question()], cursor={"600": "650"}))

        # Assert
        self.assertEqual(dig(result, "replies", 0, "request_id"), "req-1")
        self.assertIsNone(dig(result, "replies", 1))
        self.assertEqual(result["cursor"], {"600": "650", "500": "501"})
        self.assertIn("req-2", str(result["warnings"]))

    def test_approval_reaction_from_the_owner_decides(self) -> None:
        # Arrange
        fake = FakeDiscord()
        approval = {"request_id": "req-3", "delivery": {"kind": "approval", "channel_id": CHANNEL,
                                                        "message_id": "700", "thread_id": None}}
        fake.routes[("GET", f"/channels/{CHANNEL}/messages/700")] = {
            "id": "700",
            "reactions": [{"emoji": {"name": APPROVE}, "count": 2, "me": True},
                          {"emoji": {"name": REJECT}, "count": 1, "me": True}],
        }
        fake.routes[("GET", f"/channels/{CHANNEL}/messages/700/reactions/%E2%9C%85?limit=100")] = [
            {"id": BOT}, {"id": OWNER}]

        # Act
        result = make_plugin(fake).handle("poll", params(open=[approval]))

        # Assert
        self.assertEqual(result["replies"], [{"external_id": "700:approve", "responder": OWNER,
                                              "decision": "approve", "request_id": "req-3"}])

    def test_approval_reaction_from_a_stranger_is_ignored(self) -> None:
        fake = FakeDiscord()
        approval = {"request_id": "req-3", "delivery": {"kind": "approval", "channel_id": CHANNEL,
                                                        "message_id": "700", "thread_id": None}}
        fake.routes[("GET", f"/channels/{CHANNEL}/messages/700")] = {
            "id": "700", "reactions": [{"emoji": {"name": REJECT}, "count": 2, "me": True}]}
        fake.routes[("GET", f"/channels/{CHANNEL}/messages/700/reactions/%E2%9D%8C?limit=100")] = [
            {"id": BOT}, {"id": STRANGER}]

        result = make_plugin(fake).handle("poll", params(open=[approval]))

        self.assertEqual(result["replies"], [])


class ResolvedAndNotifyTests(unittest.TestCase):

    def test_answered_elsewhere_edits_the_message_and_closes_the_thread(self) -> None:
        # Arrange
        fake = FakeDiscord()
        fake.routes[("GET", f"/channels/{CHANNEL}/messages/500")] = {"id": "500", "content": "Photo?"}
        fake.routes[("PATCH", f"/channels/{CHANNEL}/messages/500")] = {"id": "500"}
        call = params(delivery={"kind": "question", "channel_id": CHANNEL, "message_id": "500", "thread_id": "500"},
                      outcome={"status": "answered", "via": "web", "responder": "joe"})

        # Act
        make_plugin(fake).handle("on_resolved", call)

        # Assert
        edit = next(payload for method, _, payload in fake.calls if "PATCH" == method)
        assert edit is not None
        self.assertEqual(edit["content"], "Photo?\n-# **Answered via web by joe**")
        self.assertEqual(edit["allowed_mentions"], {"parse": []})
        self.assertIn("/channels/500", fake.paths("PATCH"))

    def test_a_decided_approval_says_approved_or_rejected(self) -> None:
        # Arrange
        fake = FakeDiscord()
        path = f"/channels/{CHANNEL}/messages/500"
        fake.routes[("GET", path)] = {"id": "500", "content": "wants your approval"}
        fake.routes[("PATCH", path)] = {"id": "500"}
        delivery = {"kind": "approval", "channel_id": CHANNEL, "message_id": "500", "thread_id": None}

        # Act
        make_plugin(fake).handle("on_resolved", params(delivery=delivery, outcome={
            "status": "answered", "decision": "reject", "via": "web"}))

        # Assert
        edits = [payload for method, called, payload in fake.calls if method == "PATCH" and called == path]
        self.assertIn("Rejected via web", str(dig(edits[0], "content")))

    def test_notification_does_not_ping(self) -> None:
        fake = FakeDiscord()

        result = make_plugin(fake).handle("notify", params(notification={
            "kind": "completed", "case_title": "Quote", "text": "Bob quoted $1450"}))

        payload = fake.calls[0][2]
        assert payload is not None
        self.assertEqual(payload["allowed_mentions"], {"parse": [], "users": []})
        self.assertEqual(dig(result, "delivery", "kind"), "notification")


class ConfigTests(unittest.TestCase):

    def test_bad_configurations_are_rejected(self) -> None:
        for bad in ({**CONFIG, "bot_token": ""},
                    {**CONFIG, "channel_id": "general"},
                    {**CONFIG, "allowed_responders": []},
                    {**CONFIG, "allowed_responders": ["joe"]}):
            with self.subTest(bad=bad), self.assertRaises(plugin.InvalidParamsError):
                plugin.instance_from({"instance": "x", "config": bad})

    def test_integer_ids_from_toml_are_accepted(self) -> None:
        instance = plugin.instance_from({"instance": "x", "config": {**CONFIG, "channel_id": 100,
                                                                     "allowed_responders": [200]}})

        self.assertEqual((instance.channel_id, instance.allowed_responders), ("100", ("200",)))


GUILD = "500"
BOT_ROLE = "600"
# View Channel, Send Messages, Read Message History, Add Reactions, Create Public
# Threads, Send Messages in Threads.
BOT_NEEDS = (1 << 10) | (1 << 11) | (1 << 16) | (1 << 6) | (1 << 35) | (1 << 38)


def healthy_discord() -> FakeDiscord:
    """A server where everything is set up right, except the optional Manage Threads."""
    fake = FakeDiscord()
    fake.routes[("GET", "/users/@me")] = {"id": BOT, "username": "clankbot"}
    fake.routes[("GET", "/applications/@me")] = {"flags": 1 << 19, "bot_public": False}
    fake.routes[("GET", f"/channels/{CHANNEL}")] = {
        "id": CHANNEL, "name": "general", "type": 0, "guild_id": GUILD, "permission_overwrites": []}
    fake.routes[("GET", f"/guilds/{GUILD}")] = {"id": GUILD, "owner_id": "1", "roles": [
        {"id": GUILD, "permissions": str((1 << 10) | (1 << 38))},
        {"id": BOT_ROLE, "permissions": str(BOT_NEEDS)}]}
    fake.routes[("GET", f"/guilds/{GUILD}/members/{BOT}")] = {"roles": [BOT_ROLE]}
    fake.routes[("GET", f"/guilds/{GUILD}/members/{OWNER}")] = {"roles": []}
    return fake


def failed(report: Json, key: str) -> list[str]:
    items = report.get(key)
    return [str(item).split(":")[0] for item in items] if isinstance(items, list) else []


class SetupCheckTests(unittest.TestCase):

    def test_a_correct_setup_passes_with_only_the_optional_warning(self) -> None:
        report = make_plugin(healthy_discord()).handle("validate_config", params())

        self.assertEqual((report["bot"], report["channel"], report["problems"]), ("clankbot", "general", []))
        self.assertEqual(failed(report, "warnings"), [
                         "bot can lock a question's thread once it is answered (Manage Threads)"])
        self.assertEqual(make_plugin(healthy_discord()).handle("healthcheck", params())["problems"], [])

    def test_missing_intent_permissions_and_responder_are_problems(self) -> None:
        # Arrange
        fake = healthy_discord()
        fake.routes[("GET", "/applications/@me")] = {"flags": 0, "bot_public": True}
        channel = fake.routes[("GET", f"/channels/{CHANNEL}")]
        assert isinstance(channel, dict)
        # The channel takes Create Public Threads away from the bot's role.
        channel["permission_overwrites"] = [{"id": BOT_ROLE, "type": 0, "allow": "0", "deny": str(1 << 35)}]
        fake.routes[("GET", f"/guilds/{GUILD}/members/{OWNER}")] = PluginError("404: Unknown Member", status=404)

        # Act
        report = make_plugin(fake).handle("validate_config", params())

        # Assert
        self.assertEqual(failed(report, "problems"), [
            "Message Content intent",
            "bot can open a thread per question (Create Public Threads)",
            f"responder {OWNER} is a member of the server",
        ])
        self.assertIn("bot is private (only you can add it to servers)", failed(report, "warnings"))

    def test_administrators_and_the_owner_have_every_permission(self) -> None:
        guild: Json = {"id": GUILD, "owner_id": OWNER, "roles": [
            {"id": GUILD, "permissions": "0"}, {"id": "7", "permissions": str(1 << 3)}]}
        deny_all: Json = {"permission_overwrites": [{"id": GUILD, "allow": "0", "deny": str(plugin.ALL_PERMISSIONS)}]}

        self.assertEqual(plugin.effective_permissions(OWNER, [], guild, deny_all), plugin.ALL_PERMISSIONS)
        self.assertEqual(plugin.effective_permissions(STRANGER, ["7"], guild, deny_all), plugin.ALL_PERMISSIONS)
        self.assertEqual(plugin.effective_permissions(STRANGER, [], guild, deny_all), 0)

    def test_a_forum_channel_is_a_problem_and_an_invisible_channel_an_error(self) -> None:
        fake = healthy_discord()
        channel = fake.routes[("GET", f"/channels/{CHANNEL}")]
        assert isinstance(channel, dict)
        channel["type"] = 15
        report = make_plugin(fake).handle("validate_config", params())
        fake.routes[("GET", f"/channels/{CHANNEL}")] = PluginError("403: Missing Access", status=403)

        self.assertIn("channel is a text channel (threads can be opened)", failed(report, "problems"))
        with self.assertRaises(PluginError):
            make_plugin(fake).handle("validate_config", params())


class RpcTests(unittest.TestCase):

    def run_lines(self, *lines: str) -> list[Json]:
        stdout = io.StringIO()
        plugin.serve(make_plugin(FakeDiscord()), io.StringIO("\n".join(lines) + "\n"), stdout)
        return [json.loads(line) for line in stdout.getvalue().splitlines()]

    def test_protocol_errors_and_shutdown(self) -> None:
        responses = self.run_lines(
            json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocol": 1}}),
            json.dumps({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {"protocol": 2}}),
            "{not json",
            json.dumps({"jsonrpc": "2.0", "id": 3, "method": "dance", "params": {}}),
            json.dumps({"jsonrpc": "2.0", "id": 4, "method": "shutdown", "params": {}}),
            json.dumps({"jsonrpc": "2.0", "id": 5, "method": "initialize", "params": {"protocol": 1}}),
        )

        self.assertEqual(responses[0], {"jsonrpc": "2.0", "id": 1, "result": {"protocol": 1}})
        self.assertEqual(dig(responses[1], "error", "code"), -32602)
        self.assertEqual(dig(responses[2], "error", "code"), -32700)
        self.assertEqual(dig(responses[3], "error", "code"), -32601)
        self.assertEqual(responses[4], {"jsonrpc": "2.0", "id": 4, "result": {}})
        self.assertEqual(len(responses), 5)  # nothing is read after shutdown

    def test_discord_failures_carry_the_retryable_flag(self) -> None:
        stdout = io.StringIO()
        fake = FakeDiscord()
        fake.routes[("GET", "/users/@me")] = PluginError("503", retryable=True, status=503)
        request = {"jsonrpc": "2.0", "id": 9, "method": "healthcheck", "params": params()}

        plugin.serve(make_plugin(fake), io.StringIO(json.dumps(request) + "\n"), stdout)

        error = json.loads(stdout.getvalue())["error"]
        self.assertEqual((error["code"], error["data"]["retryable"]), (-32000, True))

    def test_retry_after_is_read_from_the_429_body(self) -> None:
        self.assertEqual(plugin.retry_after(b'{"retry_after": 2.5}'), 2.5)
        self.assertEqual(plugin.retry_after(b"not json"), 1.0)


if __name__ == "__main__":
    unittest.main()
