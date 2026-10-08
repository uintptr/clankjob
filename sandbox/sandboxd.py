#!/usr/bin/env python3
"""clankjob sandbox: runs the agent's shell commands in their own container.

A small HTTP service, standard library only, for the `shell` plugin. The sandbox runs the
clankjob image with none of the server's secrets, data or settings, so a command can use
every program installed and the network, but cannot read an API key, the database or
another plugin's configuration.

    GET  /health    {"ok": true, "user": …, "work": …}
    POST /run       {"command": "…", "timeout": 120, "files": [{"name": …, "data": base64}],
                     "skill": {"name": …, "files": [{"name": …, "data": base64}]}}
                 -> {"exit_code": 0, "stdout": "…", "stderr": "…", "timed_out": false,
                     "truncated": false, "files_dir": "/work/case-files"}

Case files land in /work/case-files/. A skill's files replace whatever is in
/work/skills/<name>/, so a command always runs the version the owner approved.

Commands run with `bash -c` in the work directory (a volume that persists between calls),
in a new session so a timeout kills everything they started. Listen only on a network the
server shares with the sandbox: the service has no authentication, and needs none there,
since whoever can reach it could already run commands in the sandbox.

    sandboxd.py [--listen 0.0.0.0:8000] [--work /work]
"""

import argparse
import base64
import binascii
import json
import os
import pwd
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import IO

Json = dict[str, object]

DEFAULT_TIMEOUT = 120
MAX_TIMEOUT = 600
MAX_OUTPUT = 1_000_000  # bytes kept of stdout and of stderr
MAX_REQUEST = 64 * 1024 * 1024  # case files come base64-encoded in the request
FILES_DIR = "case-files"
SKILLS_DIR = "skills"
SKILL_NAME = re.compile(r"[a-z0-9][a-z0-9-]{0,63}")
# Replacing a skill's directory is a remove then a rename: one at a time.
SKILL_LOCK = threading.Lock()


class RequestError(Exception):
    """A bad request, answered with 400 and this message."""


@dataclass(frozen=True)
class Result:
    exit_code: int | None
    stdout: str
    stderr: str
    timed_out: bool
    truncated: bool


def safe_name(name: str) -> str:
    """A case file's name as a single, visible path component."""
    cleaned = Path(name.replace("\\", "/")).name.strip().lstrip(".")
    if not cleaned:
        raise RequestError(f"bad file name {name!r}")
    return cleaned


def decode_files(files: object) -> list[tuple[str, bytes]]:
    """Each file's safe name and bytes."""
    if not isinstance(files, list):
        raise RequestError("files must be a list")
    decoded: list[tuple[str, bytes]] = []
    for entry in files:
        if not isinstance(entry, dict) or not isinstance(entry.get("name"), str) \
                or not isinstance(entry.get("data"), str):
            raise RequestError("each file needs a name and base64 data")
        try:
            data = base64.b64decode(str(entry["data"]), validate=True)
        except (binascii.Error, ValueError):
            raise RequestError(f"file {entry['name']!r}: data is not base64") from None
        decoded.append((safe_name(str(entry["name"])), data))
    return decoded


def save_files(work: Path, files: object) -> None:
    target = work / FILES_DIR
    for name, data in decode_files(files):
        target.mkdir(parents=True, exist_ok=True)
        (target / name).write_bytes(data)


def save_skill(work: Path, skill: object) -> None:
    """Write a skill's files to /work/skills/<name>/, replacing what was there."""
    if not isinstance(skill, dict):
        raise RequestError("a skill must be an object")
    request: Json = skill
    name = request.get("name")
    if not isinstance(name, str) or not SKILL_NAME.fullmatch(name):
        raise RequestError("a skill needs a name of lowercase letters, digits and dashes")
    files = decode_files(request.get("files", []))
    skills = work / SKILLS_DIR
    skills.mkdir(parents=True, exist_ok=True)
    fresh = Path(tempfile.mkdtemp(prefix=f".{name}-", dir=skills))
    for file_name, data in files:
        (fresh / file_name).write_bytes(data)
    fresh.chmod(0o755)
    with SKILL_LOCK:
        shutil.rmtree(skills / name, ignore_errors=True)
        fresh.rename(skills / name)


