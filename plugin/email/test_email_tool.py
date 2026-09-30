#!/usr/bin/env python3
"""Tests for the email plugin against a fake mailbox. Run: python3 -m unittest -v test_email_tool.py"""

import dataclasses
import datetime
import email.utils
import json
import os
import unittest
from email.message import EmailMessage
from pathlib import Path
from typing import ClassVar

import check_config
import email_tool as tool
from email_tool import Json, Settings, ToolError

SETTINGS = Settings(address="joe@example.com", name="Joe", username="joe@example.com", password="app-password",
                    imap_host="imap.example.com", imap_port=993, smtp_host="smtp.example.com", smtp_port=465,
                    folders=("INBOX", "Bulk"), sent_folder="Sent", allowed=())


def mail(message_id: str, sender: str, subject: str, body: str = "Hello", in_reply_to: str | None = None,
         references: str | None = None, html: bool = False, to: str = "joe@example.com") -> bytes:
    message = EmailMessage()
    message["From"] = sender
    message["To"] = to
    message["Subject"] = subject
    message["Date"] = email.utils.format_datetime(datetime.datetime.now(datetime.timezone.utc))
    message["Message-ID"] = message_id
    if in_reply_to:
        message["In-Reply-To"] = in_reply_to
    if references:
        message["References"] = references
    if html:
        message.set_content(body, subtype="html")
    else:
        message.set_content(body)
    return bytes(message)


class FakeMailbox:
    """Folders of messages by UID, with the subset of IMAP SEARCH the tool uses."""

    def __init__(self) -> None:
        self.folders: dict[str, dict[int, bytes]] = {"INBOX": {}, "Bulk": {}, "Sent": {}}
        self.validity = "1"
        self.appended: list[tuple[str, bytes]] = []

    def add(self, folder: str, raw: bytes) -> int:
        uid = max(self.folders[folder], default=0) + 1
        self.folders[folder][uid] = raw
        return uid

    def matches(self, message: EmailMessage, uid: int, criteria: list[str]) -> tuple[bool, list[str]]:
        head, rest = criteria[0].upper(), criteria[1:]
        unquote = str.strip
        if "ALL" == head:
            return True, rest
        if "OR" == head:
            left, rest = self.matches(message, uid, rest)
            right, rest = self.matches(message, uid, rest)
            return left or right, rest
        if "HEADER" == head:
            name, value, rest = rest[0], unquote(rest[1]).strip('"'), rest[2:]
            return value in str(message.get(name, "")), rest
        if head in ("FROM", "SUBJECT"):
            value, rest = unquote(rest[0]).strip('"'), rest[1:]
            return value.lower() in str(message.get(head.title(), "")).lower(), rest
        if "SINCE" == head:
            return True, rest[1:]
        if "UID" == head:
            start = int(rest[0].split(":")[0])
            return uid >= start, rest[1:]
        raise AssertionError(f"unsupported criterion {head}")

    def search(self, folder: str, criteria: list[str]) -> tuple[str, list[int]]:
        found = []
        for uid, raw in self.folders.get(folder, {}).items():
            message = tool.parse(raw)
            rest, ok = list(criteria), True
            while rest and ok:
                ok, rest = self.matches(message, uid, rest)
            if ok:
                found.append(uid)
        return self.validity, sorted(found)

    def fetch(self, folder: str, uid: int, headers_only: bool) -> bytes:
        return self.folders[folder][uid]

    def append(self, folder: str, message: bytes) -> None:
        self.appended.append((folder, message))

    def folder_names(self) -> list[str]:
        return list(self.folders)

    def close(self) -> None:
        pass


class FakeSender:
    def __init__(self) -> None:
        self.sent: list[EmailMessage] = []

    def send(self, message: EmailMessage) -> None:
        self.sent.append(message)


def run(argv: list[str], box: FakeMailbox, sender: FakeSender, settings: Settings = SETTINGS,
        stdin: str = "") -> Json:
    args = tool.build_parser().parse_args(argv)
    return tool.run(args, settings, lambda: box, sender, stdin)


