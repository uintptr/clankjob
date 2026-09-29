#!/usr/bin/env python3
"""Set up a directory to run clankjob with Docker Compose, from the published image.

Downloads the repository once (or uses a local checkout), then in the target directory:

    compose.yaml      runs ghcr.io/uintptr/clankjob, with ./plugins and ./prompts mounted
    clankjob.toml     server settings: public URL, LLM, model (asked)
    .env              secrets (chmod 600): API token (generated), LLM key, plugin secrets
    plugins/<id>/     every plugin of the repository; config.toml for those you configure
    prompts/          your prompt overrides and profiles (empty)
    data/             the database and case files

It asks for each value, with the example's value as the default; secrets are typed
hidden and only ever written to .env. Running it again updates the plugins and keeps
everything you set: an existing config.toml, clankjob.toml, compose.yaml or .env value
is never overwritten.

    curl -fsSLO https://raw.githubusercontent.com/uintptr/clankjob/main/deploy/deploy.py
    python3 deploy.py ~/clankjob
    python3 deploy.py ~/clankjob --configure email      # configure one plugin later
    python3 deploy.py ~/clankjob --yes                  # no questions: defaults only
"""

import argparse
import getpass
import json
import re
import secrets
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from collections.abc import Callable
from pathlib import Path, PurePosixPath

REPO = "uintptr/clankjob"
IMAGE = "ghcr.io/uintptr/clankjob"
# One line of a TOML file this script can fill in: `key = value  # comment`.
VALUE_LINE = re.compile(r'^(?P<indent>\s*)(?P<key>[A-Za-z0-9_-]+)\s*=\s*(?P<value>"[^"]*"|\[[^\]]*\]|\{[^}]*\}'
                        r'|true|false|-?\d+(?:\.\d+)?)\s*(?P<comment>#.*)?$')
ENV_REF = re.compile(r'^\{\s*env\s*=\s*"(?P<name>[A-Za-z_][A-Za-z0-9_]*)"\s*\}$')
SECRET_REF = re.compile(r'^\{\s*secret\s*=\s*"(?P<name>[^"]+)"\s*\}$')
SECTION = re.compile(r"^\s*\[(?P<name>[^\]]+)\]\s*$")


class SetupError(Exception):
    """Stops the setup with a message."""


# ---------------------------------------------------------------- asking


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
        return getpass.getpass(f"{question} (hidden): ").strip()

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
                file.write(f"{key}={env_value(new[key])}\n")
    if path.exists():
        path.chmod(0o600)
    return added


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


# ---------------------------------------------------------------- the pieces


def fetch_source(ref: str, local: Path | None, work: Path) -> Path:
    """The repository: a local checkout, or `ref` downloaded as one archive."""
    if local is not None:
        if not (local / "plugin").is_dir() or not (local / "deploy" / "compose.yaml").is_file():
            raise SetupError(f"{local} is not a clankjob checkout")
        return local
    url = f"https://codeload.github.com/{REPO}/tar.gz/{ref}"
    print(f"Downloading {REPO} ({ref})…")
    archive = work / "source.tar.gz"
    try:
        with urllib.request.urlopen(url, timeout=60) as response, archive.open("wb") as file:
            shutil.copyfileobj(response, file)
    except OSError as error:
        raise SetupError(f"cannot download {url}: {error}") from error
    wanted = ("plugin/", "deploy/compose.yaml", "clankjob.example.toml")
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            parts = PurePosixPath(member.name).parts
            inner = "/".join(parts[1:])
            if not (member.isfile() or member.isdir()) or ".." in parts or not inner.startswith(wanted):
                continue
            target = work / "source" / inner
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            source = tar.extractfile(member)
            if source is not None:
                target.write_bytes(source.read())
                target.chmod(member.mode & 0o755 | 0o644)
    return work / "source"


def copy_plugins(source: Path, target: Path) -> list[str]:
    """Copy every plugin's files, except config.toml: an existing one is kept untouched."""
    plugins = sorted(path for path in (source / "plugin").iterdir() if (path / "plugin.toml").is_file())
    for plugin in plugins:
        destination = target / plugin.name
        for file in plugin.rglob("*"):
            relative = file.relative_to(plugin)
            if "__pycache__" in relative.parts or file.suffix == ".pyc":
                continue
            copied = destination / relative
            if file.is_dir():
                copied.mkdir(parents=True, exist_ok=True)
            elif relative != Path("config.toml"):
                # A source's own config.toml (a local checkout's settings) is never copied:
                # settings come from the example, filled in here, or are kept as they are.
                copied.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(file, copied)
    return [plugin.name for plugin in plugins]


def compose_file(template: str, port: int, tag: str) -> str:
    """compose.yaml for this setup: the plugins and prompts directories mounted."""
    text = template.replace(f"image: {IMAGE}:latest", f"image: {IMAGE}:{tag}")
    text = text.replace('"127.0.0.1:8080:8080"', f'"127.0.0.1:{port}:8080"')
    kept: list[str] = []
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith(("# - ./plugins/", "# Settings of the bundled plugins",
                                "# lines of plugins you configured")):
            continue
        if stripped == "# - ./prompts:/prompts:ro":
            indent = line[: len(line) - len(line.lstrip())]
            kept.append(f"{indent}- ./prompts:/prompts:ro")
            continue
        kept.append(line)
        if stripped == "- ./data:/data":
            indent = line[: len(line) - len(line.lstrip())]
            kept.append(f"{indent}# The plugins, set up by deploy.py; settings in plugins/<id>/config.toml.")
            kept.append(f"{indent}- ./plugins:/plugins:ro")
    return "\n".join(kept) + "\n"


