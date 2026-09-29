#!/usr/bin/env python3
"""clankjob Discord plugin: a human channel (design sections 10.3 and 12).

The server starts this script once and keeps it running. They talk JSON-RPC 2.0
over stdin/stdout, one message per line (design section 9.8); stderr goes to the
server log. The process is stateless: every call carries the instance's
configuration, and the poll cursor travels back and forth with each poll, so the
server can restart this process at any time without losing anything.

How the owner answers, all over plain REST (no gateway, no public endpoint):

    question   posted with a thread; a reply in the thread is the answer
    approval   posted with two reactions; tapping one approves or rejects

Only the users listed in `allowed_responders` can answer, and messages can only
ever mention those users, whatever text the LLM put in the question.
"""

import http.client
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from typing import Protocol, TextIO

PROTOCOL = 1

API_BASE = "https://discord.com/api/v10"

# Discord's edge rejects requests whose User-Agent is not bot-shaped, and
# urllib's default ("Python-urllib/3.x") is one of them.
USER_AGENT = "DiscordBot (https://github.com/uintptr/clankjob, 1)"

REQUEST_TIMEOUT = 10

# A 429 says how long to wait. Short waits are slept through; a longer one is
# reported as retryable so the server backs off instead of this call hanging
# past its timeout.
MAX_RATE_LIMIT_WAIT = 5.0
RATE_LIMIT_RETRIES = 3

# Discord rejects message content longer than this.
MAX_CONTENT = 2000

APPROVE = "✅"
REJECT = "❌"

Json = dict[str, object]


class PluginError(Exception):
    """A call that failed; `retryable` tells the server whether to try again."""

    def __init__(self, message: str, retryable: bool = False, status: int | None = None) -> None:
        super().__init__(message)
        self.retryable = retryable
        self.status = status


class InvalidParamsError(PluginError):
    """The server sent parameters this plugin cannot use (JSON-RPC -32602)."""


# ---------------------------------------------------------------- JSON helpers


def as_object(value: object, what: str) -> Json:
    if not isinstance(value, dict):
        raise InvalidParamsError(f"{what} must be an object")
    return {str(key): item for key, item in value.items()}


def as_list(value: object, what: str) -> list[object]:
    if not isinstance(value, list):
        raise InvalidParamsError(f"{what} must be an array")
    return list(value)


def text_of(value: Json, key: str, what: str) -> str:
    item = value.get(key)
    if not isinstance(item, str) or "" == item.strip():
        raise InvalidParamsError(f"{what}.{key} must be a non-empty string")
    return item


def optional_text(value: Json, key: str) -> str | None:
    item = value.get(key)
    return item if isinstance(item, str) and "" != item.strip() else None


def snowflake(value: object, what: str) -> str:
    """A Discord id. TOML makes it easy to write one unquoted, so ints are accepted."""
    text = str(value).strip() if isinstance(value, (str, int)) and not isinstance(value, bool) else ""
    if not text.isdigit():
        raise InvalidParamsError(f"{what} must be a Discord id (digits), got {value!r}")
    return text


def id_of(message: Json) -> str:
    return snowflake(message.get("id"), "message id")


# ---------------------------------------------------------------- configuration


@dataclass(frozen=True)
class Instance:
    """One configured Discord instance, as sent by the server with every call."""

    name: str
    token: str
    channel_id: str
    allowed_responders: tuple[str, ...]
    mention: bool


def instance_from(params: Json) -> Instance:
    """Validate the `instance` and `config` parameters of a call."""
    name = params.get("instance")
    config = as_object(params.get("config"), "config")
    responders = tuple(snowflake(user, "allowed_responders entry")
                       for user in as_list(config.get("allowed_responders"), "config.allowed_responders"))
    if 0 == len(responders):
        raise InvalidParamsError("config.allowed_responders needs at least one Discord user id")
    mention = config.get("mention", True)
    return Instance(name=name if isinstance(name, str) else "discord",
                    token=text_of(config, "bot_token", "config"),
                    channel_id=snowflake(config.get("channel_id"), "config.channel_id"),
                    allowed_responders=responders,
                    mention=mention if isinstance(mention, bool) else True)


# ---------------------------------------------------------------- Discord REST


