# Discord plugin

A clankjob **human channel**: questions and approvals from your cases show up in a
Discord channel, and you answer them from there, from your phone if you like. The
web UI keeps working alongside it. Whichever answer arrives first wins, and the
other copy is marked as answered.

| The case needs… | Discord shows…                            | You answer by…               |
| --------------- | ----------------------------------------- | ---------------------------- |
| an answer       | the question, with a thread opened on it  | replying in the thread       |
| an approval     | what the case wants to do, with ✅ and ❌ | tapping one of the reactions |
| nothing (FYI)   | "case completed", "case failed", …        | nothing                      |

Everything is plain REST, polled by the server: no gateway connection, no public
endpoint, nothing to expose to the internet.

> The server side of plugins (the plugin host, human requests delivered to
> channels) is planned for milestones 3 and 4 of [the design](../../docs/design.md).
> Until then this plugin is complete and tested on its own, but not yet called.

## Setup

**1. Create the bot.** In the
[Developer Portal](https://discord.com/developers/applications): **New Application**
→ **Bot** → **Reset Token**, and keep the token. Under **Bot**, turn on the
**Message Content Intent**, or thread replies arrive empty. Set **Installation →
Install Link → None** so the bot stays private.

**2. Invite it** to a server with just you in it, with *View Channels*, *Send
Messages*, *Read Message History*, *Add Reactions*, *Create Public Threads* and
*Send Messages in Threads*:

```
https://discord.com/oauth2/authorize?client_id=<APP_ID>&scope=bot&permissions=309237713984
```

**3. Copy the ids.** Settings → Advanced → **Developer Mode** on, then right-click
the channel → **Copy Channel ID**, and right-click yourself → **Copy User ID**.

**4. Configure an instance** (`config.toml` here is git-ignored):

```sh
cp config.example.toml config.toml
```

```toml
[instances.discord_joe]
bot_token = { secret = "discord_bot_token" }   # or { env = "DISCORD_BOT_TOKEN" }
channel_id = "123456789012345678"
allowed_responders = ["234567890123456789"]
```

## Safety

- **Only `allowed_responders` can answer.** Replies and reactions from anyone else
  are ignored, so an answer from Discord carries your authority.
- **Messages can only mention `allowed_responders`.** Questions are written by the
  LLM, and an `@everyone` in one stays plain text.
- **The token is the whole bot.** Keep it in a secret or an environment variable,
  never in the file itself.
- Discord stores messages in plaintext. Don't route anything you wouldn't post
  there.

## Protocol

The server runs `discord_plugin.py` as a child process and talks JSON-RPC 2.0 over
stdin/stdout, one message per line (design §9.8). The process keeps no state: each
call carries the instance config, and the poll cursor travels with each poll.

| Method                           | What it does                                                       |
| -------------------------------- | ------------------------------------------------------------------ |
| `initialize`                     | protocol handshake (`{"protocol": 1}`)                             |
| `validate_config`, `healthcheck` | checks the token and that the channel is visible                   |
| `deliver`                        | posts a `question` (opens a thread) or an `approval` (seeds ✅ ❌) |
| `poll`                           | looks for answers to the open requests the server lists            |
| `on_resolved`                    | marks a message "Answered via web by joe", "No longer needed", …   |
| `notify`                         | posts an informational message, without pinging                    |
| `shutdown`                       | exits                                                              |

Errors come back as JSON-RPC errors with `data.retryable`, so the server knows
whether to try again (rate limits, 5xx, network) or give up (bad token, missing
permission).

## Development

Standard library only, Python 3.11+.

```sh
python3 -m unittest -v test_discord_plugin.py
autopep8 --in-place discord_plugin.py && uvx ruff check . && uvx basedpyright discord_plugin.py test_discord_plugin.py
```