def read_capped(file: IO[bytes]) -> tuple[str, bool]:
    file.seek(0, os.SEEK_END)
    size = file.tell()
    file.seek(0)
    return file.read(MAX_OUTPUT).decode(errors="replace"), size > MAX_OUTPUT


def run(command: str, timeout: int, work: Path) -> Result:
    """Run a command, keeping up to MAX_OUTPUT bytes of each stream (on disk until then)."""
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        process = subprocess.Popen(["bash", "-c", command], cwd=work, stdin=subprocess.DEVNULL, stdout=out,
                                   stderr=err, start_new_session=True)
        timed_out = False
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
        # Whatever the command left running in the background goes too.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()
        stdout, cut_out = read_capped(out)
        stderr, cut_err = read_capped(err)
    return Result(None if timed_out else process.returncode, stdout, stderr, timed_out, cut_out or cut_err)


def handle_run(request: Json, work: Path) -> Json:
    command = request.get("command")
    if not isinstance(command, str) or not command.strip():
        raise RequestError("command is required")
    timeout = request.get("timeout", DEFAULT_TIMEOUT)
    if not isinstance(timeout, int) or isinstance(timeout, bool) or not 1 <= timeout <= MAX_TIMEOUT:
        raise RequestError(f"timeout must be 1 to {MAX_TIMEOUT} seconds")
    save_files(work, request.get("files", []))
    if "skill" in request:
        save_skill(work, request["skill"])
    result = run(command, timeout, work)
    return {"exit_code": result.exit_code, "stdout": result.stdout, "stderr": result.stderr,
            "timed_out": result.timed_out, "truncated": result.truncated, "files_dir": str(work / FILES_DIR)}


def health(work: Path) -> Json:
    return {"ok": True, "user": pwd.getpwuid(os.getuid()).pw_name, "work": str(work)}


def make_handler(work: Path) -> type[BaseHTTPRequestHandler]:

    class Handler(BaseHTTPRequestHandler):
        server_version = "clankjob-sandbox/0.1"

        def reply(self, status: int, body: Json) -> None:
            data = json.dumps(body, ensure_ascii=False).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self) -> None:
            if "/health" == self.path:
                self.reply(200, health(work))
            else:
                self.reply(404, {"error": "not found"})

        def do_POST(self) -> None:
            if "/run" != self.path:
                self.reply(404, {"error": "not found"})
                return
            length = int(self.headers.get("Content-Length") or 0)
            if not 0 < length <= MAX_REQUEST:
                self.reply(413 if length else 400, {"error": f"the request must be 1 byte to {MAX_REQUEST} bytes"})
                return
            try:
                request = json.loads(self.rfile.read(length))
                if not isinstance(request, dict):
                    raise RequestError("the request must be a JSON object")
                self.reply(200, handle_run(request, work))
            except (RequestError, ValueError) as error:
                self.reply(400, {"error": str(error)})
            except OSError as error:
                self.reply(500, {"error": f"cannot run the command: {error}"})

        def log_message(self, format: str, *args: object) -> None:
            print(f"sandbox: {format % args}", file=sys.stderr, flush=True)

    return Handler


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--listen", default="0.0.0.0:8000", help="address:port (default: %(default)s)")
    parser.add_argument("--work", type=Path, default=Path("/work"), help="work directory (default: %(default)s)")
    args = parser.parse_args()
    host, _, port = str(args.listen).rpartition(":")
    work: Path = args.work
    work.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer((host or "0.0.0.0", int(port)), make_handler(work))
    print(f"sandbox: listening on {args.listen}, work directory {work}", file=sys.stderr, flush=True)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
