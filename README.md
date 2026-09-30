# clankjob

**AI agents that know how to wait.**

Most AI agents live and die inside a single chat. Ask one to get a quote from an
electrician and it will write a lovely email, then stare at you, because the
electrician answers on Thursday and the agent's attention span ends in about four
seconds.

clankjob gives agents patience. A **case** works on its goal, and when it has to wait
(for a reply, a date, or for you) it goes to sleep. It wakes up when something actually
happens, picks up exactly where it left off, and keeps going until the job is done.
Days later, across restarts, without burning a single token while it naps.

```
you      "Get a quote from Bob for an EV charger circuit. Follow up once if he's slow."

case     drafts the email ............................ you approve it
         sleeps, checking the mailbox every hour ..... 0 tokens spent
         Bob replies: "$1,450, but send a panel photo"
         wakes up, asks you for the photo ............ you answer from the web (or Discord)
         sends it, sleeps again
         Bob confirms
         completes: { "price": 1450, "start": "2026-10-12" }
```

Four LLM activations spread over three days. Everything else was the scheduler quietly
checking the mailbox while the agent slept.

The name is loosely based on **cron job**, and so is the design. A cron job does
nothing until its time comes, runs, then goes back to waiting. A case works the same way,
except it wakes on a reply or an answer as well as on a timer, and it decides for itself
when to wait next. Swap the "cron" for a clank of the machine and you get clankjob.

## Why you might love it

- **Sleeping is free.** While a case waits, no model is running. Cheap, deterministic
  checks ("any new email on this thread?") decide when it is worth waking the LLM up.
- **It never forgets.** Every step is appended to a durable event log in SQLite. The
  agent's context is rebuilt from it on each wake-up, so a crash or a `docker restart`
  loses nothing. An activation interrupted mid-tool resumes without asking the model
  twice.
- **You stay in charge.** Cases ask you questions and wait for the answer. Risky actions,
  like sending an email, wait for your approval first, which you can edit or reject;
  emails to your trusted contacts can skip it, per case. The first answer wins, whichever
  channel it came from.
- **Bring any brain.** Any OpenAI-compatible endpoint works: OpenAI, OpenRouter, or a
  model running on your own machine with Ollama, vLLM or LM Studio. Pick the model per
  case, and switch it while the case works: the next step uses the new one.
- **Instructions it always follows, files it reads when needed.** Give a case short
  instructions (tone, limits, contacts) when you create it, and edit them any time. Drop a
  contractor's photo, a PDF quote or an email onto a running case and it wakes up to read
  it, on demand, so big files never bloat every LLM call. Images work with any
  vision-capable model.
