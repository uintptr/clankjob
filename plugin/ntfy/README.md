# ntfy plugin

Lets a case push a notification to your phone or desktop through [ntfy](https://ntfy.sh):
"tell me when the filing is out", "notify me if the backup fails". Works with the public
ntfy.sh or a self-hosted server, with or without login.

| Tool        | What it does                                                                   |
| ----------- | ------------------------------------------------------------------------------ |
| `ntfy_send` | Publishes a message (title, priority, tags, click URL, Markdown) to your topic |

The server, the topic and the credentials come from `config.toml`: the agent chooses what
the notification says, never where it goes. It only informs. To get an answer, a case uses
`ask_human`, which the web inbox and the human channels (Discord) handle.

## Setup

Copy `config.example.toml` to `config.toml`, set `NTFY_URL` and `NTFY_TOPIC`, and
subscribe to that topic in the ntfy app. On a server without login, anyone who knows a
topic's name can read and write it, so pick a name that is hard to guess, or use a server
with access control and set `NTFY_TOKEN` (or `NTFY_USERNAME` and `NTFY_PASSWORD`) as a
secret reference. Then check it:

```sh
export NTFY_TOKEN=...          # only if config.toml uses { env = "NTFY_TOKEN" }
./check_config.py              # read-only
./check_config.py --notify     # also sends a test notification
```

```
  ok   ntfy server at https://ntfy.sh is healthy
  ok   topic clankjob-change-me accepts no login
  ok   test notification sent (id JTCkqsx1XztX)
```

Tests use a fake ntfy. `NTFY_LIVE_TEST=1` also runs the read-only checks against
`NTFY_URL` (default ntfy.sh):

```sh
python3 -m unittest -v test_ntfy_tool.py
```

## Design

`ntfy_tool.py` is a command plugin ([design §9.9](../../docs/design.md)), standard
library only, printing JSON.

- **Publishing.** One `POST` of ntfy's JSON form to the server root (`topic`, `message`,
  `title`, `priority` 1 to 5, `tags`, `click`, `markdown`). The reply's message `id` and
  `time` are returned to the agent.
- **Limits.** The message is at most 4096 bytes (ntfy turns anything longer into an
  attachment, which not every server allows), the title 250 characters, 10 tags.
  `click` must be an http(s) URL. Everything is checked before a request is made.
- **Authentication.** `NTFY_TOKEN` is sent as `Bearer`, a user name and password as
  `Basic`; setting both kinds is refused. The token and password are replaced by `***` in
  any error text.
- **Check.** `check_config.py` calls `GET /v1/health` and `GET /<topic>/auth`, which
  checks the credentials and the right to read the topic. Only `--notify` can prove the
  right to publish, since ntfy has no dry run.
- **Repeats.** A call sends a notification each time it runs. If the server stops
  mid-call, the call runs again when the case resumes (§9.9), so the owner may get the
  same notification twice.
- `requires_config = true`: without a server and a topic the plugin stays unloaded.

Planned: an optional list of topics the agent may choose from (e.g. `alerts` and
`digest`), and sending a case file as an attachment.