def server_config(template: str, asker: Asker, env: dict[str, str]) -> tuple[str, dict[str, str]]:
    """clankjob.toml from the example: public URL, LLM endpoint, model and its key."""
    print("\nServer (clankjob.toml)")
    text = template
    public_url = asker.ask("  Public URL, e.g. https://clank.example.com (empty: none)", "")
    if public_url:
        text = set_value(text, None, "public_url", toml_string(public_url))
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


def configure_plugin(plugin: Path, asker: Asker, env: dict[str, str]) -> dict[str, str]:
    example = plugin / "config.example.toml"
    text, found = fill_template(example.read_text(encoding="utf-8"), asker, env,
                                f"Plugin {plugin.name} ({plugin / 'config.toml'})")
    (plugin / "config.toml").write_text(text, encoding="utf-8")
    return found


# ---------------------------------------------------------------- main


def setup(args: argparse.Namespace, asker: Asker, run: Callable[[list[str]], int]) -> None:
    target: Path = args.dir.expanduser().resolve()
    target.mkdir(parents=True, exist_ok=True)
    for directory in ("data", "prompts/profiles", "plugins"):
        (target / directory).mkdir(parents=True, exist_ok=True)
    env_path = target / ".env"
    env = read_env(env_path)
    new_env: dict[str, str] = {}

    with tempfile.TemporaryDirectory(prefix="clankjob-deploy-") as work:
        source = fetch_source(args.ref, args.source, Path(work))
        names = copy_plugins(source, target / "plugins")
        print(f"Plugins: {', '.join(names)} (in {target / 'plugins'})")

        compose = target / "compose.yaml"
        if compose.exists():
            print("compose.yaml: kept (delete it to regenerate)")
        else:
            compose.write_text(compose_file((source / "deploy" / "compose.yaml").read_text(encoding="utf-8"),
                                            args.port, args.image_tag), encoding="utf-8")
            print("compose.yaml: written")

        config = target / "clankjob.toml"
        if config.exists():
            print("clankjob.toml: kept")
        else:
            text, found = server_config((source / "clankjob.example.toml").read_text(encoding="utf-8"), asker, env)
            config.write_text(text, encoding="utf-8")
            new_env.update(found)
            print("clankjob.toml: written")

    if "CLANKJOB_TOKEN" not in env:
        new_env["CLANKJOB_TOKEN"] = secrets.token_urlsafe(24)
        print(f"\nAPI token for signing in to the web UI (also in .env): {new_env['CLANKJOB_TOKEN']}")

    wanted = set(args.configure or [])
    for name in names:
        plugin = target / "plugins" / name
        if not (plugin / "config.example.toml").is_file() or (plugin / "config.toml").exists():
            continue
        if name in wanted or (not wanted and asker.yes(f"\nConfigure the {name} plugin now?", False)):
            new_env.update(configure_plugin(plugin, asker, {**env, **new_env}))
    unknown = wanted - set(names)
    if unknown:
        raise SetupError(f"no plugin named {', '.join(sorted(unknown))}; plugins: {', '.join(names)}")

    added = add_env(env_path, new_env)
    if added:
        print(f".env: added {', '.join(added)}")

    print(f"\nReady in {target}. Next:")
    print(f"  cd {target} && docker compose up -d")
    print(f"  then open http://127.0.0.1:{args.port} and sign in with CLANKJOB_TOKEN from .env")
    print("  check a plugin:  docker compose exec clankjob /plugins/<id>/check_config.py")
    if shutil.which("docker") and asker.yes("\nStart it now (docker compose up -d)?", False):
        status = run(["docker", "compose", "--project-directory", str(target), "up", "-d"])
        if 0 != status:
            raise SetupError("docker compose up failed; see its output above")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("dir", type=Path, nargs="?", default=Path(
        "clankjob"), help="where to set up (default ./clankjob)")
    parser.add_argument("--ref", default="main", help="branch, tag or commit to take the plugins from (default main)")
    parser.add_argument("--source", type=Path, help="a local clankjob checkout instead of downloading")
    parser.add_argument("--image-tag", default="latest", help="image tag, e.g. 1.2 (default latest)")
    parser.add_argument("--port", type=int, default=8080, help="local port for the web UI and API (default 8080)")
    parser.add_argument("--configure", action="append", metavar="PLUGIN", help="configure this plugin (repeatable)")
    parser.add_argument("--yes", action="store_true", help="ask nothing: take defaults, configure no plugin")
    args = parser.parse_args()
    asker = Asker(assume_defaults=args.yes or not sys.stdin.isatty())
    try:
        setup(args, asker, lambda command: subprocess.run(command, check=False).returncode)
    except SetupError as error:
        print(f"failed: {error}", file=sys.stderr)
        return 1
    except (KeyboardInterrupt, EOFError):
        print("\ninterrupted; run it again to continue (nothing you set is overwritten)", file=sys.stderr)
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
