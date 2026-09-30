#!/usr/bin/env python3
"""Tests for deploy.py, against this checkout. Run: python3 -m unittest -v test_deploy.py"""

import argparse
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import tomllib

import deploy
from deploy import Asker

REPO = Path(__file__).resolve().parent.parent


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
        self.assertEqual("secret", deploy.erase_backspaces("sex\bcret"))
        self.assertEqual("secret", deploy.erase_backspaces("\x7fsecrex\x7ft"))
        self.assertEqual("", deploy.erase_backspaces("ab\b\b\b"))
        with mock.patch("getpass.getpass", return_value="tk_1x\b2 "):
            self.assertEqual("tk_12", Asker().secret("Token"))


class FillTemplateTests(unittest.TestCase):

    def test_discord_example_is_filled_and_its_token_goes_to_env(self) -> None:
        # Arrange
        template = (REPO / "plugin/discord/config.example.toml").read_text()
        asker = ScriptedAsker({"channel_id": "111", "allowed_responders": "222, 333",
                               "DISCORD_BOT_TOKEN": "tok", "mention": "n"})

        # Act
        text, found = deploy.fill_template(template, asker, {}, "Plugin discord")

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

        text, found = deploy.fill_template(template, asker, {"EMAIL_PASSWORD": "set"}, "Plugin email")

        self.assertEqual(tomllib.loads(text)["env"]["EMAIL_ADDRESS"], "me@gmail.com")
        self.assertEqual(found, {})
        self.assertFalse(any("EMAIL_PASSWORD" in question for question in asker.asked))


class ServerConfigTests(unittest.TestCase):

    def test_public_url_and_model_are_set_and_a_keyless_llm_needs_no_key(self) -> None:
        template = (REPO / "clankjob.example.toml").read_text()
        asker = ScriptedAsker({"Public URL": "https://cj.example.com", "LLM endpoint": "http://ollama:11434/v1",
                               "Default model": "qwen3:14b"})

        text, found = deploy.server_config(template, asker, {})

        config = tomllib.loads(text)
        self.assertEqual(config["public_url"], "https://cj.example.com")
        self.assertEqual((config["llm"]["default"]["base_url"], config["llm"]["default"]["model"]),
                         ("http://ollama:11434/v1", "qwen3:14b"))
        self.assertNotIn("api_key", config["llm"]["default"])
        self.assertEqual(found, {})


class FilesTests(unittest.TestCase):

    def test_compose_mounts_plugins_and_prompts_with_the_chosen_port_and_tag(self) -> None:
        text = deploy.compose_file((REPO / "deploy/compose.yaml").read_text(), 18090, "1.2")

        self.assertIn("image: ghcr.io/uintptr/clankjob:1.2", text)
        self.assertIn('"127.0.0.1:18090:8080"', text)
        self.assertIn("- ./plugins:/plugins:ro", text)
        self.assertIn("      - ./prompts:/prompts:ro", text)
        self.assertNotIn("./plugins/email.toml", text)

    def test_env_keeps_existing_values_quotes_when_needed_and_is_private(self) -> None:
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / ".env"
            path.write_text("CLANKJOB_TOKEN=kept\n")

            added = deploy.add_env(path, {"CLANKJOB_TOKEN": "new", "EMAIL_PASSWORD": "a b#c", "EMPTY": ""})

            self.assertEqual(added, ["EMAIL_PASSWORD"])
            self.assertEqual(path.read_text(), "CLANKJOB_TOKEN=kept\nEMAIL_PASSWORD='a b#c'\n")
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_plugins_are_copied_without_caches_and_configs_are_kept(self) -> None:
        with tempfile.TemporaryDirectory() as work:
            target = Path(work) / "plugins"
            (target / "email").mkdir(parents=True)
            (target / "email" / "config.toml").write_text("[env]\nEMAIL_ADDRESS = \"mine\"\n")

            names = deploy.copy_plugins(REPO, target)

            self.assertIn("email", names)
            self.assertTrue((target / "email" / "email_tool.py").is_file())
            self.assertIn("mine", (target / "email" / "config.toml").read_text())
            self.assertEqual(list(target.rglob("__pycache__")), [])


