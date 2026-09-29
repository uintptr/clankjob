# Plugin guidelines

Plugins live in `plugin/<id>/`, one self-contained directory each, mounted as
`/plugins` in a container. The design is in `docs/design.md` §9 (plugin system), §9.9
(command plugins) and §10.3 (human channels). Python code follows
`agent/AGENT_python.md`; markdown follows `agent/AGENT_md.md`.

## Every plugin has a `check_config.py`

**Every plugin ships an executable `check_config.py` at the root of its directory that
checks, against the real services, that the plugin will work on this machine with this
configuration.** It is the first thing a user runs after filling in `config.toml`, and
the first thing to ask for when a plugin misbehaves.

Why: a plugin fails in ways its unit tests cannot see: a wrong token, a missing
permission, an intent left off in a developer portal, a spam folder named differently, a
cloud IP blocked by YouTube. Each of these cost a debugging session before the check
existed. One command with a clear checklist turns them into a one-line fix, and the same
command everywhere means nobody has to remember how each plugin is checked.

Current checks:

| Plugin               | Checks                                                                               |
| -------------------- | ------------------------------------------------------------------------------------ |
| `discord`            | token, channel type, Message Content intent, bot permissions, responders; `--notify` |
| `email`              | IMAP login, folders listed and configured ones present, SMTP login                   |
| `documents`          | the programs its tools drive, Tesseract languages, an OCR round trip                 |
| `youtube_transcribe` | `uv` on PATH, the script runs, YouTube serves video details and caption tracks       |

### The contract

- **Location and shebang.** `plugin/<id>/check_config.py`, executable, run from anywhere:
  paths are relative to the script (`Path(__file__).resolve().parent`), not to the
  current directory.
- **Standard library only**, with the plain `#!/usr/bin/env python3` shebang, even when
  the plugin itself needs dependencies: the check is what tells the user those are
  missing. Call the plugin's own script to exercise them (as `youtube_transcribe` runs
  `scripts/yt.py`).
- **Configuration exactly as the server reads it.** Read `config.toml` next to the script
  and resolve values the same way: a literal string, `{ env = "NAME" }` from the
  environment, or `{ secret = "name" }` from the secrets directory. Pass the plugin's
  commands the same environment the server does (`PATH`, `HOME`, `TZ`, `LANG`, `LC_ALL`
  plus the plugin's `[env]`), so a check that passes here passes in the server.
- **Flags.** `--config PATH` (default: `config.toml` next to the script) and
  `--secrets-dir DIR` (default `/run/secrets`); `--instance NAME` for plugins with
  instances. Anything else is specific to the plugin (e.g. `--video` for the video used).
- **Read-only by default.** Log in, list, look up, never change anything: no message
  sent, nothing marked read, no file written. An action with an outside effect (posting a
  test message, sending a test email) is opt-in behind an explicit flag such as
  `--notify` or `--send-test`, and only runs after every read-only check passed.
- **Reuse the plugin's code.** Import the plugin's module and call the same functions the
  plugin uses (Discord's `diagnose`, email's `ImapMailbox`), so the check tests the real
  code path. When the plugin reports problems to the server (e.g. through
  `validate_config`), the check and the server's startup check are the same code.
- **Never print a secret.** Errors name the reference (`env "EMAIL_PASSWORD"`), never the
  value; replace resolved secrets in any error text before printing it. A
  `{ secret = … }` whose name looks like a token is refused without echoing it.

### Output

One line per check, then an exit status:

```
bot 'clankbot' in #general                        optional summary line
  ok   Message Content intent
  FAIL bot can open a thread per question (Create Public Threads)
         fix: give the bot's role Create Public Threads in #general
  warn bot can lock a question's thread once it is answered (Manage Threads)
         fix: optional: give the bot's role Manage Threads
```

- Two spaces, a four-character mark (`ok  `, `FAIL` for what breaks the plugin, `warn`
  for what degrades it), a space, then what was checked, phrased so `ok` reads as a fact
  ("folder Bulk (56 messages)").
- A failed or warned check may be followed by a line indented by 9 spaces saying why
  and, when known, how to fix it (`fix: …`).
- Exit `0` when no check failed (warnings allowed), `1` otherwise. When the check cannot
  even start (unreadable `config.toml`, missing environment variable), print
  `failed: <reason>` to stderr and exit `1`.

### Tests

- Unit tests cover the check's offline logic (config resolution, parsing server replies)
  like any other plugin code.
- A test that talks to the real service is skipped unless `<PLUGIN>_LIVE_TEST=1` is set
  (e.g. `EMAIL_LIVE_TEST=1`), so the default test run stays offline and needs no
  credentials. It calls the same functions as `check_config.py`.

### Documentation

The plugin's `README.md` shows how to run the check in its setup steps, with the
environment variable to export first when the config uses `{ env = … }`, and what a
passing run looks like.

## A plugin documents itself

**Everything specific to a plugin is documented in its own directory, not in
`docs/design.md`.** Its `README.md` says what it does, how to set it up and check it
(`check_config.py`), and has a **Design** section for how it works inside: its tools,
wait conditions, configuration, protocol details and what is planned for it. Instructions
for the agent go in the plugin's `guides/` (read with `read_guide`) or in its tool and
condition descriptions in `plugin.toml`.

`docs/design.md` describes the **plugin system**: manifests, command and process plugins,
human channels, approvals, wait conditions. It names plugins only as examples and links
to their README (§11 and §12 are such pointers).

Why: a plugin is copied, mounted and removed as a directory. Its documentation has to
travel with it, stay true when only the plugin changes, and not force a design-doc edit
for every plugin change. When a change is about the host (e.g. how every command tool
treats blank arguments), it belongs in `docs/design.md`; when it is about one plugin, it
belongs in that plugin's README.

## Plugin directory layout

```
plugin/<id>/
  plugin.toml            manifest (id = directory name, runtime, tools, conditions, guides)
  config.example.toml    template, committed; documents every setting
  config.toml            the user's settings, git-ignored (plugin/*/config.toml)
  check_config.py        see above
  README.md              what it does, setup, check_config.py, tests
  <code>, test_<code>.py
  guides/*.md            optional instructions the agent reads with `read_guide`
```

- **A plugin that cannot work unconfigured** (email needs a mailbox) sets
  `requires_config = true` in `plugin.toml`: it stays unloaded, with a note on the Plugins
  page, until its `config.toml` exists, so the agent is never offered tools that can only
  fail. The Docker image bundles every plugin without its `config.toml`.
- **Python 3.11.** The Docker image runs plugins with Debian bookworm's Python 3.11, so
  no 3.12+ syntax or library (e.g. the same quotes nested inside an f-string). CI runs
  every plugin's tests on 3.11.
- **Secrets never go in `config.toml`.** Use `{ env = "NAME" }` or `{ secret = "name" }`;
  the example file shows the reference, never a placeholder that looks like a value.
- **Self-contained.** A plugin directory must work when copied or mounted alone, so it
  imports nothing from other plugins; small helpers such as config resolution are
  copied rather than shared.