class DiscordApi(Protocol):
    def call(self, method: str, path: str, payload: Json | None = None) -> object: ...


def retry_after(body: bytes) -> float:
    """Seconds to wait, out of a 429 body; one second when it cannot be read."""
    try:
        parsed = json.loads(body)
    except json.JSONDecodeError:
        return 1.0
    value = parsed.get("retry_after", 1.0) if isinstance(parsed, dict) else 1.0
    return float(value) if isinstance(value, (int, float)) and not isinstance(value, bool) else 1.0


class HttpDiscord:
    """The real API, over urllib."""

    def __init__(self, token: str) -> None:
        self.token = token

    def call(self, method: str, path: str, payload: Json | None = None) -> object:
        data = None if payload is None else json.dumps(payload).encode("utf-8")
        headers = {"Authorization": f"Bot {self.token}", "User-Agent": USER_AGENT}
        if data is not None:
            headers["Content-Type"] = "application/json"
        for _ in range(RATE_LIMIT_RETRIES):
            request = urllib.request.Request(f"{API_BASE}{path}", data=data, headers=headers, method=method)
            try:
                with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT) as response:
                    body = response.read()
            except urllib.error.HTTPError as error:
                with error:
                    body = error.read()
                if 429 == error.code:
                    wait = retry_after(body)
                    if wait > MAX_RATE_LIMIT_WAIT:
                        raise PluginError(f"rate limited for {wait:.0f}s", retryable=True, status=429) from error
                    time.sleep(wait)
                    continue
                detail = body.decode("utf-8", "replace")[:300]
                raise PluginError(f"{method} {path} -> {error.code}: {detail}",
                                  retryable=500 <= error.code,
                                  status=error.code) from error
            except (OSError, http.client.HTTPException) as error:
                # URLError and socket timeouts are OSError subclasses.
                raise PluginError(f"{method} {path}: {error}", retryable=True) from error
            return None if 0 == len(body) else json.loads(body)
        raise PluginError(f"{method} {path}: rate limited {RATE_LIMIT_RETRIES} times", retryable=True, status=429)


def call_object(api: DiscordApi, method: str, path: str, payload: Json | None = None) -> Json:
    body = api.call(method, path, payload)
    if not isinstance(body, dict):
        raise PluginError(f"{method} {path}: expected an object")
    return {str(key): value for key, value in body.items()}


def call_array(api: DiscordApi, path: str) -> list[object]:
    body = api.call("GET", path)
    if not isinstance(body, list):
        raise PluginError(f"GET {path}: expected an array")
    return list(body)


# ---------------------------------------------------------------- messages


def truncate(text: str, limit: int) -> str:
    return text if len(text) <= limit else f"{text[:limit - 1]}…"


def message_payload(instance: Instance, content: str, ping: bool) -> Json:
    """A message that can only ever mention the allowed responders.

    Question text comes from the LLM, and `@everyone` in it must stay text.
    """
    mentions = " ".join(f"<@{user}>" for user in instance.allowed_responders)
    body = f"{mentions}\n{content}" if ping and instance.mention else content
    users = list(instance.allowed_responders) if ping and instance.mention else []
    return {"content": truncate(body, MAX_CONTENT), "allowed_mentions": {"parse": [], "users": users}}


def link_line(case_url: str | None) -> str:
    return f"\n<{case_url}>" if case_url is not None else ""


def question_content(title: str, question: str, case_url: str | None) -> str:
    return (f"**{truncate(title, 200)}** needs your input\n"
            f"{truncate(question, 1500)}\n"
            f"-# Reply in the thread to answer{link_line(case_url)}")


