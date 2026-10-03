#!/usr/bin/env python3
"""Tests for clankjob_setup.py, against this checkout. Run: python3 -m unittest -v test_clankjob_setup.py"""

import argparse
import os
import shutil
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import clankjob_setup as setup
import tomllib
from clankjob_setup import Asker, Share

REPO = Path(__file__).resolve().parent.parent
SHARE = Share.checkout(REPO)


class ScriptedAsker(Asker):
    """Answers questions containing a key of `answers`; takes the default otherwise."""

    def __init__(self, answers: dict[str, str]) -> None:
        super().__init__()
        self.answers = answers
        self.asked: list[str] = []

    def answer(self, question: str) -> str | None:
        self.asked.append(question)
        return next((value for key, value in self.answers.items() if key in question), None)

    def ask(self, question: str, default: str = "") -> str:
        found = self.answer(question)
        return default if found is None else found

    def secret(self, question: str) -> str:
        return self.answer(question) or ""

    def yes(self, question: str, default: bool = False) -> bool:
        found = self.answer(question)
        return default if found is None else found == "y"


class AskerTests(unittest.TestCase):

    def test_backspaces_in_a_hidden_answer_delete_what_was_typed(self) -> None:
        self.assertEqual("secret", setup.erase_backspaces("sex\bcret"))
        self.assertEqual("secret", setup.erase_backspaces("\x7fsecrex\x7ft"))
        self.assertEqual("", setup.erase_backspaces("ab\b\b\b"))
        with mock.patch("getpass.getpass", return_value="tk_1x\b2 "):
            self.assertEqual("tk_12", Asker().secret("Token"))


class FillTemplateTests(unittest.TestCase):

    def test_discord_example_is_filled_and_its_token_goes_to_env(self) -> None:
        # Arrange
        template = (REPO / "plugin/discord/config.example.toml").read_text()
        asker = ScriptedAsker({"channel_id": "111", "allowed_responders": "222, 333",
                               "DISCORD_BOT_TOKEN": "tok", "mention": "n"})

        # Act
        text, found = setup.fill_template(template, asker, {}, "Plugin discord")

        # Assert
        config = tomllib.loads(text)["instances"]["discord_joe"]
        self.assertEqual(config["bot_token"], {"env": "DISCORD_BOT_TOKEN"}, "the secret stays a reference")
        self.assertEqual((config["channel_id"], config["allowed_responders"]), ("111", ["222", "333"]))
        self.assertIs(config["mention"], False)
        self.assertEqual(found, {"DISCORD_BOT_TOKEN": "tok"})
        self.assertIn("# Right-click the channel", text, "comments are kept")

    def test_secrets_already_in_env_are_not_asked_again(self) -> None:
        template = (REPO / "plugin/email/config.example.toml").read_text()
        asker = ScriptedAsker({"EMAIL_ADDRESS": "me@gmail.com"})

        text, found = setup.fill_template(template, asker, {"EMAIL_PASSWORD": "set"}, "Plugin email")

        self.assertEqual(tomllib.loads(text)["env"]["EMAIL_ADDRESS"], "me@gmail.com")
        self.assertEqual(found, {})
        self.assertFalse(any("EMAIL_PASSWORD" in question for question in asker.asked))


class ServerConfigTests(unittest.TestCase):

    def test_public_url_and_model_are_set_and_a_keyless_llm_needs_no_key(self) -> None:
        template = (REPO / "clankjob.example.toml").read_text()
        asker = ScriptedAsker({"Public URL": "https://cj.example.com", "time zone": "America/Toronto",
                               "LLM endpoint": "http://ollama:11434/v1", "Default model": "qwen3:14b"})

        text, found = setup.server_config(template, asker, {})

        config = tomllib.loads(text)
        self.assertEqual(config["public_url"], "https://cj.example.com")
        self.assertEqual(config["timezone"], "America/Toronto")
        self.assertEqual((config["llm"]["default"]["base_url"], config["llm"]["default"]["model"]),
                         ("http://ollama:11434/v1", "qwen3:14b"))
        self.assertNotIn("api_key", config["llm"]["default"])
        self.assertEqual(found, {})

    def test_an_unknown_time_zone_is_refused(self) -> None:
        template = (REPO / "clankjob.example.toml").read_text()

        with self.assertRaises(setup.SetupError):
            _ = setup.server_config(template, ScriptedAsker({"time zone": "Mars/Olympus"}), {})


class FilesTests(unittest.TestCase):

    def test_env_keeps_existing_values_quotes_when_needed_and_is_private(self) -> None:
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / ".env"
            _ = path.write_text("CLANKJOB_TOKEN=kept\n")

            added = setup.add_env(path, {"CLANKJOB_TOKEN": "new", "EMAIL_PASSWORD": "a b#c", "EMPTY": ""})
            setup.set_env(path, "CLANKJOB_TAG", "1.2")
            setup.set_env(path, "CLANKJOB_TAG", "1.3")

            self.assertEqual(added, ["EMAIL_PASSWORD"])
            self.assertEqual(path.read_text(), "CLANKJOB_TOKEN=kept\nEMAIL_PASSWORD='a b#c'\nCLANKJOB_TAG=1.3\n")
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_the_share_is_found_in_a_checkout_with_every_plugin(self) -> None:
        share = Share.find(REPO)

        self.assertEqual(share, SHARE)
        self.assertIn("email", share.bundled())
        self.assertNotIn("__pycache__", share.bundled())


def arguments(target: Path, command: str = "setup", **overrides: object) -> argparse.Namespace:
    values: dict[str, object] = {"command": command, "dir": target, "share": REPO, "tag": None, "port": None,
                                 "configure": None, "yes": False}
    values.update(overrides)
    return argparse.Namespace(**values)