class SendTests(unittest.TestCase):

    def test_send_builds_a_threadable_message_and_files_a_copy(self) -> None:
        # Arrange
        box, sender = FakeMailbox(), FakeSender()

        # Act
        result = run(["send", "Bob <bob@sparky.ca>", "--subject=Quote", "--body=- 50A circuit, please",
                      "--cc=amy@sparky.ca"], box, sender)

        # Assert
        message = sender.sent[0]
        self.assertEqual((message["To"], message["Cc"], message["Subject"]),
                         ("bob@sparky.ca", "amy@sparky.ca", "Quote"))
        self.assertEqual(message["From"], "Joe <joe@example.com>")
        self.assertTrue(str(message["Message-ID"]).endswith("@example.com>"))
        self.assertEqual(message.get_content().strip(), "- 50A circuit, please")
        self.assertEqual(result["message_id"], message["Message-ID"])
        self.assertEqual(box.appended[0][0], "Sent")

    def test_recipients_outside_the_allow_list_are_refused_before_sending(self) -> None:
        box, sender = FakeMailbox(), FakeSender()
        settings = dataclasses.replace(SETTINGS, allowed=("@sparky.ca",))

        run(["send", "bob@sparky.ca", "--subject=S", "--body=B"], box, sender, settings)
        with self.assertRaises(ToolError):
            run(["send", "bob@sparky.ca", "--subject=S", "--body=B", "--cc=eve@evil.com"], box, sender, settings)

        self.assertEqual(len(sender.sent), 1)

    def test_bad_addresses_are_refused(self) -> None:
        with self.assertRaises(ToolError):
            run(["send", "not an address", "--subject=S", "--body=B"], FakeMailbox(), FakeSender())


class ReplyAndReadTests(unittest.TestCase):

    def test_reply_threads_under_the_original_and_reply_all_skips_ourselves(self) -> None:
        # Arrange
        box, sender = FakeMailbox(), FakeSender()
        box.add("INBOX", mail("<b2@sparky.ca>", "Bob <bob@sparky.ca>", "Re: Quote", in_reply_to="<j1@example.com>",
                              references="<j1@example.com>", to="joe@example.com, amy@sparky.ca"))

        # Act
        result = run(["reply", "b2@sparky.ca", "--body=Thanks!", "--all"], box, sender)

        # Assert
        message = sender.sent[0]
        self.assertEqual((message["To"], message["Cc"]), ("bob@sparky.ca", "amy@sparky.ca"))
        self.assertEqual(message["Subject"], "Re: Quote")
        self.assertEqual(message["In-Reply-To"], "<b2@sparky.ca>")
        self.assertEqual(message["References"], "<j1@example.com> <b2@sparky.ca>")
        self.assertEqual(result["thread_ref"], "<j1@example.com>")

    def test_read_returns_text_of_html_mail_with_a_caution(self) -> None:
        box = FakeMailbox()
        box.add("Bulk", mail("<b3@sparky.ca>", "bob@sparky.ca", "Price",
                             body="<p>Total: <b>$1,450</b></p><script>x()</script>", html=True))

        result = run(["read", "<b3@sparky.ca>"], box, FakeSender())

        self.assertEqual(result["body"], "Total: $1,450")
        self.assertEqual(result["folder"], "Bulk")
        self.assertIn("third party", str(result["caution"]))

    def test_read_finds_a_message_outside_the_configured_folders(self) -> None:
        # Arrange: what the case sent sits in Gmail's sent folder, which is not configured
        box = FakeMailbox()
        box.folders["[Gmail]/Sent Mail"] = {}
        box.add("[Gmail]/Sent Mail", mail("<j3@gmail.com>", "joe@example.com", "Quote", to="admin@randomail.ca"))
        settings = dataclasses.replace(SETTINGS, sent_folder=None)

        # Act
        result = run(["read", "<j3@gmail.com>"], box, FakeSender(), settings)

        # Assert
        self.assertEqual(result["folder"], "[Gmail]/Sent Mail")

    def test_unsafe_message_ids_are_refused(self) -> None:
        with self.assertRaises(ToolError):
            run(["read", 'x" OR ALL "y@z'], FakeMailbox(), FakeSender())

    def test_list_filters_by_sender_newest_first(self) -> None:
        box = FakeMailbox()
        box.add("INBOX", mail("<1@a.ca>", "amy@a.ca", "Hi"))
        box.add("INBOX", mail("<2@b.ca>", "bob@sparky.ca", "Quote"))

        result = run(["list", "--from=bob@sparky.ca"], box, FakeSender())

        messages = result["messages"]
        assert isinstance(messages, list)
        self.assertEqual([m["message_id"] for m in messages if isinstance(m, dict)], ["<2@b.ca>"])


