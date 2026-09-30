# Discord intake

Starts a case from Discord: @mention the bot with what you want done, and it creates the
case and replies with a link.

```
you        @clankbot Get 3 quotes for a 200A panel upgrade
           In Laval, before November.
clankbot   ↳ Started **Get 3 quotes for a 200A panel upgrade**: <https://cj.example/#/cases/01M3…>
```

The first line is the case's title and the whole message its goal, so the agent starts
right away. Attachments are passed as links in the goal (Discord's links expire within a
day, so ask the agent to download them early, or upload them on the case page).

It is a **standalone service**, not part of the server or of the Discord plugin:

- It talks to the server only through its public REST API (`POST /api/v1/cases`), like
  the web UI. The server knows nothing about it.
- It shares no code or settings with the Discord plugin, which keeps posting the cases'
  questions. It can use its own bot, or the plugin's.
- Where its cases ask questions and send notifications is `DISCORD_INTAKE_HUMAN_CHANNELS`,
  the Discord plugin's channel instance (e.g. `discord_joe`), so cases started from
  Discord come back to Discord. Unset, they follow the server's `default_human_channels`,
  which is none (the web client only) unless you set it. A case's channels can be changed
  later under Settings on its page.

## Setup

1. **A bot.** Either reuse the Discord plugin's bot, or create one in the
   [Developer Portal](https://discord.com/developers/applications) (Bot → Reset Token)
   and invite it to your server. In the channel it needs **View Channel**, **Read Message
   History** and **Send Messages**. No privileged intent is needed: Discord always shows
   a bot the text of messages that mention it.

2. **Settings** in `.env` next to `compose.yaml` (Developer Mode on, right-click → Copy ID):

   ```sh
   COMPOSE_PROFILES=discord-intake            # turns the service on
   DISCORD_INTAKE_TOKEN=...                   # the bot's token
   DISCORD_INTAKE_CHANNEL_ID=123456789012345678
   DISCORD_INTAKE_ALLOWED_USERS=234567890123456789    # comma-separated user ids
   CLANKJOB_PUBLIC_URL=https://cj.example     # optional: links in the replies
   DISCORD_INTAKE_HUMAN_CHANNELS=discord_joe  # optional: its cases ask on Discord
   ```

   It reuses `CLANKJOB_TOKEN` to call the server (not needed when the server runs with
   `CLANKJOB_REQUIRE_TOKEN=false`). The compose service passes it only these variables,
   never the rest of `.env`.

3. **Start and check:**

   ```sh
   docker compose up -d
   docker compose run --rm discord-intake --check
   ```

   ```
     ok   Discord bot clankbot (900000000000000000)
     ok   reads channel #clankjob (123456789012345678)
     ok   mentions of the bot's role (345678901234567890) count too
     ok   1 allowed user(s): 234567890123456789
     ok   clankjob server at http://clankjob:8080 accepts the token
     ok   links back: https://cj.example
   ```

   `docker compose logs -f discord-intake` shows each case it starts.

Tests use a fake Discord and a fake server:

```sh
python3 -m unittest -v test_discord_intake.py
```

## Design

`discord_intake.py` (standard library only) polls the channel every 20 s
(`DISCORD_INTAKE_POLL`) over Discord's REST API, with no gateway connection and no
public endpoint.

- **Which messages.** Only top-level messages in the channel (replies in a question's
  thread are the plugin's), from `DISCORD_INTAKE_ALLOWED_USERS`, that @mention the bot,
  and not from bots. A mention with no text gets a reply asking what to do.
- **The bot's role counts.** Discord gives a bot a role with its own name, and the
  mention list offers both; picking the role is an easy mistake, so a mention of that role
  (found at startup: the server's role tagged with the bot's id) counts as a mention of
  the bot. Without the Message Content intent Discord may hide the text of such a message;
  the bot then replies asking to mention it directly.
- **Skipped messages are logged** when they look meant for the bot: a mention from
  someone not allowed, or an allowed person's message that mentions a role but not the
  bot.
- **State.** The id of the last message handled is kept in
  `/state/discord_intake.json` (the `intake-state` volume). On its very first run it
  starts from the newest message, so the channel's history never starts cases.
- **At most once per message.** The cursor moves past a message as soon as its case is
  created, before the reply is posted, so a failed reply never creates a second case. If
  the server is down or restarting (a network error or HTTP 5xx), the cursor stays and
  the message is tried again at the next poll; a refusal (HTTP 4xx) is explained in a
  reply and not retried.
- **Replies** quote the message and never mention anyone (`allowed_mentions` is empty),
  since titles come from users' text. Short Discord rate limits are slept through; long
  ones wait for the next poll.
- **Owner.** The case's owner is the author's Discord display name.

Planned: a reaction (👀) while the case is being created, and replies in a thread per case.
