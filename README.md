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

## Why you might love it

- **Sleeping is free.** While a case waits, no model is running. Cheap, deterministic
  checks ("any new email on this thread?") decide when it is worth waking the LLM up.
- **It never forgets.** Every step is appended to a durable event log in SQLite. The
  agent's context is rebuilt from it on each wake-up, so a crash or a `docker restart`
  loses nothing. An activation interrupted mid-tool resumes without asking the model
  twice.
- **You stay in charge.** Cases ask you questions and wait for the answer. Risky actions,
  like sending an email, can require your approval first. The first answer wins,
  whichever channel it came from.
- **Bring any brain.** Any OpenAI-compatible endpoint works: OpenAI, OpenRouter, or a
  model running on your own machine with Ollama, vLLM or LM Studio.
- **Instructions it always follows, files it reads when needed.** Give a case short
  instructions (tone, limits, contacts) when you create it, and edit them any time. Drop a
  contractor's photo, a PDF quote or an email onto a running case and it wakes up to read
  it, on demand, so big files never bloat every LLM call. Images work with any
  vision-capable model.
- **Tune it without recompiling.** Every word sent to the model is a template. Drop a
  file in `/prompts` to change the rules, or write a profile ("you negotiate quotes with
  tradespeople") and pick it per case. Broken templates are caught at load time, not in
  the middle of a case.
- **Extend it with plugins.** Email, Discord and anything else live outside the core as
  plugins with their own config, tools and wake-up conditions. Plugins will soon be
  writable in Python, too.
- **Boring where it counts.** One Rust binary, one SQLite file, synchronous code, no
  message broker. It runs happily in a small container.

## What can a case do?

A case's goal is plain language. Some things clankjob is built for:

- Chase a quote, a refund or an appointment over email, following up politely when
  people go quiet.
- Watch for something ("tell me when the permit office replies") and act on it.
- Run a multi-day errand that needs your input at a few key moments.
- Come back later: "check again on Monday at 9" is just a timer.

## Status

clankjob is young. Milestone 1, the engine every case depends on, is done and tested.

| Feature                                                             | State     |
| ------------------------------------------------------------------- | --------- |
| Case engine: activations, sleep and wake, crash-safe resume         | Available |
| Core tools: `sleep`, `ask_human`, `complete`, `fail`, notes         | Available |
| Timers, timeouts, "ask a human but also wake if X happens"          | Available |
| Budgets: activations, turns per activation, tokens                  | Available |
| OpenAI-compatible LLM adapter                                       | Available |
| REST API with bearer tokens, prompt templates, profiles, hot reload | Available |
| Web UI: cases, timeline, inbox, prompts, light and dark themes      | Available |
| Discord: get asked, answer from chat, get notified when done        | Available |
| Python plugins (human channels so far)                              | Available |
| Command plugins: any CLI script as a tool, e.g. YouTube transcripts | Available |
| Email: send, reply, read, wait for replies (you approve each email) | Available |
| Approvals: approve, edit or reject tool calls (web and Discord)     | Available |
| Docker image with document tools (metadata, OCR, text extraction)   | Available |

The full design, including everything planned, is in [docs/design.md](docs/design.md).

## Quick start

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

**3. Give it a job:**

```sh
curl -s localhost:8080/api/v1/cases \
  -H "Authorization: Bearer $CLANKJOB_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"title": "Tea reminder", "goal": "Wait two minutes, then ask me whether the tea is ready."}'
```

**4. Open the web UI** at <http://127.0.0.1:8080>, sign in with your token, and watch
the case think, sleep and wake up. Questions from your agents land in the inbox.

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
  email/        send and read email, wait for replies (IMAP/SMTP, approvals)
  documents/    metadata, OCR and text of the case's files
  weather/      forecasts and past weather for a place (Open-Meteo, no key)
  shell/        a bash shell for the agent, run in the sandbox container
  web/          web search (Google), pages as text, waits for page and feed changes
sandbox/        the sandbox's exec service (sandboxd.py), for plugin/shell
intake/
  discord/      optional service: @mention the bot on Discord to start a case (REST API only)
web/            the web UI (vanilla JavaScript and CSS, no CDN, compiled into the binary)
docs/
  design.md     the full design
```

## Run it with Docker

The image has the server, Python for plugins, and the programs behind the document tools
(ExifTool, Poppler, Tesseract OCR in English and French, pandoc, FFmpeg).
`compose.yaml` runs it from this checkout with the same `clankjob.toml`, `plugin/` and
`data/` as `cargo run`; the image overrides the listen address and paths itself.

```sh
# secrets for the { env = ... } references, next to compose.yaml (git-ignored)
cat > .env <<'EOF'
CLANKJOB_TOKEN=change-me
OPENROUTER_API_KEY=sk-or-...
EOF

docker compose up -d --build
docker compose logs -f
```

The web UI is on <http://127.0.0.1:8080>; put a reverse proxy with TLS in front for
`public_url`. Stop any `cargo run` first: both would use `data/`. Details in
[docs/design.md §18](docs/design.md).

**On a server, without the source:** the image is published to
`ghcr.io/uintptr/clankjob` (amd64 and arm64) by GitHub Actions. Download
[`deploy/compose.yaml`](deploy/compose.yaml), write `clankjob.toml` and `.env`, and run
`docker compose up -d`; [deploy/README.md](deploy/README.md) walks through it. Or let
[`deploy/deploy.py`](deploy/deploy.py) do it: it downloads the plugins, asks for your
settings and secrets, and writes everything.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Coding guidelines for contributors, human or AI, live in [AGENT.md](AGENT.md) and
[agent/](agent/).