class SetupTests(unittest.TestCase):

    def test_a_full_setup_then_a_rerun_and_an_update_that_keep_everything(self) -> None:
        # Arrange
        with tempfile.TemporaryDirectory() as work:
            target = Path(work)
            asker = ScriptedAsker({"LLM API key": "sk-or-1", "Configure the email": "y",
                                   "EMAIL_ADDRESS": "me@gmail.com", "EMAIL_PASSWORD": "app-pw"})

            # Act
            setup.setup(arguments(target), SHARE, asker)
            first_env = (target / ".env").read_text()
            setup.setup(arguments(target), SHARE, ScriptedAsker({}))
            _ = setup.prepare(arguments(target, "update"), SHARE)

            # Assert
            for path in ("compose.yaml", "update", "config/clankjob.toml", ".env", "data", "plugins",
                         "prompts/profiles", "config/plugins/email.toml"):
                self.assertTrue((target / path).exists(), path)
            self.assertEqual((target / "compose.yaml").read_text(), SHARE.compose.read_text())
            self.assertTrue(os.access(target / "update", os.X_OK))
            self.assertEqual(list((target / "plugins").iterdir()), [], "the plugins come with the image")
            self.assertFalse((target / "config/plugins/discord.toml").exists(), "not configured, not written")
            env = setup.read_env(target / ".env")
            self.assertEqual((env["OPENROUTER_API_KEY"], env["EMAIL_PASSWORD"]), ("sk-or-1", "app-pw"))
            self.assertEqual((env["CLANKJOB_TAG"], env["CLANKJOB_PORT"]), ("latest", "8080"))
            self.assertEqual(env["UID"], str(os.getuid()))
            self.assertGreater(len(env["CLANKJOB_TOKEN"]), 20)
            self.assertEqual((target / ".env").read_text(), first_env, "a rerun or an update changes no secret")
            email = tomllib.loads((target / "config/plugins/email.toml").read_text())
            self.assertEqual(email["env"]["EMAIL_ADDRESS"], "me@gmail.com")

    def test_update_asks_nothing_and_takes_a_new_tag_and_port(self) -> None:
        with tempfile.TemporaryDirectory() as work:
            target = Path(work)
            setup.setup(arguments(target, yes=True), SHARE, Asker(assume_defaults=True))
            _ = (target / "compose.yaml").write_text("image: x:${CLANKJOB_TAG}  # an edit for compose.override.yaml\n")

            _ = setup.prepare(arguments(target, "update", tag="1.2", port=9000), SHARE)

            env = setup.read_env(target / ".env")
            self.assertEqual((env["CLANKJOB_TAG"], env["CLANKJOB_PORT"]), ("1.2", "9000"))
            self.assertEqual((target / "compose.yaml").read_text(), SHARE.compose.read_text())

    def test_a_directory_set_up_by_deploy_py_is_converted(self) -> None:
        # Arrange: copies of the plugins with their settings inside, and a compose.yaml
        # with the port and tag written into it.
        with tempfile.TemporaryDirectory() as work:
            target = Path(work)
            _ = shutil.copytree(REPO / "plugin" / "email", target / "plugins" / "email",
                                ignore=shutil.ignore_patterns("config.toml", "__pycache__"))
            _ = (target / "plugins/email/config.toml").write_text("[env]\nEMAIL_ADDRESS = \"me@x.ca\"\n")
            _ = shutil.copytree(REPO / "plugin" / "weather", target / "plugins" / "mine",
                                ignore=shutil.ignore_patterns("__pycache__"))
            _ = (target / "plugins/mine/plugin.toml").write_text('id = "mine"\nruntime = "command"\n')
            _ = (target / "clankjob.toml").write_text("# mine\n")
            _ = (target / "compose.yaml").write_text(
                'services:\n  clankjob:\n    image: ghcr.io/uintptr/clankjob:1.2\n'
                + '    ports:\n      - "127.0.0.1:8081:8080"\n')
            _ = (target / ".env").write_text("CLANKJOB_TOKEN=kept\n")

            # Act
            _ = setup.prepare(arguments(target, "update"), SHARE)

            # Assert
            self.assertEqual((target / "config/clankjob.toml").read_text(), "# mine\n")
            self.assertIn("me@x.ca", (target / "config/plugins/email.toml").read_text())
            self.assertTrue((target / "plugins.old/email/email_tool.py").is_file())
            self.assertEqual(sorted(path.name for path in (target / "plugins").iterdir()), ["mine"],
                             "a plugin of your own stays")
            self.assertTrue((target / "compose.yaml.old").is_file())
            env = setup.read_env(target / ".env")
            self.assertEqual((env["CLANKJOB_TAG"], env["CLANKJOB_PORT"], env["CLANKJOB_TOKEN"]),
                             ("1.2", "8081", "kept"))

    def test_settings_mounted_by_hand_are_moved_to_config(self) -> None:
        with tempfile.TemporaryDirectory() as work:
            target = Path(work)
            (target / "plugins").mkdir()
            _ = (target / "plugins/discord.toml").write_text("# discord\n")

            done, _ = setup.convert_old_layout(target, SHARE.bundled())

            self.assertEqual((target / "config/plugins/discord.toml").read_text(), "# discord\n")
            self.assertEqual(done, ["plugins/discord.toml moved to config/plugins/"])

    def test_a_missing_directory_says_to_mount_one(self) -> None:
        with self.assertRaisesRegex(setup.SetupError, "mount your setup directory"):
            _ = setup.prepare(arguments(Path("/nonexistent/setup")), SHARE)


if __name__ == "__main__":
    unittest.main()
