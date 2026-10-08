#!/usr/bin/env python3
"""clankjob shell plugin: run the agent's command in the sandbox container.

A command plugin (design section 9.9) that forwards `run_command` to the sandbox's exec
service (sandbox/sandboxd.py) at SANDBOX_URL, with an optional case file, and prints the
exit code and output. With a skill, its files go along and replace /work/skills/<name>/.
Standard library only.

    shell_tool.py --command=CMD [--timeout=SECONDS] [--file=PATH] [--skill=DIR]
"""

import argparse
import base64
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

Json = dict[str, object]

DEFAULT_TIMEOUT = 120
MAX_TIMEOUT = 600
MAX_FILE = 40 * 1024 * 1024
# The server links a `file` argument as "<argument>-<case file name>".
LINK_PREFIX = "file-"
# And a `skill` argument as a directory "<argument>-<skill name>" holding its files.
SKILL_PREFIX = "skill-"


class ToolError(Exception):
    """A failure reported to the LLM (printed to stderr, exit status 1)."""


def sandbox_url(env: dict[str, str]) -> str:
    url = env.get("SANDBOX_URL", "").strip().rstrip("/")
    if not url.startswith(("http://", "https://")):
        raise ToolError("SANDBOX_URL is not set in the shell plugin's config.toml [env]")
    return url


def call(url: str, path: str, body: Json | None, timeout: float) -> Json:
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(f"{url}{path}", data=data, headers={"Content-Type": "application/json"},
                                     method="GET" if body is None else "POST")
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            reply = json.loads(response.read())
    except urllib.error.HTTPError as error:
        try:
            detail = json.loads(error.read()).get("error", "")
        except (ValueError, AttributeError):
            detail = ""
        raise ToolError(f"the sandbox refused the command: {detail or error}") from None
    except (OSError, ValueError) as error:
        raise ToolError(f"cannot reach the sandbox at {url}: {error} (is the sandbox container running?)") from None
    if not isinstance(reply, dict):
        raise ToolError("the sandbox sent an unexpected reply")
    return reply


def case_file(path: Path) -> Json:
    if not path.is_file():
        raise ToolError(f"{path.name} cannot be read")
    size = path.stat().st_size
    if size > MAX_FILE:
        raise ToolError(f"{path.name} is {size // 1_000_000} MB; at most {MAX_FILE // 1_000_000} MB can be copied")
    name = path.name.removeprefix(LINK_PREFIX) or path.name
    return {"name": name, "data": base64.b64encode(path.read_bytes()).decode()}


def skill_files(path: Path) -> Json:
    if not path.is_dir():
        raise ToolError(f"{path.name} is not a skill's directory")
    files: list[Json] = [{"name": file.name, "data": base64.b64encode(file.read_bytes()).decode()}
                         for file in sorted(path.iterdir()) if file.is_file()]
    return {"name": path.name.removeprefix(SKILL_PREFIX), "files": files}


def report(reply: Json, timeout: int) -> str:
    """Exit status first, then each non-empty stream."""
    if reply.get("timed_out"):
        head = f"timed out after {timeout}s (killed)"
    else:
        head = f"exit code {reply.get('exit_code')}"
    parts = [head]
    for stream in ("stdout", "stderr"):
        text = str(reply.get(stream) or "")
        if text:
            parts.append(f"--- {stream} ---\n{text.rstrip()}")
    if reply.get("truncated"):
        parts.append("(output cut at 1 MB per stream: redirect it to a file in /work and read parts of it)")
    return "\n".join(parts)


def run(env: dict[str, str], command: str, timeout: int, file: Path | None,
        skill: Path | None = None) -> tuple[str, bool]:
    """The report, and whether the command succeeded."""
    if not 1 <= timeout <= MAX_TIMEOUT:
        raise ToolError(f"timeout must be 1 to {MAX_TIMEOUT} seconds")
    body: Json = {"command": command, "timeout": timeout, "files": [case_file(file)] if file else []}
    if skill:
        body["skill"] = skill_files(skill)
    reply = call(sandbox_url(env), "/run", body, timeout + 30)
    return report(reply, timeout), 0 == reply.get("exit_code")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--command", required=True)
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT)
    parser.add_argument("--file", type=Path)
    parser.add_argument("--skill", type=Path)
    args = parser.parse_args()
    try:
        text, _ = run(dict(os.environ), args.command, args.timeout, args.file, args.skill)
    except ToolError as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1
    # A failing command is a result for the agent to read, not a tool error.
    print(text)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