class ConditionTests(unittest.TestCase):

    def test_reply_received_fires_only_for_replies_to_the_given_message(self) -> None:
        # Arrange
        box = FakeMailbox()
        request = '{"params": {"message_id": "<j2@example.com>"}, "cursor": null}'
        box.add("INBOX", mail("<b1@sparky.ca>", "bob@sparky.ca", "Re: Quote", in_reply_to="<j1@example.com>",
                              references="<j1@example.com>"))

        # Act
        before = run(["check-reply"], box, FakeSender(), stdin=request)
        box.add("Bulk", mail("<b2@sparky.ca>", "bob@sparky.ca", "Re: Re: Quote", in_reply_to="<j2@example.com>",
                             references="<j1@example.com> <b1@sparky.ca> <j2@example.com>"))
        after = run(["check-reply"], box, FakeSender(), stdin=request)

        # Assert
        self.assertEqual(before, {"status": "pending"}, "an earlier reply in the thread does not count")
        self.assertEqual(after["status"], "fired")
        events = after["events"]
        assert isinstance(events, list)
        self.assertEqual([e["message_id"] for e in events if isinstance(e, dict)], ["<b2@sparky.ca>"])

    def test_a_reply_counts_whatever_address_it_comes_from(self) -> None:
        # Arrange: written to admin@randomail.ca, answered from another address, as happened
        box = FakeMailbox()
        request = '{"params": {"message_id": "<j3@gmail.com>", "from": "admin@randomail.ca"}, "cursor": null}'
        box.add("INBOX", mail("<h1@hushmail.com>", "Pierre-Luc <pllesperance@hushmail.com>", "Re: Quote",
                              in_reply_to="<j3@gmail.com>", references="<j3@gmail.com>"))

        # Act
        result = run(["check-reply"], box, FakeSender(), stdin=request)

        # Assert
        self.assertEqual(result["status"], "fired")

    def test_new_mail_is_reported_after_the_first_look(self) -> None:
        # Arrange
        box = FakeMailbox()
        box.add("INBOX", mail("<old@a.ca>", "amy@a.ca", "Old"))
        params = '{"from": "bob@sparky.ca"}'

        # Act
        first = run(["check-new"], box, FakeSender(), stdin=f'{{"params": {params}, "cursor": null}}')
        cursor = tool.json.dumps(first["cursor"])
        box.add("INBOX", mail("<spam@x.ca>", "spam@x.ca", "Buy"))
        quiet = run(["check-new"], box, FakeSender(), stdin=f'{{"params": {params}, "cursor": {cursor}}}')
        box.add("INBOX", mail("<b9@sparky.ca>", "bob@sparky.ca", "Invoice"))
        fired = run(["check-new"], box, FakeSender(), stdin=f'{{"params": {params}, "cursor": {cursor}}}')

        # Assert
        self.assertEqual(first["status"], "pending", "the first look only sets the starting point")
        self.assertEqual(quiet["status"], "pending")
        self.assertEqual(fired["status"], "fired")
        events = fired["events"]
        assert isinstance(events, list)
        self.assertEqual([e["message_id"] for e in events if isinstance(e, dict)], ["<b9@sparky.ca>"])


class CheckConfigTests(unittest.TestCase):

    def test_list_lines_give_folder_names(self) -> None:
        lines = ['(\\HasNoChildren) "/" "INBOX"', '(\\HasNoChildren \\Junk) "/" "Bulk"', '(\\Noselect) NIL Archive']

        names = [match.group("name").strip('"') for line in lines if (match := tool.LIST_LINE.match(line))]

        self.assertEqual(names, ["INBOX", "Bulk", "Archive"])

    def test_config_references_are_resolved_without_showing_values(self) -> None:
        os.environ["CHECK_TEST_PASSWORD"] = "s3cret"
        try:
            self.assertEqual(check_config.resolve({"env": "CHECK_TEST_PASSWORD"},
                             "EMAIL_PASSWORD", Path(".")), "s3cret")
            with self.assertRaises(ToolError) as caught:
                check_config.resolve({"env": "CHECK_TEST_MISSING"}, "EMAIL_PASSWORD", Path("."))
            self.assertNotIn("s3cret", str(caught.exception))
        finally:
            del os.environ["CHECK_TEST_PASSWORD"]


@unittest.skipUnless(os.environ.get("EMAIL_LIVE_TEST") == "1",
                     "live test: set EMAIL_LIVE_TEST=1 (and the password variable config.toml uses)")