def approval_content(title: str, summary: str, details: object, case_url: str | None) -> str:
    block = ""
    if details is not None:
        rendered = details if isinstance(details, str) else json.dumps(details, indent=2)
        block = f"\n```\n{truncate(rendered.replace('```', "'''"), 1000)}\n```"
    return (f"**{truncate(title, 200)}** wants your approval\n"
            f"{truncate(summary, 500)}{block}\n"
            f"-# React {APPROVE} to approve or {REJECT} to reject. To edit it first, use the web UI.{link_line(case_url)}")


def notification_content(title: str, text: str, case_url: str | None) -> str:
    return f"**{truncate(title, 200)}**\n{truncate(text, 1500)}{link_line(case_url)}"


# ---------------------------------------------------------------- the plugin


@dataclass(frozen=True)
class Delivery:
    """What the server stores for a posted request and hands back in later calls."""

    kind: str
    channel_id: str
    message_id: str
    thread_id: str | None

    def to_json(self) -> Json:
        return {"kind": self.kind, "channel_id": self.channel_id,
                "message_id": self.message_id, "thread_id": self.thread_id}


def delivery_from(value: object) -> Delivery:
    delivery = as_object(value, "delivery")
    kind = text_of(delivery, "kind", "delivery")
    thread = delivery.get("thread_id")
    return Delivery(kind=kind,
                    channel_id=snowflake(delivery.get("channel_id"), "delivery.channel_id"),
                    message_id=snowflake(delivery.get("message_id"), "delivery.message_id"),
                    thread_id=None if thread is None else snowflake(thread, "delivery.thread_id"))


def attachments_of(message: Json) -> list[Json]:
    """Attachment links. Discord CDN links expire, so the server downloads them at once."""
    found: list[Json] = []
    raw_attachments = message.get("attachments")
    for raw in raw_attachments if isinstance(raw_attachments, list) else []:
        if isinstance(raw, dict):
            found.append({"url": raw.get("url"), "filename": raw.get("filename"),
                          "content_type": raw.get("content_type"), "size": raw.get("size")})
    return found


class Plugin:
    """Answers the server's calls. `client_for` builds the Discord client for a token."""

    def __init__(self, client_for: Callable[[str], DiscordApi] = HttpDiscord) -> None:
        self.client_for = client_for

    def handle(self, method: str, params: Json) -> Json:
        handlers: dict[str, Callable[[Json], Json]] = {
            "initialize": self.initialize,
            "validate_config": self.check,
            "healthcheck": self.check,
            "deliver": self.deliver,
            "poll": self.poll,
            "on_resolved": self.on_resolved,
            "notify": self.notify,
            "shutdown": lambda _params: {},
        }
        handler = handlers.get(method)
        if handler is None:
            raise KeyError(method)
        return handler(params)

    def initialize(self, params: Json) -> Json:
        protocol = params.get("protocol")
        if PROTOCOL != protocol:
            raise InvalidParamsError(f"this plugin speaks protocol {PROTOCOL}, the server asked for {protocol!r}")
        return {"protocol": PROTOCOL}

    def check(self, params: Json) -> Json:
        """Validate the configuration against Discord: the token works and the channel is visible."""
        instance = instance_from(params)
        api = self.client_for(instance.token)
        bot = call_object(api, "GET", "/users/@me")
        channel = call_object(api, "GET", f"/channels/{instance.channel_id}")
        return {"bot": bot.get("username"), "channel": channel.get("name")}

    def deliver(self, params: Json) -> Json:
        """Post a question (with a thread) or an approval (with two reactions)."""
        instance = instance_from(params)
        request = as_object(params.get("request"), "request")
        kind = text_of(request, "kind", "request")
        title = optional_text(request, "case_title") or "A case"
        text = text_of(request, "text", "request")
        case_url = optional_text(request, "case_url")
        api = self.client_for(instance.token)
        messages = f"/channels/{instance.channel_id}/messages"
        if "question" == kind:
            posted = call_object(api, "POST", messages,
                                 message_payload(instance, question_content(title, text, case_url), ping=True))
            message_id = id_of(posted)
            try:
                thread = call_object(api, "POST", f"{messages}/{message_id}/threads",
                                     {"name": truncate(title, 100), "auto_archive_duration": 1440})
            except PluginError as error:
                # Without a thread there is nothing to correlate a reply with, so
                # the question is taken back down and the failure reported.
                try:
                    api.call("DELETE", f"{messages}/{message_id}")
                except PluginError:
                    pass
                raise PluginError(f"cannot open a thread (give the bot Create Public Threads): {error}",
                                  retryable=error.retryable) from error
            delivery = Delivery("question", instance.channel_id, message_id, id_of(thread))
        elif "approval" == kind:
            content = approval_content(title, text, request.get("details"), case_url)
            posted = call_object(api, "POST", messages, message_payload(instance, content, ping=True))
            message_id = id_of(posted)
            for emoji in (APPROVE, REJECT):
                quoted = urllib.parse.quote(emoji, safe="")
                api.call("PUT", f"{messages}/{message_id}/reactions/{quoted}/@me")
            delivery = Delivery("approval", instance.channel_id, message_id, None)
        else:
            raise InvalidParamsError(f"request.kind must be question or approval, got {kind!r}")
        return {"delivery": delivery.to_json()}

    def poll(self, params: Json) -> Json:
        """Look for answers to the open requests the server lists.

        One failing request does not hide the others' answers: its error is
        reported in `warnings` and its cursor is kept for the next poll.
        """
        instance = instance_from(params)
        api = self.client_for(instance.token)
        cursor_in = params.get("cursor")
        cursor = {str(k): str(v) for k, v in cursor_in.items()} if isinstance(cursor_in, dict) else {}
        next_cursor: dict[str, str] = {}
        replies: list[Json] = []
        warnings: list[str] = []
        for raw in as_list(params.get("open", []), "open"):
            item = as_object(raw, "open entry")
            request_id = text_of(item, "request_id", "open entry")
            delivery = delivery_from(item.get("delivery"))
            try:
                if "question" == delivery.kind and delivery.thread_id is not None:
                    after = cursor.get(delivery.thread_id, delivery.thread_id)
                    reply, after = self.thread_reply(api, instance, delivery.thread_id, after, warnings)
                    next_cursor[delivery.thread_id] = after
                elif "approval" == delivery.kind:
                    reply = self.approval_decision(api, instance, delivery)
                else:
                    reply = None
            except PluginError as error:
                warnings.append(f"{request_id}: {error}")
                if delivery.thread_id is not None and delivery.thread_id in cursor:
                    next_cursor[delivery.thread_id] = cursor[delivery.thread_id]
                continue
            if reply is not None:
                reply["request_id"] = request_id
                replies.append(reply)
        return {"replies": replies, "cursor": next_cursor, "warnings": warnings}

    def thread_reply(self, api: DiscordApi, instance: Instance, thread_id: str,
                     after: str, warnings: list[str]) -> tuple[Json | None, str]:
        """The first reply from an allowed responder in a question's thread."""
        fetched = call_array(api, f"/channels/{thread_id}/messages?after={after}&limit=100")
        # Newest first from the API; walk oldest first so the first answer wins.
        for raw in reversed(fetched):
            if not isinstance(raw, dict):
                continue
            message = {str(key): value for key, value in raw.items()}
            after = id_of(message)
            author = message.get("author")
            if not isinstance(author, dict) or author.get("bot") or author.get("id") not in instance.allowed_responders:
                continue
            content = message.get("content")
            text = content.strip() if isinstance(content, str) else ""
            attachments = attachments_of(message)
            if "" == text and 0 == len(attachments):
                warnings.append("a reply had no readable text: enable the Message Content intent for the bot")
                continue
            return {"external_id": after, "responder": str(author.get("id")),
                    "text": text, "attachments": attachments}, after
        return None, after

    def approval_decision(self, api: DiscordApi, instance: Instance, delivery: Delivery) -> Json | None:
        """Approve or reject, if an allowed responder tapped one of the two reactions."""
        path = f"/channels/{delivery.channel_id}/messages/{delivery.message_id}"
        message = call_object(api, "GET", path)
        counts: dict[str, int] = {}
        raw_reactions = message.get("reactions")
        for reaction in raw_reactions if isinstance(raw_reactions, list) else []:
            if isinstance(reaction, dict):
                emoji = reaction.get("emoji")
                name = emoji.get("name") if isinstance(emoji, dict) else None
                count = reaction.get("count")
                if isinstance(name, str) and isinstance(count, int):
                    # Our own seed counts as one when `me` is set.
                    counts[name] = count - (1 if reaction.get("me") else 0)
        for emoji, decision in ((APPROVE, "approve"), (REJECT, "reject")):
            if counts.get(emoji, 0) <= 0:
                continue
            quoted = urllib.parse.quote(emoji, safe="")
            for user in call_array(api, f"{path}/reactions/{quoted}?limit=100"):
                if isinstance(user, dict) and user.get("id") in instance.allowed_responders:
                    return {"external_id": f"{delivery.message_id}:{decision}",
                            "responder": str(user.get("id")), "decision": decision}
        return None

    def on_resolved(self, params: Json) -> Json:
        """Mark a delivered request as settled, so no stale prompt is left in the channel."""
        instance = instance_from(params)
        delivery = delivery_from(params.get("delivery"))
        outcome = as_object(params.get("outcome"), "outcome")
        status = text_of(outcome, "status", "outcome")
        via = optional_text(outcome, "via")
        responder = optional_text(outcome, "responder")
        line = {
            "answered": f"Answered via {via or 'another channel'}{f' by {responder}' if responder else ''}",
            "superseded": "No longer needed",
            "cancelled": "Case cancelled",
        }.get(status)
        if line is None:
            raise InvalidParamsError(f"outcome.status must be answered, superseded or cancelled, got {status!r}")
        api = self.client_for(instance.token)
        path = f"/channels/{delivery.channel_id}/messages/{delivery.message_id}"
        message = call_object(api, "GET", path)
        content = message.get("content")
        previous = content if isinstance(content, str) else ""
        updated = f"{truncate(previous, MAX_CONTENT - len(line) - 8)}\n-# **{line}**"
        # Editing never pings anyone again, whatever the content says.
        call_object(api, "PATCH", path, {"content": updated, "allowed_mentions": {"parse": []}})
        if delivery.thread_id is not None:
            try:
                api.call("PATCH", f"/channels/{delivery.thread_id}", {"archived": True, "locked": True})
            except PluginError as error:
                print(f"discord: could not close thread {delivery.thread_id}: {error}", file=sys.stderr)
        return {}

    def notify(self, params: Json) -> Json:
        """Post an informational message (case completed, failed, budget exceeded)."""
        instance = instance_from(params)
        notification = as_object(params.get("notification"), "notification")
        content = notification_content(optional_text(notification, "case_title") or "A case",
                                       text_of(notification, "text", "notification"),
                                       optional_text(notification, "case_url"))
        api = self.client_for(instance.token)
        posted = call_object(api, "POST", f"/channels/{instance.channel_id}/messages",
                             message_payload(instance, content, ping=False))
        return {"delivery": Delivery("notification", instance.channel_id, id_of(posted), None).to_json()}


# ---------------------------------------------------------------- JSON-RPC loop


def rpc_error(request_id: object, code: int, message: str, retryable: bool = False) -> Json:
    return {"jsonrpc": "2.0", "id": request_id,
            "error": {"code": code, "message": message, "data": {"retryable": retryable}}}


def handle_line(plugin: Plugin, line: str) -> tuple[Json | None, bool]:
    """Answer one request line. Returns the response (None for a notification) and whether to stop."""
    try:
        message = json.loads(line)
    except json.JSONDecodeError as error:
        return rpc_error(None, -32700, f"invalid JSON: {error}"), False
    if not isinstance(message, dict) or not isinstance(message.get("method"), str):
        return rpc_error(None, -32600, "not a JSON-RPC request"), False
    request_id = message.get("id")
    method = str(message["method"])
    raw_params = message.get("params", {})
    try:
        result = plugin.handle(method, as_object(raw_params, "params"))
    except KeyError:
        return rpc_error(request_id, -32601, f"unknown method {method}"), False
    except InvalidParamsError as error:
        return rpc_error(request_id, -32602, str(error)), False
    except PluginError as error:
        return rpc_error(request_id, -32000, str(error), retryable=error.retryable), False
    stop = "shutdown" == method
    if request_id is None:
        return None, stop
    return {"jsonrpc": "2.0", "id": request_id, "result": result}, stop


def serve(plugin: Plugin, stdin: TextIO, stdout: TextIO) -> None:
    for line in stdin:
        if "" == line.strip():
            continue
        response, stop = handle_line(plugin, line)
        if response is not None:
            stdout.write(json.dumps(response) + "\n")
            stdout.flush()
        if stop:
            return


if __name__ == "__main__":
    serve(Plugin(), sys.stdin, sys.stdout)
