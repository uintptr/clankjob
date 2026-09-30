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

> Questions, answers and notifications work today. Approvals (✅/❌) are ready here
> but wait on approvals in the server, and attachments in replies are not imported
> yet (the agent is told a file was sent).

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

**5. Point the server at it.** In `clankjob.toml`:

```toml
plugins_dir = "./plugin"          # "/plugins" in a container
public_url = "https://clank.acme.com"   # optional: adds a link to the case
```

Start the server with the token in its environment (`DISCORD_BOT_TOKEN`, or whatever
`bot_token = { env = … }` names). At startup the server runs the same checks as
`check_config.py` below and logs `channel ready`, or an error per problem (with the fix)
and `channel loaded with problems`. The web UI's **Plugins** page shows the same, with
a **Test now** button, and the server reloads the plugin by itself when you edit
`config.toml`. A new token in the environment still needs a server restart. New cases ask only in the web UI unless the New case form or
`default_human_channels` in `clankjob.toml` turns the channel on; a running case's
Settings (Notifications) turns it on or off too.

**Check it on its own** at any time, without the server:

```sh
./check_config.py            # token, intent, permissions, responders
./check_config.py --notify   # also post a test message
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

## Design

A **human channel** plugin: it has no LLM tools or wait conditions, only `HumanChannel`.
It lives in `plugin/discord/` as an external Python plugin (standard library only,
Python 3.11+) that implements the protocol of [design §9.8](../../docs/design.md), with tests against a fake Discord.
The server loads it from `plugins_dir` and drives it through [design §10.3](../../docs/design.md). Its `validate_config`
checks the whole setup with read-only calls: the token, that the channel is a text
channel, the Message Content intent, the bot's effective permissions in the channel
(roles and channel overwrites), and that each allowed responder is in the server and can
reply in threads. The server runs it for every instance when plugins load and logs each
problem with its fix; the Plugins page shows the result and can run it again ("Test
now"); `check_config.py` runs it without the server and prints a checklist. Editing
`config.toml` reloads the plugin by itself ([design §9.4](../../docs/design.md)); a new token in the environment needs a
server restart.

### Configuration

Instances go in `plugin/discord/config.toml` (git-ignored; `config.example.toml` is the
template) and are validated against `schema.json`:

```toml
[instances.discord_joe]
bot_token = { secret = "discord_bot_token" }
channel_id = "123456789012345678"
allowed_responders = ["234567890123456789"]   # only these users can answer
mention = true                                # ping them on questions and approvals
# poll_interval = "20s"
# notify_on = ["completed", "failed", "budget_exceeded"]
```

Links back to a case come from the server's `public_url`, passed as `case_url` in each
request, so the plugin needs no URL of its own. `validate_config` and `healthcheck`
check the token (`GET /users/@me`) and that the channel is visible.

### Messages

A question opens a **thread** on itself; a reply in the thread is the answer. Scoping
replies to a thread is what keeps concurrent questions apart, with nothing to match up.

```
@joe
**Electrician quote** needs your input
Bob needs a photo of the electrical panel before confirming the price. Can you send one?
-# Reply in the thread to answer
<https://clank.acme.com/#/cases/01J9…>
```

An approval is answered with a **reaction**: the bot seeds ✅ and ❌, and a tap by an
allowed responder decides. Editing the proposed action stays a web-only feature.

````
@joe
**Electrician quote** wants your approval
Send an email to bob@sparkyelectric.ca
```
{ "to": ["bob@sparkyelectric.ca"], "subject": "Quote request: 50A EV charger circuit" }
```
-# React ✅ to approve or ❌ to reject. To edit it first, use the web UI.
````

After resolution the bot edits the message with a status line ("Answered via web by
joe", "No longer needed", "Case cancelled") and closes the question's thread.

Every message sets `allowed_mentions` to the allowed responders only: questions are
written by the LLM, and an `@everyone` in one must stay plain text.

### Talking to Discord

- **REST only**, polled: no gateway WebSocket and no public endpoint (`urllib`).
- The poll cursor maps each open thread to the last message seen in it; approvals are
  checked by reading the message's reactions.
- Attachments are returned as links, and the host downloads them at once because
  Discord CDN links expire.
- Rate limits: short `429` waits are slept through; longer ones come back as retryable
  errors so the host backs off. One or two API calls per open request per poll.
- The bot needs the **Message Content** intent to read thread replies; an empty reply
  from a responder is reported as a warning pointing at it.

### Later: buttons

Approve/Reject buttons need Discord **Interactions**, which call a public HTTPS endpoint.
They would come in as a push source, written to a durable inbox table and resolved through
the same answer path ([design §19](../../docs/design.md)). Reactions remain the fallback.

## Development

Standard library only, Python 3.11+.

```sh
python3 -m unittest -v test_discord_plugin.py
autopep8 --in-place discord_plugin.py && uvx ruff check . && uvx basedpyright discord_plugin.py test_discord_plugin.py
```
