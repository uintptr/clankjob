#!/usr/bin/env python3
"""Start clankjob cases from Discord: a message that @mentions the bot, from an allowed
user, becomes a new case, and the bot replies with a link to it.

A standalone service, standard library only. It knows the server only through its public
REST API (`POST /api/v1/cases`) and shares no code or configuration with the Discord
plugin, which keeps handling the questions of cases. It polls the channel over Discord's
REST API (no gateway, no public endpoint) and remembers the last message it handled in a
state file, so a restart neither replays old messages nor misses new ones.

Configuration, all from the environment:

    DISCORD_INTAKE_TOKEN          bot token (its own bot, or the plugin's)
    DISCORD_INTAKE_CHANNEL_ID     channel to watch
    DISCORD_INTAKE_ALLOWED_USERS  user ids whose mentions start cases, comma-separated
    CLANKJOB_URL                  the server, e.g. http://clankjob:8080
    CLANKJOB_TOKEN                an API token (unless the server requires none)
    CLANKJOB_PUBLIC_URL           optional: base of the links posted back
    DISCORD_INTAKE_POLL           optional: seconds between polls (default 20, at least 5)
    DISCORD_INTAKE_STATE          optional: state file (default /state/discord_intake.json)
    DISCORD_INTAKE_HUMAN_CHANNELS optional: chat channels its cases ask and notify on,
                                  comma-separated (e.g. discord_joe); unset: the server's
                                  default_human_channels, which is none unless set

    discord_intake.py             run until stopped
    discord_intake.py --once      one poll, then exit
    discord_intake.py --check     check the configuration and exit
"""

import argparse
import json
import os
import re
import signal
import sys
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from types import FrameType

Json = dict[str, object]
# method, url, headers, body -> status, body. Replaced by a fake in tests.
Http = Callable[[str, str, dict[str, str], bytes | None], tuple[int, bytes]]

DISCORD_API = "https://discord.com/api/v10"
USER_AGENT = "DiscordBot (https://github.com/uintptr/clankjob, 0.1) clankjob-intake"
TIMEOUT = 30
MIN_POLL = 5
MAX_TITLE = 80
MAX_REPLY = 1900
# Discord waits shorter than this are slept through; longer ones end the poll.
MAX_RATE_LIMIT_WAIT = 10.0


class IntakeError(Exception):
    """A failure to report: a bad setting, or a service that refused a call."""

    def __init__(self, message: str, retryable: bool = False) -> None:
        super().__init__(message)
        self.retryable = retryable


def http(method: str, url: str, headers: dict[str, str], body: bytes | None) -> tuple[int, bytes]:
    request = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()
    except OSError as error:
        raise IntakeError(f"cannot reach {url.split('?')[0]}: {error}", retryable=True) from None


# ---------------------------------------------------------------- settings


@dataclass(frozen=True)
class Settings:
    discord_token: str
    channel_id: str
    allowed_users: frozenset[str]
    server_url: str
    server_token: str
    public_url: str
    poll_seconds: float
    state_path: Path
    # None: the server's default.
    human_channels: tuple[str, ...] | None = None

    @classmethod
    def from_env(cls, env: dict[str, str]) -> "Settings":
        def need(name: str) -> str:
            value = env.get(name, "").strip()
            if not value:
                raise IntakeError(f"{name} is not set")
            return value

        channel = need("DISCORD_INTAKE_CHANNEL_ID")
        users = frozenset(user.strip() for user in need("DISCORD_INTAKE_ALLOWED_USERS").split(",") if user.strip())
        for snowflake in (channel, *users):
            if not snowflake.isdigit():
                raise IntakeError(f"{snowflake!r} is not a Discord id (Developer Mode on, right-click, Copy ID)")
        server = need("CLANKJOB_URL").rstrip("/")
        if not server.startswith(("http://", "https://")):
            raise IntakeError("CLANKJOB_URL must start with http:// or https://")
        try:
            poll = max(MIN_POLL, float(env.get("DISCORD_INTAKE_POLL", "20") or "20"))
        except ValueError:
            raise IntakeError("DISCORD_INTAKE_POLL must be a number of seconds") from None
        channels = env.get("DISCORD_INTAKE_HUMAN_CHANNELS", "").strip()
        return cls(need("DISCORD_INTAKE_TOKEN"), channel, users, server, env.get("CLANKJOB_TOKEN", "").strip(),
                   env.get("CLANKJOB_PUBLIC_URL", "").strip().rstrip("/"), poll,
                   Path(env.get("DISCORD_INTAKE_STATE", "") or "/state/discord_intake.json"),
                   tuple(name.strip() for name in channels.split(",") if name.strip()) if channels else None)


# ---------------------------------------------------------------- the two services


