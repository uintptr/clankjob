#!/usr/bin/env python3
"""Tests for the shell plugin, against a fake sandbox. Run: python3 -m unittest -v test_shell_tool.py

SHELL_LIVE_TEST=1 also runs check_config.py's checks against the sandbox in config.toml."""

import base64
import json
import os
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import shell_tool as tool
from shell_tool import ToolError


class FakeSandbox:
    """Answers /run with a canned reply (or status) and keeps the requests it got."""

    def __init__(self, reply: dict[str, object], status: int = 200) -> None:
        self.requests: list[dict[str, object]] = []
        fake = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                fake.requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                data = json.dumps(reply).encode()
                self.send_response(status)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, format: str, *args: object) -> None:
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.env = {"SANDBOX_URL": f"http://127.0.0.1:{self.server.server_address[1]}/"}
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


class ShellToolTests(unittest.TestCase):

    def sandbox(self, reply: dict[str, object], status: int = 200) -> FakeSandbox:
        fake = FakeSandbox(reply, status)
        self.addCleanup(fake.close)
        return fake

    def test_report_gives_the_exit_code_then_each_stream(self) -> None:
        fake = self.sandbox({"exit_code": 2, "stdout": "out\n", "stderr": "", "timed_out": False})
        text, ok = tool.run(fake.env, "ls nope", 30, None)
        self.assertEqual(("exit code 2\n--- stdout ---\nout", False), (text, ok))
        self.assertEqual({"command": "ls nope", "timeout": 30, "files": []}, fake.requests[0])

    def test_timeouts_and_cut_output_are_said(self) -> None:
        text = tool.report({"timed_out": True, "stdout": "", "stderr": "x", "truncated": True}, 5)
        self.assertTrue(text.startswith("timed out after 5s"))
        self.assertIn("cut at 1 MB", text)

    def test_a_case_file_is_sent_under_its_case_name(self) -> None:
        fake = self.sandbox({"exit_code": 0})
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / "file-quote.pdf"
            path.write_bytes(b"%PDF")
            tool.run(fake.env, "ls", 10, path)
        files = fake.requests[0]["files"]
        self.assertEqual([{"name": "quote.pdf", "data": base64.b64encode(b"%PDF").decode()}], files)

    def test_errors(self) -> None:
        with self.assertRaisesRegex(ToolError, "SANDBOX_URL"):
            tool.run({}, "ls", 10, None)
        with self.assertRaisesRegex(ToolError, "timeout"):
            tool.run({"SANDBOX_URL": "http://x"}, "ls", 601, None)
        with self.assertRaisesRegex(ToolError, "refused the command: command is required"):
            tool.run(self.sandbox({"error": "command is required"}, 400).env, " ", 10, None)
        with self.assertRaisesRegex(ToolError, "is the sandbox container running"):
            tool.run({"SANDBOX_URL": "http://127.0.0.1:9"}, "ls", 10, None)


@unittest.skipUnless("1" == os.environ.get("SHELL_LIVE_TEST"), "set SHELL_LIVE_TEST=1 to use the real sandbox")
class LiveTests(unittest.TestCase):

    def test_check_config_passes(self) -> None:
        from check_config import HERE, live_checks, load_env
        from shell_tool import sandbox_url
        checks = live_checks(sandbox_url(load_env(HERE / "config.toml", Path("/run/secrets"))))
        self.assertTrue(all(check.ok or not check.required for check in checks), checks)


if __name__ == "__main__":
    unittest.main()
