#!/usr/bin/env python3
"""Check the email plugin's settings against your real mail servers, without sending.

Reads config.toml the way the server does (`{ env = ... }` and `{ secret = ... }`
references included), then logs in to IMAP, lists the mailbox's folders and checks that
the configured ones exist, and logs in to SMTP. Nothing is sent, nothing is marked read,
and the password is never printed.

    export EMAIL_PASSWORD=...        # if config.toml uses { env = "EMAIL_PASSWORD" }
    ./check_config.py
    ./check_config.py --secrets-dir /run/secrets
"""

import argparse
import os
import sys
from dataclasses import dataclass
from pathlib import Path

import tomllib
from email_tool import ImapMailbox, Settings, SmtpSender, ToolError

HERE = Path(__file__).resolve().parent


@dataclass(frozen=True)
class Check:
    ok: bool
    what: str
    detail: str = ""


def resolve(value: object, name: str, secrets_dir: Path) -> str:
    """A config value: a literal, `{ env = "NAME" }` or `{ secret = "name" }`. Errors never show values."""
    if isinstance(value, (str, int)):
        return str(value)
    if isinstance(value, dict) and set(value) == {"env"}:
        variable = str(value["env"])
        resolved = os.environ.get(variable, "").strip()
        if "" == resolved:
            raise ToolError(f"{name}: environment variable {variable} is not set in this shell")
        return resolved
    if isinstance(value, dict) and set(value) == {"secret"}:
        secret = str(value["secret"])
        if "/" in secret or secret.startswith(".") or len(secret) > 64:
            raise ToolError(f"{name}: the secret name should name a file, not hold the secret itself")
        path = secrets_dir / secret
        if not path.is_file():
            raise ToolError(f"{name}: secret file {path} not found")
        return path.read_text(encoding="utf-8").strip()
    raise ToolError(f"{name} must be a string, {{ env = \"NAME\" }} or {{ secret = \"name\" }}")


def load_settings(config: Path, secrets_dir: Path) -> Settings:
    with config.open("rb") as file:
        table = tomllib.load(file).get("env", {})
    if not isinstance(table, dict):
        raise ToolError(f"{config}: [env] must be a table")
    env = {str(name): resolve(value, str(name), secrets_dir) for name, value in table.items()}
    return Settings.from_env(env)


def live_checks(settings: Settings) -> list[Check]:
    """Connect to the real servers: IMAP login and folders, SMTP login. Sends nothing."""
    checks: list[Check] = []
    try:
        mailbox = ImapMailbox(settings)
    except ToolError as error:
        return [Check(False, f"IMAP login to {settings.imap_host}:{settings.imap_port}", str(error))]
    try:
        checks.append(Check(True, f"IMAP login to {settings.imap_host}:{settings.imap_port} as {settings.username}"))
        folders = mailbox.folder_names()
        checks.append(Check(True, f"{len(folders)} folders: {', '.join(folders)}"))
        wanted = list(settings.folders) + ([settings.sent_folder] if settings.sent_folder else [])
        for folder in wanted:
            if folder in folders:
                _, uids = mailbox.search(folder, ["ALL"])
                checks.append(Check(True, f"folder {folder} ({len(uids)} messages)"))
            else:
                checks.append(Check(False, f"folder {folder}",
                              "not in the mailbox; fix EMAIL_FOLDERS / EMAIL_SENT_FOLDER"))
    except ToolError as error:
        checks.append(Check(False, "IMAP", str(error)))
    finally:
        mailbox.close()
    try:
        SmtpSender(settings).check_login()
        checks.append(Check(True, f"SMTP login to {settings.smtp_host}:{settings.smtp_port}"))
    except ToolError as error:
        checks.append(Check(False, f"SMTP login to {settings.smtp_host}:{settings.smtp_port}", str(error)))
    return checks


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--config", type=Path, default=HERE / "config.toml")
    parser.add_argument("--secrets-dir", type=Path, default=Path("/run/secrets"))
    args = parser.parse_args()
    try:
        settings = load_settings(args.config, args.secrets_dir)
    except (ToolError, OSError, tomllib.TOMLDecodeError) as error:
        print(f"failed: {error}", file=sys.stderr)
        return 1
    checks = live_checks(settings)
    for check in checks:
        print(f"  {'ok  ' if check.ok else 'FAIL'} {check.what}")
        if check.detail:
            print(f"         {check.detail.replace(settings.password, '<password>')}")
    return 0 if all(check.ok for check in checks) else 1


if __name__ == "__main__":
    sys.exit(main())
