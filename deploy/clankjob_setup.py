#!/usr/bin/env python3
"""Set up or update a directory that runs clankjob with Docker Compose.

It runs inside the image, on the directory mounted at /setup, so what it writes always
matches the image; `install.sh` and `./update` start it:

    docker run --rm -it --user "$(id -u):$(id -g)" -v "$PWD:/setup" ghcr.io/uintptr/clankjob setup
    docker compose run --rm --no-deps -v "$PWD:/setup" clankjob update

The directory holds only your settings and data; the plugins come with the image:

    compose.yaml              the image's own, replaced by every update: never edit it
    compose.override.yaml     your changes to it, if any (docker compose merges it in)
    update                    pulls the image and restarts the containers on it
    .env                      CLANKJOB_TAG, CLANKJOB_PORT, UID, GID and secrets (chmod 600)
    config/clankjob.toml      server settings
    config/plugins/<id>.toml  settings of the plugins you configured
    plugins/                  your own plugins, if any (they replace bundled ones by id)
    prompts/                  your prompt overrides and profiles
    data/                     the database, case files and database backups

`setup` asks for what is missing (the server settings, and each plugin you want to
configure); `update` asks nothing. Neither ever overwrites a setting or secret you have.
Both first convert a directory set up by the former deploy.py or by hand: settings move
to config/, copies of bundled plugins to plugins.old/, and the old compose.yaml's port and
image tag to .env (the file itself is kept as compose.yaml.old).

From a checkout, without Docker (e.g. to try it): python3 deploy/clankjob_setup.py setup --dir ~/cj
"""

import argparse
import getpass
import json
import os
import re
import secrets
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

try:
    # Line editing for input(): without it, a terminal whose Backspace sends ^H while the
    # tty expects ^? (DEL) echoes "^H" and keeps it in the answer.
    import readline  # noqa: F401  # pyright: ignore[reportUnusedImport]
except ImportError:
    pass

IMAGE = "ghcr.io/uintptr/clankjob"
DEFAULT_TAG = "latest"
DEFAULT_PORT = 8080
# Where the image keeps its plugins and templates (Dockerfile).
IMAGE_SHARE = Path("/usr/share/clankjob")
CHECKOUT = Path(__file__).resolve().parent.parent
# One line of a TOML file this script can fill in: `key = value  # comment`.
VALUE_LINE = re.compile(r'^(?P<indent>\s*)(?P<key>[A-Za-z0-9_-]+)\s*=\s*(?P<value>"[^"]*"|\[[^\]]*\]|\{[^}]*\}'
                        + r'|true|false|-?\d+(?:\.\d+)?)\s*(?P<comment>#.*)?$')
ENV_REF = re.compile(r'^\{\s*env\s*=\s*"(?P<name>[A-Za-z_][A-Za-z0-9_]*)"\s*\}$')
SECRET_REF = re.compile(r'^\{\s*secret\s*=\s*"(?P<name>[^"]+)"\s*\}$')
SECTION = re.compile(r"^\s*\[(?P<name>[^\]]+)\]\s*$")


class SetupError(Exception):
    """Stops the setup with a message."""


# ---------------------------------------------------------------- asking


def erase_backspaces(text: str) -> str:
    """`text` with each backspace (^H or DEL) removing the character before it, as typed.

    getpass reads with echo off and without readline, so a Backspace the tty does not
    treat as its erase character arrives in the answer instead of deleting."""
    kept: list[str] = []
    for character in text:
        if character in "\b\x7f":
            if kept:
                kept.pop()
        else:
            kept.append(character)
    return "".join(kept)


class Asker:
    """Asks the user; with `assume_defaults`, takes every default without asking."""

    def __init__(self, assume_defaults: bool = False) -> None:
        self.assume_defaults = assume_defaults

    def ask(self, question: str, default: str = "") -> str:
        if self.assume_defaults:
            return default
        shown = f" [{default}]" if default else ""
        answer = input(f"{question}{shown}: ").strip()
        return answer or default

    def secret(self, question: str) -> str:
        if self.assume_defaults:
            return ""
        return erase_backspaces(getpass.getpass(f"{question} (hidden): ")).strip()

    def yes(self, question: str, default: bool = False) -> bool:
        if self.assume_defaults:
            return default
        answer = input(f"{question} [{'Y/n' if default else 'y/N'}]: ").strip().lower()
        return default if not answer else answer.startswith("y")