class SetupTests(unittest.TestCase):

    def test_without_a_directory_a_setup_updates_itself_and_elsewhere_gets_a_new_one(self) -> None:
        with tempfile.TemporaryDirectory() as work:
            here = Path(work)
            self.assertEqual(here / "clankjob", deploy.default_target(here))
            (here / "compose.yaml").write_text("services: {}\n")
            self.assertEqual(here / "clankjob", deploy.default_target(here))
            (here / "clankjob.toml").write_text("")
            self.assertEqual(here, deploy.default_target(here))

    def test_an_old_compose_is_updated_keeping_its_port_and_tag_and_backed_up(self) -> None:
        template = (REPO / "deploy/compose.yaml").read_text()
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / "compose.yaml"
            old = deploy.compose_file(template, 8081, "1.2").split("\n  sandbox:")[0] + "\n"
            path.write_text(old)
            self.assertEqual((8081, "1.2"), deploy.compose_settings(old))

            self.assertEqual(8081, deploy.update_compose(path, template, None, None, keep=False))
            self.assertIn("sandbox:", path.read_text())
            self.assertEqual((8081, "1.2"), deploy.compose_settings(path.read_text()))
            self.assertEqual(old, (Path(work) / "compose.yaml.bak").read_text())

            updated = path.read_text()
            deploy.update_compose(path, template, None, None, keep=False)
            self.assertEqual(updated, path.read_text(), "an up-to-date file is left alone")
            self.assertEqual(9000, deploy.update_compose(path, template, 9000, None, keep=True))
            self.assertEqual(updated, path.read_text(), "--keep-compose changes nothing")
            deploy.update_compose(path, template, 9000, "latest", keep=False)
            self.assertEqual((9000, "latest"), deploy.compose_settings(path.read_text()))

    def test_start_pulls_then_restarts_the_server_only_when_its_plugins_changed(self) -> None:
        with tempfile.TemporaryDirectory() as work, mock.patch("deploy.shutil.which", return_value="/usr/bin/docker"):
            target = Path(work) / "clankjob"
            args = argparse.Namespace(dir=target, ref="main", source=REPO, image_tag=None, port=None,
                                      keep_compose=False, configure=None, yes=True, start=True)
            commands: list[list[str]] = []

            def run(command: list[str]) -> int:
                commands.append(command[4:])
                return 0

            deploy.setup(args, Asker(assume_defaults=True), run)
            first = list(commands)
            commands.clear()
            deploy.setup(args, Asker(assume_defaults=True), run)
            unchanged = list(commands)
            commands.clear()
            (target / "plugins/weather/README.md").write_text("an older copy\n")
            deploy.setup(args, Asker(assume_defaults=True), run)

            self.assertEqual([["pull"], ["up", "-d"]], first, "a new setup has no server to restart")
            self.assertEqual([["pull"], ["up", "-d"]], unchanged)
            self.assertEqual([["pull"], ["up", "-d"], ["restart", "clankjob"]], commands)

    def test_a_full_setup_then_a_rerun_that_keeps_everything(self) -> None:
        # Arrange
        with tempfile.TemporaryDirectory() as work:
            target = Path(work) / "clankjob"
            args = argparse.Namespace(dir=target, ref="main", source=REPO, image_tag=None, port=None,
                                      keep_compose=False, configure=None, yes=False, start=False)
            asker = ScriptedAsker({"LLM API key": "sk-or-1", "Configure the email": "y",
                                   "EMAIL_ADDRESS": "me@gmail.com", "EMAIL_PASSWORD": "app-pw",
                                   "Start it now": "n"})

            # Act
            deploy.setup(args, asker, lambda _command: 0)
            first_env = (target / ".env").read_text()
            deploy.setup(args, ScriptedAsker({"Start it now": "n"}), lambda _command: 0)

            # Assert
            for path in ("compose.yaml", "clankjob.toml", ".env", "data", "prompts/profiles",
                         "plugins/email/config.toml", "plugins/documents/docs_tool.py"):
                self.assertTrue((target / path).exists(), path)
            self.assertFalse((target / "plugins/discord/config.toml").exists(), "not configured, not written")
            env = deploy.read_env(target / ".env")
            self.assertEqual((env["OPENROUTER_API_KEY"], env["EMAIL_PASSWORD"]), ("sk-or-1", "app-pw"))
            self.assertGreater(len(env["CLANKJOB_TOKEN"]), 20)
            self.assertEqual((target / ".env").read_text(), first_env, "a rerun changes no secret")
            email = tomllib.loads((target / "plugins/email/config.toml").read_text())
            self.assertEqual(email["env"]["EMAIL_ADDRESS"], "me@gmail.com")


if __name__ == "__main__":
    unittest.main()