class Discord:
    def __init__(self, token: str, send: Http = http) -> None:
        self.token = token
        self.send = send

    def call(self, method: str, path: str, payload: Json | None = None) -> object:
        headers = {"Authorization": f"Bot {self.token}", "User-Agent": USER_AGENT}
        body = None
        if payload is not None:
            headers["Content-Type"] = "application/json"
            body = json.dumps(payload).encode()
        for _ in range(3):
            status, data = self.send(method, f"{DISCORD_API}{path}", headers, body)
            if 429 == status:
                wait = retry_after(data)
                if wait > MAX_RATE_LIMIT_WAIT:
                    raise IntakeError(f"Discord rate limit ({wait:.0f}s)", retryable=True)
                time.sleep(wait)
                continue
            if status >= 400:
                raise IntakeError(f"Discord {method} {path.split('?')[0]}: HTTP {status} {discord_message(data)}",
                                  retryable=status >= 500)
            return json.loads(data) if data else None
        raise IntakeError("Discord kept rate limiting", retryable=True)

    def me(self) -> Json:
        user = self.call("GET", "/users/@me")
        if not isinstance(user, dict):
            raise IntakeError("Discord sent an unexpected reply for /users/@me")
        return user

    def messages_after(self, channel_id: str, after: str | None) -> list[Json]:
        """Messages newer than `after`, oldest first; without `after`, only the newest one."""
        query = f"?after={after}&limit=50" if after else "?limit=1"
        found = self.call("GET", f"/channels/{channel_id}/messages{query}")
        messages = [message for message in found if isinstance(message, dict)] if isinstance(found, list) else []
        return sorted(messages, key=lambda message: int(str(message.get("id", "0"))))

    def reply(self, channel_id: str, message_id: str, content: str) -> None:
        self.call("POST", f"/channels/{channel_id}/messages", {
            "content": content[:MAX_REPLY],
            "message_reference": {"message_id": message_id, "fail_if_not_exists": False},
            # Case titles come from users' text: never let them ping anyone.
            "allowed_mentions": {"parse": [], "replied_user": False},
        })


def retry_after(body: bytes) -> float:
    try:
        return float(json.loads(body).get("retry_after", 1.0))
    except (ValueError, AttributeError, TypeError):
        return 1.0


def discord_message(body: bytes) -> str:
    try:
        return str(json.loads(body).get("message", ""))
    except (ValueError, AttributeError):
        return ""


class Server:
    """The clankjob server, through its public REST API only."""

    def __init__(self, url: str, token: str, send: Http = http) -> None:
        self.url = url
        self.token = token
        self.send = send

    def headers(self) -> dict[str, str]:
        headers = {"Content-Type": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        return headers

    def create_case(self, title: str, goal: str, owner: str | None,
                    human_channels: tuple[str, ...] | None = None) -> Json:
        body: Json = {"title": title, "goal": goal}
        if owner:
            body["owner"] = owner
        if human_channels is not None:
            body["human_channels"] = list(human_channels)
        status, data = self.send("POST", f"{self.url}/api/v1/cases", self.headers(), json.dumps(body).encode())
        if 201 == status:
            case = json.loads(data)
            if isinstance(case, dict):
                return case
        try:
            error = json.loads(data).get("error", {}).get("message", "")
        except (ValueError, AttributeError):
            error = ""
        raise IntakeError(f"the server refused the case: HTTP {status} {error}".strip(), retryable=status >= 500)

    def check(self) -> str:
        """What the server says about the token, for --check."""
        status, _ = self.send("GET", f"{self.url}/api/v1/cases?limit=1", self.headers(), None)
        if 200 == status:
            return ""
        if 401 == status:
            return "the server refused CLANKJOB_TOKEN (401)"
        return f"the server answered HTTP {status}"


# ---------------------------------------------------------------- messages to cases


def mentions(message: Json, bot_id: str) -> bool:
    users = message.get("mentions")
    return isinstance(users, list) and any(isinstance(user, dict) and bot_id == user.get("id") for user in users)


def request_of(message: Json, bot_id: str) -> tuple[str, str]:
    """The case a message asks for: its title (first line) and goal (the whole text,
    with attachment links), without the bot's mention."""
    text = re.sub(rf"<@!?{bot_id}>", "", str(message.get("content", ""))).strip()
    raw = message.get("attachments")
    attachments = [item for item in raw if isinstance(item, dict)] if isinstance(raw, list) else []
    if attachments:
        links = "\n".join(f"- {item.get('filename', 'file')}: {item.get('url', '')}" for item in attachments)
        text = f"{text}\n\nAttachments sent on Discord (these links expire within a day):\n{links}".strip()
    first = next((line.strip() for line in text.splitlines() if line.strip()), "")
    title = first if len(first) <= MAX_TITLE else first[: MAX_TITLE - 1].rstrip() + "…"
    return title, text


def author_name(message: Json) -> str | None:
    author = message.get("author")
    if not isinstance(author, dict):
        return None
    name = author.get("global_name") or author.get("username")
    return str(name) if name else None


@dataclass
class Intake:
    settings: Settings
    discord: Discord
    server: Server
    bot_id: str = ""

    def load_cursor(self) -> str | None:
        try:
            state = json.loads(self.settings.state_path.read_text())
        except (OSError, ValueError):
            return None
        cursor = state.get("after") if isinstance(state, dict) else None
        return str(cursor) if cursor else None

    def save_cursor(self, message_id: str) -> None:
        path = self.settings.state_path
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps({"after": message_id}))
        temporary.replace(path)

    def link(self, case_id: str) -> str:
        return f"<{self.settings.public_url}/#/cases/{case_id}>" if self.settings.public_url else f"case `{case_id}`"

    def poll(self) -> int:
        """Handle the new messages; returns how many cases were started."""
        if not self.bot_id:
            self.bot_id = str(self.discord.me().get("id", ""))
        after = self.load_cursor()
        messages = self.discord.messages_after(self.settings.channel_id, after)
        if after is None:
            # First run: start from now, not from the channel's history.
            self.save_cursor(str(messages[-1]["id"]) if messages else "0")
            log(f"watching channel {self.settings.channel_id} from now on")
            return 0
        started = 0
        for message in messages:
            started += self.handle(message)
            self.save_cursor(str(message["id"]))
        return started

    def handle(self, message: Json) -> int:
        author = message.get("author")
        author_id = str(author.get("id", "")) if isinstance(author, dict) else ""
        wanted = (isinstance(author, dict) and not author.get("bot") and author_id in self.settings.allowed_users
                  and mentions(message, self.bot_id))
        if not wanted:
            return 0
        message_id = str(message["id"])
        title, goal = request_of(message, self.bot_id)
        if not goal:
            self.discord.reply(self.settings.channel_id, message_id,
                               "Tell me what to do after the mention, and I will start a case for it.")
            return 0
        try:
            case = self.server.create_case(title, goal, author_name(message), self.settings.human_channels)
        except IntakeError as error:
            if error.retryable:
                # The server is down or restarting: stop here, and this message is
                # tried again at the next poll (the cursor has not moved past it).
                raise
            self.discord.reply(self.settings.channel_id, message_id, f"I could not start a case: {error}")
            return 0
        # The case exists: move past the message before replying, so a failed reply
        # can never start the case twice.
        self.save_cursor(message_id)
        log(f"started case {case.get('id')} from message {message_id}")
        self.discord.reply(self.settings.channel_id, message_id,
                           f"Started **{title}**: {self.link(str(case.get('id')))}")
        return 1