# ---------------------------------------------------------------- .env


def read_env(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    if path.is_file():
        for line in path.read_text(encoding="utf-8").splitlines():
            if "=" in line and not line.lstrip().startswith("#"):
                key, value = line.split("=", 1)
                values[key.strip()] = value.strip()
    return values


def env_value(value: str) -> str:
    """A value as docker compose reads it from an env file: quoted when it needs to be."""
    if not re.search(r"[\s#'\"\\$]", value):
        return value
    if "'" not in value:
        return f"'{value}'"
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"').replace("$", "\\$") + '"'


def add_env(path: Path, new: dict[str, str]) -> list[str]:
    """Append the variables `.env` does not have yet; never change existing ones."""
    existing = read_env(path)
    added = [key for key in new if key not in existing and new[key]]
    if added:
        with path.open("a", encoding="utf-8") as file:
            for key in added:
                _ = file.write(f"{key}={env_value(new[key])}\n")
    if path.exists():
        path.chmod(0o600)
    return added


def set_env(path: Path, key: str, value: str) -> None:
    """Set one variable in `.env`, replacing its line or appending it: for values the user
    changes explicitly (`--tag`, `--port`)."""
    lines = path.read_text(encoding="utf-8").splitlines() if path.is_file() else []
    line = f"{key}={env_value(value)}"
    pattern = re.compile(rf"^\s*{re.escape(key)}\s*=")
    replaced = [line if pattern.match(old) else old for old in lines]
    if replaced == lines:
        replaced.append(line)
    _ = path.write_text("\n".join(replaced) + "\n", encoding="utf-8")
    path.chmod(0o600)


# ---------------------------------------------------------------- filling in TOML


def toml_string(value: str) -> str:
    # JSON strings are valid TOML basic strings.
    return json.dumps(value, ensure_ascii=False)


def fill_template(template: str, asker: Asker, env: dict[str, str], title: str) -> tuple[str, dict[str, str]]:
    """Walk through a config template, asking for every active value.

    Strings, lists of strings, booleans and numbers are asked with the template's value
    as default. `{ env = "NAME" }` values are asked hidden and returned for `.env`
    (unless `.env` already has them); the line itself stays a reference.

    # Returns

    The filled-in text and the secrets for `.env`
    """
    secrets_found: dict[str, str] = {}
    out: list[str] = []
    section = ""
    print(f"\n{title}")
    for line in template.splitlines():
        header = SECTION.match(line)
        if header:
            section = header.group("name")
            out.append(line)
            continue
        match = VALUE_LINE.match(line)
        if match is None:
            out.append(line)
            continue
        key, value = match.group("key"), match.group("value")
        label = f"  {section}.{key}" if section and section != "env" else f"  {key}"
        comment = f"  {match.group('comment')}" if match.group("comment") else ""
        env_ref = ENV_REF.match(value)
        if env_ref:
            name = env_ref.group("name")
            if name not in env and name not in secrets_found:
                secrets_found[name] = asker.secret(f"{label} → {name}")
            out.append(line)
            continue
        if SECRET_REF.match(value):
            print(f"{label}: read from the secrets directory ({value}); left as is")
            out.append(line)
            continue
        if value.startswith("["):
            items = re.findall(r'"([^"]*)"', value)
            answer = asker.ask(f"{label} (comma-separated)", ", ".join(items))
            parts = [part.strip() for part in answer.split(",") if part.strip()]
            new_value = "[" + ", ".join(toml_string(part) for part in parts) + "]"
        elif value.startswith('"'):
            new_value = toml_string(asker.ask(label, value[1:-1]))
        elif value in ("true", "false"):
            new_value = "true" if asker.yes(label, value == "true") else "false"
        else:
            answer = asker.ask(label, value)
            if not re.fullmatch(r"-?\d+(\.\d+)?", answer):
                raise SetupError(f"{key} must be a number, got {answer!r}")
            new_value = answer
        out.append(f"{match.group('indent')}{key} = {new_value}{comment}")
    return "\n".join(out) + "\n", secrets_found


def set_value(text: str, section: str | None, key: str, value: str) -> str:
    """Set `key = value` in `section` (None: before any section), uncommenting it if needed."""
    lines = text.splitlines()
    current: str | None = None
    pattern = re.compile(rf"^\s*#?\s*{re.escape(key)}\s*=")
    for index, line in enumerate(lines):
        header = SECTION.match(line)
        if header:
            current = header.group("name")
            continue
        if current == section and pattern.match(line):
            comment = re.search(r"\s(#[^\"]*)$", line.split("=", 1)[1])
            suffix = f"  {comment.group(1)}" if comment and not line.lstrip().startswith("#") else ""
            lines[index] = f"{key} = {value}{suffix}"
            return "\n".join(lines) + "\n"
    raise SetupError(f"no `{key}` in [{section or 'top'}] of the template")


def comment_out(text: str, section: str, key: str) -> str:
    lines = text.splitlines()
    current = None
    for index, line in enumerate(lines):
        header = SECTION.match(line)
        if header:
            current = header.group("name")
        elif current == section and re.match(rf"^\s*{re.escape(key)}\s*=", line):
            lines[index] = f"# {line}"
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------- what the image brings


@dataclass(frozen=True)
class Share:
    """The plugins and templates this setup comes from: the image's, or a checkout's."""

    plugins: Path
    compose: Path
    update: Path
    server_example: Path

    @staticmethod
    def image(root: Path) -> "Share":
        return Share(root / "plugins", root / "compose.yaml", root / "update", root / "clankjob.example.toml")

    @staticmethod
    def checkout(root: Path) -> "Share":
        return Share(root / "plugin", root / "deploy" / "compose.yaml", root / "deploy" / "update",
                     root / "clankjob.example.toml")

    @staticmethod
    def find(given: Path | None) -> "Share":
        """`given`, else the image's, else the checkout this script is in."""
        for root in [given] if given else [IMAGE_SHARE, CHECKOUT]:
            for share in (Share.image(root), Share.checkout(root)):
                if share.compose.is_file() and share.plugins.is_dir():
                    return share
        raise SetupError(f"no plugins and templates in {given or IMAGE_SHARE}: run this from the image or a checkout")

    def bundled(self) -> dict[str, Path]:
        """Every plugin by id."""
        return {path.name: path for path in sorted(self.plugins.iterdir()) if (path / "plugin.toml").is_file()}


# ---------------------------------------------------------------- converting an old setup


def move(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    _ = shutil.move(str(source), str(destination))


def convert_old_layout(target: Path, bundled: dict[str, Path]) -> tuple[list[str], dict[str, str]]:
    """Bring a directory set up by the former deploy.py or by hand to this layout.

    # Returns

    What was done, and the `.env` values the old compose.yaml had (port and image tag)
    """
    done: list[str] = []
    settings = target / "config" / "plugins"
    if (target / "clankjob.toml").is_file() and not (target / "config" / "clankjob.toml").exists():
        move(target / "clankjob.toml", target / "config" / "clankjob.toml")
        done.append("clankjob.toml moved to config/clankjob.toml")
    for plugin_id in bundled:
        # By hand, per plugin: plugins/<id>.toml mounted on the plugin's config.toml.
        loose = target / "plugins" / f"{plugin_id}.toml"
        if loose.is_file() and not (settings / f"{plugin_id}.toml").exists():
            move(loose, settings / f"{plugin_id}.toml")
            done.append(f"plugins/{plugin_id}.toml moved to config/plugins/")
        # deploy.py: a copy of every plugin, its settings inside.
        copy = target / "plugins" / plugin_id
        if not (copy / "plugin.toml").is_file():
            continue
        if (copy / "config.toml").is_file() and not (settings / f"{plugin_id}.toml").exists():
            move(copy / "config.toml", settings / f"{plugin_id}.toml")
            done.append(f"plugins/{plugin_id}/config.toml moved to config/plugins/{plugin_id}.toml")
        move(copy, target / "plugins.old" / plugin_id)
    if (target / "plugins.old").is_dir():
        done.append("copies of bundled plugins moved to plugins.old/ (the image has them; delete it when all works)")
    compose = target / "compose.yaml"
    old_env: dict[str, str] = {}
    if compose.is_file() and "${CLANKJOB_TAG" not in compose.read_text(encoding="utf-8"):
        text = compose.read_text(encoding="utf-8")
        port = re.search(r'"(?:[\d.]+:)?(\d+):8080"', text)
        tag = re.search(rf"image: {re.escape(IMAGE)}:([^\s\"']+)", text)
        if port:
            old_env["CLANKJOB_PORT"] = port.group(1)
        if tag:
            old_env["CLANKJOB_TAG"] = tag.group(1)
        move(compose, target / "compose.yaml.old")
        done.append("compose.yaml kept as compose.yaml.old: carry any changes of yours over to compose.override.yaml")
    return done, old_env


# ---------------------------------------------------------------- the pieces


def install_files(target: Path, share: Share) -> list[str]:
    """compose.yaml and the update script, replaced by the image's own."""
    changed: list[str] = []
    for source, name, mode in ((share.compose, "compose.yaml", 0o644), (share.update, "update", 0o755)):
        destination = target / name
        text = source.read_text(encoding="utf-8")
        if not destination.is_file() or destination.read_text(encoding="utf-8") != text:
            _ = destination.write_text(text, encoding="utf-8")
            changed.append(name)
        destination.chmod(mode)
    return changed


def server_config(template: str, asker: Asker, env: dict[str, str]) -> tuple[str, dict[str, str]]:
    """clankjob.toml from the example: public URL, time zone, LLM endpoint, model and its key."""
    print("\nServer (config/clankjob.toml)")
    text = template
    public_url = asker.ask("  Public URL, e.g. https://clank.example.com (empty: none)", "")
    if public_url:
        text = set_value(text, None, "public_url", toml_string(public_url))
    timezone = asker.ask("  Your time zone, e.g. America/Toronto", "UTC")
    try:
        _ = ZoneInfo(timezone)
    except (ValueError, ZoneInfoNotFoundError) as error:
        raise SetupError(f"{timezone!r} is not a time zone like America/Toronto") from error
    text = set_value(text, None, "timezone", toml_string(timezone))
    base_url = asker.ask("  LLM endpoint (OpenAI-compatible)", "https://openrouter.ai/api/v1")
    text = set_value(text, "llm.default", "base_url", toml_string(base_url))
    model = asker.ask("  Default model", "openai/gpt-4.1-mini")
    text = set_value(text, "llm.default", "model", toml_string(model))
    found: dict[str, str] = {}
    if "OPENROUTER_API_KEY" not in env:
        key = asker.secret("  LLM API key (empty for a local model without one)")
        if key:
            found["OPENROUTER_API_KEY"] = key
        else:
            text = comment_out(text, "llm.default", "api_key")
    return text, found


def configure_plugins(target: Path, bundled: dict[str, Path], asker: Asker, wanted: set[str],
                      env: dict[str, str]) -> dict[str, str]:
    """Write config/plugins/<id>.toml for the plugins wanted (or, when none is named, those
    the user says yes to) that have none yet.

    # Returns

    The secrets they need, for `.env`
    """
    unknown = wanted - set(bundled)
    if unknown:
        raise SetupError(f"no plugin named {', '.join(sorted(unknown))}; plugins: {', '.join(bundled)}")
    found: dict[str, str] = {}
    for plugin_id, plugin in bundled.items():
        example = plugin / "config.example.toml"
        settings = target / "config" / "plugins" / f"{plugin_id}.toml"
        if not example.is_file() or settings.exists():
            continue
        if plugin_id in wanted or (not wanted and asker.yes(f"\nConfigure the {plugin_id} plugin now?", False)):
            text, secrets_needed = fill_template(example.read_text(encoding="utf-8"), asker, {**env, **found},
                                                 f"Plugin {plugin_id} (config/plugins/{plugin_id}.toml)")
            _ = settings.write_text(text, encoding="utf-8")
            found.update(secrets_needed)
    return found


def defaults(env: dict[str, str], old_env: dict[str, str]) -> dict[str, str]:
    """The `.env` values compose.yaml reads, when missing: the old compose.yaml's port and
    tag, else the defaults; the user and group running this (who owns the directory)."""
    values = {"CLANKJOB_TAG": old_env.get("CLANKJOB_TAG", DEFAULT_TAG),
              "CLANKJOB_PORT": old_env.get("CLANKJOB_PORT", str(DEFAULT_PORT))}
    # Run as root (no --user), the owner is unknown: compose's default (1000) stays.
    if hasattr(os, "getuid") and os.getuid() != 0:
        values.update({"UID": str(os.getuid()), "GID": str(os.getgid())})
    return {key: value for key, value in values.items() if key not in env}


# ---------------------------------------------------------------- main


def prepare(args: argparse.Namespace, share: Share) -> tuple[Path, dict[str, Path], dict[str, str]]:
    """Convert an old layout, create the directories, write compose.yaml and the update
    script, and add the missing `.env` defaults: everything `setup` and `update` share.

    # Returns

    The directory, the bundled plugins, and `.env` as it is now
    """
    target: Path = args.dir.expanduser().resolve()
    if not target.is_dir():
        raise SetupError(f"{target} does not exist: mount your setup directory there (-v \"$PWD:/setup\")")
    bundled = share.bundled()
    converted, old_env = convert_old_layout(target, bundled)
    for note in converted:
        print(f"converted: {note}")
    for directory in ("config/plugins", "data", "plugins", "prompts/profiles"):
        (target / directory).mkdir(parents=True, exist_ok=True)
    changed = install_files(target, share)
    print(f"{', '.join(changed)}: written from the image" if changed else "compose.yaml: up to date")
    env_path = target / ".env"
    added = add_env(env_path, defaults(read_env(env_path), old_env))
    for key, value in (("CLANKJOB_TAG", args.tag), ("CLANKJOB_PORT", args.port and str(args.port))):
        if value:
            set_env(env_path, key, value)
            added.append(key)
    if added:
        print(f".env: set {', '.join(dict.fromkeys(added))}")
    return target, bundled, read_env(env_path)


def setup(args: argparse.Namespace, share: Share, asker: Asker) -> None:
    target, bundled, env = prepare(args, share)
    new_env: dict[str, str] = {}
    config = target / "config" / "clankjob.toml"
    if config.exists():
        print("config/clankjob.toml: kept")
    else:
        text, found = server_config(share.server_example.read_text(encoding="utf-8"), asker, env)
        _ = config.write_text(text, encoding="utf-8")
        new_env.update(found)
        print("config/clankjob.toml: written")
    if "CLANKJOB_TOKEN" not in env:
        new_env["CLANKJOB_TOKEN"] = secrets.token_urlsafe(24)
        print(f"\nAPI token for signing in to the web UI (also in .env): {new_env['CLANKJOB_TOKEN']}")
    new_env.update(configure_plugins(target, bundled, asker, set(args.configure or []), {**env, **new_env}))
    added = add_env(target / ".env", new_env)
    if added:
        print(f".env: added {', '.join(added)}")
    port = read_env(target / ".env").get("CLANKJOB_PORT", str(DEFAULT_PORT))
    print("\nReady. In the directory:")
    print("  docker compose up -d")
    print(f"  then open http://127.0.0.1:{port} and sign in with CLANKJOB_TOKEN from .env")
    print("  check a plugin:  docker compose exec clankjob /usr/share/clankjob/plugins/<id>/check_config.py")
    print("  update later:    ./update")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["setup", "update"],
                        help="setup: ask for what is missing; update: refresh compose.yaml, ask nothing")
    parser.add_argument("--dir", type=Path, default=Path("/setup"), help="the setup directory (default /setup)")
    parser.add_argument("--share", type=Path, help="plugins and templates (default: the image's, else this checkout)")
    parser.add_argument("--tag", help=f"image tag to run, e.g. 1.2 (saved in .env; default {DEFAULT_TAG})")
    parser.add_argument("--port", type=int, help=f"local port for the web UI and API (saved in .env; default {DEFAULT_PORT})")
    parser.add_argument("--configure", action="append", metavar="PLUGIN", help="configure this plugin (repeatable)")
    parser.add_argument("--yes", action="store_true", help="ask nothing: take defaults, configure no plugin")
    args = parser.parse_args()
    try:
        share = Share.find(args.share)
        if "update" == args.command:
            _ = prepare(args, share)
        else:
            setup(args, share, Asker(assume_defaults=args.yes or not sys.stdin.isatty()))
    except SetupError as error:
        print(f"failed: {error}", file=sys.stderr)
        return 1
    except (KeyboardInterrupt, EOFError):
        print("\ninterrupted; run it again to continue (nothing you set is overwritten)", file=sys.stderr)
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
