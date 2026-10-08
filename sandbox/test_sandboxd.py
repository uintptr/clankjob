#!/usr/bin/env python3
"""Tests for the sandbox exec service, running real commands. Run: python3 -m unittest -v test_sandboxd.py"""

import base64
import json
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer
from pathlib import Path

import sandboxd
from sandboxd import Json, RequestError


class RunTests(unittest.TestCase):

    @property
    def work(self) -> Path:
        """A fresh work directory, removed after the test."""
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        return Path(directory.name).resolve()

    def test_output_exit_code_and_work_directory(self) -> None:
        work = self.work
        result = sandboxd.run("pwd; echo oops >&2; exit 3", 10, work)
        self.assertEqual((3, str(work), "oops\n", False),
                         (result.exit_code, result.stdout.strip(), result.stderr, result.timed_out))

    def test_a_timeout_kills_the_command_and_what_it_started(self) -> None:
        started = time.monotonic()
        work = self.work
        result = sandboxd.run("(sleep 30; touch late) & sleep 30", 1, work)
        self.assertLess(time.monotonic() - started, 10)
        self.assertEqual((None, True), (result.exit_code, result.timed_out))
        time.sleep(0.2)
        self.assertFalse((work / "late").exists())

    def test_background_processes_do_not_outlive_a_finished_command(self) -> None:
        result = sandboxd.run("sleep 30 & echo done", 10, self.work)
        self.assertEqual("done\n", result.stdout)

    def test_long_output_is_cut(self) -> None:
        result = sandboxd.run(f"head -c {sandboxd.MAX_OUTPUT + 10} /dev/zero", 10, self.work)
        self.assertEqual((sandboxd.MAX_OUTPUT, True), (len(result.stdout), result.truncated))

    def test_files_land_in_case_files_under_a_safe_name(self) -> None:
        data = base64.b64encode(b"hello").decode()
        work = self.work
        sandboxd.save_files(work, [{"name": "../../etc/.quote.pdf", "data": data}])
        self.assertEqual(b"hello", (work / "case-files" / "quote.pdf").read_bytes())
        for bad in ([{"name": "..", "data": data}], [{"name": "x", "data": "%%%"}], [{"name": "x"}], "x"):
            with self.assertRaises(RequestError):
                sandboxd.save_files(self.work, bad)

    def test_a_skill_replaces_its_directory(self) -> None:
        work = self.work
        encoded = base64.b64encode(b"print(1)").decode()
        (work / "skills" / "forecast").mkdir(parents=True)
        (work / "skills" / "forecast" / "stale.py").write_text("tampered")
        sandboxd.save_skill(work, {"name": "forecast", "files": [{"name": "f.py", "data": encoded}]})
        self.assertEqual(["f.py"], [path.name for path in (work / "skills" / "forecast").iterdir()])
        self.assertEqual(["forecast"], [path.name for path in (work / "skills").iterdir()], "no temporary left")
        for bad in ({"name": "../x", "files": []}, {"name": "x", "files": "nope"}, "x", {"files": []}):
            with self.assertRaises(RequestError):
                sandboxd.save_skill(work, bad)

    def test_request_checks(self) -> None:
        requests: list[Json] = [{}, {"command": "  "}, {"command": "true", "timeout": 0},
                                 {"command": "true", "timeout": 601}, {"command": "true", "timeout": True}]
        for request in requests:
            with self.assertRaises(RequestError):
                sandboxd.handle_run(request, self.work)


class HttpTests(unittest.TestCase):

    def start(self) -> str:
        """The URL of a sandbox service on a free port, stopped after the test."""
        directory = tempfile.TemporaryDirectory()
        server = ThreadingHTTPServer(("127.0.0.1", 0), sandboxd.make_handler(Path(directory.name)))
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(directory.cleanup)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return f"http://127.0.0.1:{server.server_address[1]}"

    def post(self, url: str, body: object) -> tuple[int, dict[str, object]]:
        request = urllib.request.Request(f"{url}/run", data=json.dumps(body).encode(), method="POST")
        try:
            with urllib.request.urlopen(request, timeout=10) as response:
                return response.status, json.loads(response.read())
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read())

    def test_run_and_health(self) -> None:
        files = [{"name": "a.txt", "data": base64.b64encode(b"from the case").decode()}]
        url = self.start()
        status, reply = self.post(url, {"command": "cat case-files/a.txt", "files": files})
        self.assertEqual((200, "from the case", 0), (status, reply["stdout"], reply["exit_code"]))
        with urllib.request.urlopen(f"{url}/health", timeout=10) as response:
            self.assertTrue(json.loads(response.read())["ok"])

    def test_a_command_runs_a_skills_files(self) -> None:
        script = base64.b64encode(b"print('sunny')").decode()
        status, reply = self.post(self.start(), {"command": "python3 skills/forecast/f.py",
                                                 "skill": {"name": "forecast", "files": [{"name": "f.py", "data": script}]}})
        self.assertEqual((200, "sunny\n"), (status, reply["stdout"]))

    def test_bad_requests_get_400(self) -> None:
        url = self.start()
        self.assertEqual(400, self.post(url, {"timeout": 5})[0])
        self.assertEqual(400, self.post(url, ["not", "an", "object"])[0])


if __name__ == "__main__":
    unittest.main()