def log(text: str) -> None:
    print(f"discord-intake: {text}", file=sys.stderr, flush=True)


# ---------------------------------------------------------------- check and main


def check(settings: Settings, discord: Discord, server: Server) -> bool:
    ok = True

    def line(passed: bool, what: str, detail: str = "") -> None:
        nonlocal ok
        ok = ok and passed
        print(f"  {'ok  ' if passed else 'FAIL'} {what}")
        if detail and not passed:
            print(f"         {detail}")

    try:
        bot = discord.me()
        line(True, f"Discord bot {bot.get('username')} ({bot.get('id')})")
        channel = discord.call("GET", f"/channels/{settings.channel_id}")
        name = channel.get("name") if isinstance(channel, dict) else None
        discord.messages_after(settings.channel_id, None)
        line(True, f"reads channel #{name} ({settings.channel_id})")
    except IntakeError as error:
        line(False, "Discord", f"{error}\n         fix: the token, and View Channel + Read Message History for the bot")
    line(True, f"{len(settings.allowed_users)} allowed user(s): {', '.join(sorted(settings.allowed_users))}")
    try:
        problem = server.check()
        line(not problem, f"clankjob server at {settings.server_url} accepts the token", problem)
    except IntakeError as error:
        line(False, f"clankjob server at {settings.server_url}", str(error))
    print(f"  {'ok  ' if settings.public_url else 'warn'} links back: "
          f"{settings.public_url or 'CLANKJOB_PUBLIC_URL not set, replies give the case id only'}")
    return ok


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--once", action="store_true", help="poll once, then exit")
    mode.add_argument("--check", action="store_true", help="check the configuration, then exit")
    args = parser.parse_args()
    try:
        settings = Settings.from_env(dict(os.environ))
    except IntakeError as error:
        print(f"failed: {error}", file=sys.stderr)
        return 1
    intake = Intake(settings, Discord(settings.discord_token), Server(settings.server_url, settings.server_token))
    if args.check:
        return 0 if check(settings, intake.discord, intake.server) else 1
    stopping = False

    def stop(_signal: int, _frame: FrameType | None) -> None:
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    log(f"polling channel {settings.channel_id} every {settings.poll_seconds:.0f}s")
    while not stopping:
        try:
            intake.poll()
        except IntakeError as error:
            log(f"{error}{'; will retry' if error.retryable else ''}")
            if not error.retryable and args.once:
                return 1
        if args.once:
            return 0
        deadline = time.monotonic() + settings.poll_seconds
        while not stopping and time.monotonic() < deadline:
            time.sleep(0.5)
    return 0


if __name__ == "__main__":
    sys.exit(main())