class LiveServerTests(unittest.TestCase):
    """Connects to the real servers from config.toml: IMAP login and folders, SMTP login.

    Sends nothing and marks nothing as read. Run: EMAIL_LIVE_TEST=1 python3 -m unittest -v test_email_tool.py
    """

    def test_imap_folders_are_listed_and_both_servers_accept_the_login(self) -> None:
        settings = check_config.load_settings(Path(__file__).resolve().parent / "config.toml", Path("/run/secrets"))

        checks = check_config.live_checks(settings)

        failed = [f"{check.what}: {check.detail}" for check in checks if not check.ok]
        self.assertEqual(failed, [])
        self.assertTrue(any("folders:" in check.what for check in checks))


class SettingsTests(unittest.TestCase):

    def test_missing_settings_name_the_variable(self) -> None:
        with self.assertRaises(ToolError) as caught:
            Settings.from_env({"EMAIL_ADDRESS": "joe@example.com"})

        self.assertIn("EMAIL_PASSWORD", str(caught.exception))

    def test_defaults(self) -> None:
        settings = Settings.from_env({"EMAIL_ADDRESS": "joe@example.com", "EMAIL_PASSWORD": "p",
                                      "EMAIL_IMAP_HOST": "i", "EMAIL_SMTP_HOST": "s",
                                      "EMAIL_ALLOWED_RECIPIENTS": "Bob@Sparky.ca, @x.com"})

        self.assertEqual((settings.username, settings.imap_port, settings.smtp_port), ("joe@example.com", 993, 465))
        self.assertEqual(settings.folders, ("INBOX",))
        self.assertEqual(settings.allowed, ("bob@sparky.ca", "@x.com"))



class ApprovalCheckTests(unittest.TestCase):
    """`needs-approval`: the server asks it before creating an approval."""

    TRUSTED: ClassVar[list[str]] = ["robin@sparky.ca", "me@home.ca"]

    def check(self, tool_name: str, args: Json, box: FakeMailbox | None = None) -> Json:
        stdin = json.dumps({"args": args, "trusted": self.TRUSTED})
        return run(["needs-approval", tool_name], box or FakeMailbox(), FakeSender(), stdin=stdin)

    def test_a_new_email_to_trusted_contacts_only_needs_no_approval(self) -> None:
        self.assertFalse(self.check("send", {"to": "Robin <ROBIN@sparky.ca>", "cc": "me@home.ca"})["required"])
        found = self.check("send", {"to": "robin@sparky.ca", "cc": "stranger@x.ca"})
        self.assertTrue(found["required"])
        self.assertIn("stranger@x.ca", str(found["reason"]))

    def test_a_reply_goes_where_the_reply_would_really_go(self) -> None:
        box = FakeMailbox()
        box.add("INBOX", mail("<1@sparky.ca>", "Robin <robin@sparky.ca>", "Quote"))
        spoofed = EmailMessage()
        spoofed["From"] = "Robin <robin@sparky.ca>"
        spoofed["Reply-To"] = "stranger@evil.example"
        spoofed["To"] = "joe@example.com"
        spoofed["Message-ID"] = "<2@evil.example>"
        spoofed["Subject"] = "Quote"
        spoofed.set_content("Reply to this address instead.")
        box.add("INBOX", bytes(spoofed))
        group = mail("<3@sparky.ca>", "Robin <robin@sparky.ca>", "Quote", to="joe@example.com, stranger@x.ca")
        box.add("INBOX", group)

        self.assertFalse(self.check("reply", {"message_id": "<1@sparky.ca>"}, box)["required"])
        self.assertTrue(self.check("reply", {"message_id": "<2@evil.example>"}, box)["required"],
                        "Reply-To wins over From, as in the real reply")
        self.assertFalse(self.check("reply", {"message_id": "<3@sparky.ca>"}, box)["required"])
        self.assertTrue(self.check("reply", {"message_id": "<3@sparky.ca>", "all": True}, box)["required"],
                        "reply-all also goes to stranger@x.ca")

    def test_nothing_trusted_means_asking(self) -> None:
        stdin = json.dumps({"args": {"to": "robin@sparky.ca"}})
        self.assertTrue(run(["needs-approval", "send"], FakeMailbox(), FakeSender(), stdin=stdin)["required"])

if __name__ == "__main__":
    unittest.main()