- **Tune it without recompiling.** Every word sent to the model is a template. Drop a
  file in `/prompts` to change the rules, or write a profile ("you negotiate quotes with
  tradespeople") and pick it per case. Broken templates are caught at load time, not in
  the middle of a case.
- **Extend it with plugins.** Email, Discord, web search and everything else live
  outside the core as plugins with their own config, tools and wake-up conditions. A
  plugin is a Python process, or just a command-line script declared in a manifest: any
  CLI becomes a tool without writing a line of plugin code.
- **Boring where it counts.** One Rust binary, one SQLite file, synchronous code, no
  message broker. It runs happily in a small container.

## What can a case do?

A case's goal is plain language. Some things clankjob is built for:

- Chase a quote, a refund or an appointment over email, following up politely when
  people go quiet.
- Watch for something ("tell me when the permit office replies") and act on it.
- Run a multi-day errand that needs your input at a few key moments.
- Come back later: "check again on Monday at 9" is just a timer.
- Research: a stock's filings and valuation, what a video says, the weather on the day a
  roof started leaking.

## Status

clankjob is young. Everything below is built and tested:

| Feature                                                                      | State     |
| ---------------------------------------------------------------------------- | --------- |
| Case engine: activations, sleep and wake, crash-safe resume                  | Available |
| Core tools: `sleep`, `ask_human`, `complete`, `fail`, notes, contacts        | Available |
| Timers, timeouts, "ask a human but also wake if X happens"                   | Available |
| Budgets: activations, turns per activation, tokens; cost estimates           | Available |
| OpenAI-compatible LLM adapter, model catalog, model per case (changeable)    | Available |
| REST API with bearer tokens, prompt templates, profiles, hot reload          | Available |
| Web UI: cases, timeline, inbox, contacts, plugins, prompts, light and dark   | Available |
| Instructions and files per case, images for vision models                    | Available |
| Approvals: approve, edit or reject tool calls (web and Discord)              | Available |
| Contacts: trusted recipients skip email approval (per-case setting)          | Available |
| Discord: get asked, answer from chat, get notified; start a case by @mention | Available |
| Plugins: Python processes, and command plugins (any CLI script as a tool)    | Available |
| Published Docker image (amd64, arm64) and `deploy.py` for servers            | Available |

The bundled plugins are listed [below](#plugins). The full design, including everything
planned, is in [docs/design.md](docs/design.md).

## Get started

### On a server, with Docker (recommended)

You need Docker with Compose, Python 3.11+ and an OpenAI-compatible endpoint (OpenRouter,
OpenAI, or your own Ollama). Nothing is built on the server: the image
`ghcr.io/uintptr/clankjob` is published by GitHub Actions for amd64 and arm64.

```sh
curl -fsSLO https://raw.githubusercontent.com/uintptr/clankjob/main/deploy/deploy.py
python3 deploy.py ~/clankjob
```

[`deploy.py`](deploy/deploy.py) downloads the plugins, asks for your public URL, LLM,
model and API key, generates the token you sign in with, and offers to configure each
plugin (email, Discord, web search, …). Secrets are typed hidden and only written to
`.env`. At the end it offers to pull the image and start everything; then open
<http://127.0.0.1:8080> and sign in with `CLANKJOB_TOKEN` from `.env`.

To update later, from inside the setup:

```sh
cd ~/clankjob && python3 deploy.py --yes --start
```

It refreshes the plugins and `compose.yaml`, pulls the latest image and restarts what
changed, and never overwrites a setting or secret you already have. Configure a plugin
you skipped with `python3 deploy.py --configure finance`. Put a reverse proxy with TLS in
front for public access; [deploy/README.md](deploy/README.md) covers that, updates,
backups and the same setup by hand.

### From source

You need a Rust toolchain and an OpenAI-compatible endpoint. A local
[Ollama](https://ollama.com) works fine.

**1. Copy the example config** and pick an LLM:

```sh
cp clankjob.example.toml clankjob.toml
```

[`clankjob.example.toml`](clankjob.example.toml) uses OpenRouter by default and has
commented-out blocks for Ollama and OpenAI. Every option is documented inline. The
essentials look like this:

```toml
listen = "127.0.0.1:8080"
data_dir = "./data"

[api]
tokens = [{ env = "CLANKJOB_TOKEN" }]

[llm.default]
provider = "openai-compatible"
base_url = "https://openrouter.ai/api/v1"
api_key = { env = "OPENROUTER_API_KEY" }
model = "openai/gpt-4.1-mini"
```

Secrets are never written in the file itself. They are references to environment
variables (`{ env = "…" }`) or Docker secrets (`{ secret = "…" }`).

To test without a token, set `require_token = false` under `[api]` (or
`CLANKJOB_REQUIRE_TOKEN=false`): the API and web UI then need no sign-in. Anyone who can
reach the server can use it, so keep it to a trusted machine.

**2. Run the server:**

```sh
export CLANKJOB_TOKEN=change-me
export OPENROUTER_API_KEY=sk-or-...   # or whatever your LLM block needs
cargo run --release -p clankjob-server -- clankjob.toml
```

To run this checkout in Docker instead, with the same `clankjob.toml`, `plugin/` and
`data/`, put those two variables in `.env` and run `docker compose up -d --build`. Never
run both on the same `data/`.

### Give it a job

In the web UI, **New case** needs only a title; the goal, instructions, files, model and
budgets are optional. Or use the API:

```sh
curl -s localhost:8080/api/v1/cases \
  -H "Authorization: Bearer $CLANKJOB_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"title": "Tea reminder", "goal": "Wait two minutes, then ask me whether the tea is ready."}'
```

Watch the case think, sleep and wake up on its page. Questions from your agents land in
the inbox (and on Discord, if you set it up).

Prefer the terminal? Everything the UI does is plain REST:

```sh
curl -s localhost:8080/api/v1/cases/<id>/events -H "Authorization: Bearer $CLANKJOB_TOKEN"
```

When it asks you something, answer it:

```sh
curl -s localhost:8080/api/v1/cases/<id>/messages \
  -H "Authorization: Bearer $CLANKJOB_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"text": "Yes, and it is delicious."}'
```

## How it works

```
             REST API ──▶ create / message / answer / wake / cancel
                 │
                 ▼
  ┌──────── work queue ◀──────── scheduler ◀──── wait conditions
  │         (leased, 1 row          (fires timers,     (timer, deadline,
  │          per case)               deadlines)         human answer, …)
  ▼
worker ──▶ rebuild context from the event log ──▶ LLM ──▶ tools
  ▲                                                        │
  └──────── keep going, or sleep / ask_human / complete ◀──┘
```

- A **case** runs in **activations**. Each activation rebuilds the conversation from the
  event log and loops LLM → tools until the case sleeps, asks you something or finishes.
- `sleep` registers **wait conditions**. The **scheduler** fires them when they are due
  and queues the case again with a wake event explaining why.
- Everything is committed step by step in SQLite, so the next activation, even after a
  crash, continues from the last completed step.

## Tuning behaviour with prompts

Built-in prompt templates cover the platform rules, the case header, wake-up messages and
the nudge sent when the model forgets to call a tool. Override any of them, or add
profiles:

```
prompts/
  system.md              replaces the built-in platform rules
  profiles/
    quotes.md            "You negotiate quotes with tradespeople for {{ case.owner }}."
```

Create a case with `"profile": "quotes"` to use it. Send `SIGHUP` or
`POST /api/v1/admin/reload` to pick up changes; invalid templates are rejected with a
precise error and the previous version keeps working.

## Plugins

Every plugin lives in its own directory under `plugin/`, with a README, a
`config.example.toml` when it has settings, and a `check_config.py` that checks it
against the real services (the web UI's **Plugins** page shows the same status). Every
case can use every loaded plugin: the system prompt lists each one in a line, and a case
loads the ones it needs, so an unused plugin costs a line instead of its tool schemas.

| Plugin               | What cases get                                                                    | Needs                                |
| -------------------- | --------------------------------------------------------------------------------- | ------------------------------------ |
| `email`              | Send, reply, read, wait for replies; approval unless every recipient is trusted   | an IMAP/SMTP mailbox                 |
| `discord`            | Questions, approvals and notifications in a Discord channel, answered in chat     | a bot token                          |
| `web`                | Google search, pages as text, waits for a page or feed to change                  | a Google Programmable Search key     |
| `documents`          | Metadata, OCR and text of the case's files (PDF, Office, images, media)           | nothing (in the image)               |
| `shell`              | A bash shell in a separate sandbox container, with network tools                  | the sandbox container (compose.yaml) |
| `ntfy`               | Push notifications to your phone or desktop through ntfy                          | an ntfy server and topic             |
| `weather`            | Forecasts and past weather for a place (Open-Meteo)                               | nothing                              |
| `home_assistant`     | Sensors, history and services of your home; waits for a state; approval to act    | Home Assistant and an access token   |
| `youtube_transcribe` | Video transcripts, and guides for summaries and earnings calls                    | nothing (a proxy if YouTube blocks)  |
| `finance`            | Market data, screens, SEC filings, 13F holdings, FRED macro, DCF; analysis guides | a free FRED key for the macro tools  |

The Discord **intake** ([intake/discord](intake/discord/README.md)) is separate: a small
service that starts a case when you @mention its bot.

## Project layout

```
crates/
  core/         domain types and the LlmProvider trait
  storage/      SQLite schema, migrations and repositories
  engine/       activation loop, scheduler, worker pool, prompt templates
  llm-openai/   OpenAI-compatible adapter
  plugin-host/  loads plugins and runs Python ones as child processes
  server/       REST API, configuration, process lifecycle (the `clankjob` binary)
plugin/
  discord/      Discord human channel (Python, standard library only)
  youtube_transcribe/  YouTube transcripts as tools, plus analysis guides
  finance/      market data, SEC filings, 13F, FRED, DCF, plus analysis guides
  email/        send and read email, wait for replies (IMAP/SMTP, approvals)
  documents/    metadata, OCR and text of the case's files
  ntfy/         push notifications through ntfy (any server, optional login)
  weather/      forecasts and past weather for a place (Open-Meteo, no key)
  home_assistant/  states, history and services of Home Assistant, waits for a state (hacli)
  shell/        a bash shell for the agent, run in the sandbox container
  web/          web search (Google), pages as text, waits for page and feed changes
sandbox/        the sandbox's exec service (sandboxd.py), for plugin/shell
intake/
  discord/      optional service: @mention the bot on Discord to start a case (REST API only)
deploy/         deploy.py and the compose.yaml for servers running the published image
web/            the web UI (vanilla JavaScript and CSS, no CDN, compiled into the binary)
docs/
  design.md     the full design
```

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Python (plugins, sandbox, intake, deploy), as CI runs it in each of those directories:

```sh
python -m unittest -v
uvx ruff check .
uvx basedpyright .
```

Coding guidelines for contributors, human or AI, live in [AGENT.md](AGENT.md) and
[agent/](agent/).
