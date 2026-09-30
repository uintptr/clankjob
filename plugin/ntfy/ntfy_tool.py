#!/usr/bin/env python3
"""clankjob ntfy plugin: push a notification to the owner's phone or desktop through ntfy.

A command plugin (design section 9.9): the tool call runs this script, which publishes one
message and prints JSON. Standard library only. The server, the topic and the credentials
come from config.toml [env]; the agent chooses only what the notification says.

    send --message=TEXT [--title=T] [--priority=P] [--tags=a,b] [--click=URL] [--markdown]

Environment:
    NTFY_URL        the ntfy server, e.g. https://ntfy.sh or a self-hosted one
    NTFY_TOPIC      the topic to publish to
    NTFY_TOKEN      an access token (tk_...), or
    NTFY_USERNAME and NTFY_PASSWORD for basic authentication; neither for an open server
"""

import argparse
import base64
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass

Json = dict[str, object]

TIMEOUT = 30
USER_AGENT = "clankjob-ntfy/0.1"
# ntfy turns a longer message into an attachment, which not every server allows.
MAX_MESSAGE_BYTES = 4096
MAX_TITLE = 250
MAX_TAGS = 10
PRIORITIES = {"min": 1, "low": 2, "default": 3, "high": 4, "urgent": 5}
TOPIC = re.compile(r"^[-_A-Za-z0-9]{1,64}$")


class ToolError(Exception):
    """A failure reported to the LLM (printed to stderr, exit status 1)."""


@dataclass(frozen=True)
class Server:
    """An ntfy server, its topic and how to authenticate to it."""

    url: str
    topic: str
    token: str = ""
    username: str = ""
    password: str = ""

    @classmethod
    def from_env(cls, env: dict[str, str]) -> "Server":
        url = env.get("NTFY_URL", "").strip().rstrip("/")
        topic = env.get("NTFY_TOPIC", "").strip()
        if not url:
            raise ToolError("NTFY_URL is not set: copy config.example.toml to config.toml and set the server")
        if urllib.parse.urlsplit(url).scheme not in ("http", "https") or not urllib.parse.urlsplit(url).netloc:
            raise ToolError(f"NTFY_URL must be an http(s) URL like https://ntfy.sh, got {url!r}")
        if not TOPIC.match(topic):
            raise ToolError("NTFY_TOPIC must be 1 to 64 letters, digits, '-' or '_'")
        server = cls(url, topic, env.get("NTFY_TOKEN", "").strip(), env.get("NTFY_USERNAME", "").strip(),
                     env.get("NTFY_PASSWORD", "").strip())
        if server.token and (server.username or server.password):
            raise ToolError("set NTFY_TOKEN or NTFY_USERNAME and NTFY_PASSWORD, not both")
        if bool(server.username) != bool(server.password):
            raise ToolError("NTFY_USERNAME and NTFY_PASSWORD go together")
        return server

    @property
    def auth(self) -> str:
        """How it authenticates, for messages: never the credentials themselves."""
        if self.token:
            return "access token"
        return f"user {self.username}" if self.username else "no login"

    def headers(self) -> dict[str, str]:
        headers = {"User-Agent": USER_AGENT}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        elif self.username:
            pair = f"{self.username}:{self.password}".encode()
            headers["Authorization"] = f"Basic {base64.b64encode(pair).decode()}"
        return headers

    def request(self, path: str, body: Json | None = None) -> Json:
        """GET `path`, or POST `body` as JSON; the reply as JSON."""
        headers = self.headers()
        data = None
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(f"{self.url}{path}", data=data, headers=headers,
                                         method="GET" if body is None else "POST")
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
                reply = response.read()
        except urllib.error.HTTPError as error:
            raise ToolError(f"ntfy refused the request (HTTP {error.code}): "
                            f"{self.scrub(reason(error.read()) or str(error.reason))}") from None
        except OSError as error:
            raise ToolError(f"cannot reach ntfy at {self.url}: {self.scrub(str(error))}") from None
        try:
            parsed = json.loads(reply)
        except ValueError:
            raise ToolError(f"ntfy at {self.url} sent a reply that is not JSON: is NTFY_URL an ntfy server?") \
                from None
        if not isinstance(parsed, dict):
            raise ToolError("ntfy sent an unexpected reply")
        return parsed

    def scrub(self, text: str) -> str:
        for secret in (self.token, self.password):
            if secret:
                text = text.replace(secret, "***")
        return text


def reason(body: bytes) -> str:
    """The error ntfy gives in a reply body ({"error": "...", "link": ...}), or its text."""
    try:
        parsed = json.loads(body)
    except ValueError:
        return body.decode(errors="replace").strip()[-300:]
    return str(parsed.get("error", "")) if isinstance(parsed, dict) else ""


def notification(topic: str, message: str, title: str | None, priority: str | None, tags: str | None,
                 click: str | None, markdown: bool) -> Json:
    """The JSON body ntfy publishes, after checking every field."""
    message = message.strip()
    if not message:
        raise ToolError("message is empty")
    if len(message.encode()) > MAX_MESSAGE_BYTES:
        raise ToolError(f"message is longer than {MAX_MESSAGE_BYTES} bytes: shorten it, and put the details "
                        "in the case, where the owner can read them")
    body: Json = {"topic": topic, "message": message}
    if title and title.strip():
        if len(title.strip()) > MAX_TITLE:
            raise ToolError(f"title is longer than {MAX_TITLE} characters")
        body["title"] = title.strip()
    if priority:
        if priority not in PRIORITIES:
            raise ToolError(f"priority must be one of {', '.join(PRIORITIES)}")
        body["priority"] = PRIORITIES[priority]
    if tags:
        names = [tag.strip() for tag in tags.split(",") if tag.strip()]
        if len(names) > MAX_TAGS:
            raise ToolError(f"at most {MAX_TAGS} tags")
        if names:
            body["tags"] = names
    if click and click.strip():
        parts = urllib.parse.urlsplit(click.strip())
        if parts.scheme not in ("http", "https") or not parts.netloc:
            raise ToolError("click must be an http(s) URL")
        body["click"] = click.strip()
    if markdown:
        body["markdown"] = True
    return body


def send(server: Server, body: Json) -> Json:
    reply = server.request("/", body)
    result: Json = {"sent": True, "topic": server.topic}
    for key in ("id", "time"):
        if key in reply:
            result[key] = reply[key]
    return result


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    command = sub.add_parser("send")
    command.add_argument("--message",
                         required=True)
    command.add_argument("--title")
    command.add_argument("--priority")
    command.add_argument("--tags")
    command.add_argument("--click")
    command.add_argument("--markdown",
                         action="store_true")
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        server = Server.from_env(dict(os.environ))
        body = notification(server.topic, args.message, args.title, args.priority, args.tags, args.click,
                            args.markdown)
        result = send(server, body)
    except ToolError as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
