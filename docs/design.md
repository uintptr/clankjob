# Clankjob — Design Document

**Status:** Draft, partly built (see §0) · **Updated:** 2026-09-29

Clankjob is a platform for running **long-lived, LLM-driven tasks** ("cases") that
can work for a while, go to sleep, wake themselves up when something happens (or
when a timer fires), and continue until they are done.

The platform is split into:

- **Server**: a Rust REST API built on [rouille](https://github.com/tomaka/rouille),
  plus the case engine, scheduler and plugin host.
- **Web client**: a single-page app, compiled into the server binary, that creates,
  monitors and steers cases.

All capabilities that reach outside the core, such as email or Discord, are **plugins**.
The core never needs to be rewritten to add one. When a case needs a person, it can be
answered from the web client **or** from any chat plugin (Discord first); whichever
answer arrives first wins.

______________________________________________________________________

## 0. Implementation status

This document describes both what exists and what is planned. Sections and features are
marked **(built)**, **(partly built)** or **(planned)**.

| Area                                                                     | Status                | Where     |
| ------------------------------------------------------------------------ | --------------------- | --------- |
| Case engine: activations, sleep and wake, crash-safe resume, budgets     | Built                 | §4–§6     |
| Core tools, `core.timer`, `core.human_input`                             | Built                 | §5.1      |
| Context rebuilding, notes, prompt templates, profiles, hot reload        | Built                 | §7.1–§7.4 |
| Compaction of long transcripts                                           | Planned               | §7.3      |
| Instructions (always in context) and files (read on demand)              | Built                 | §7.5      |
| OpenAI-compatible LLM adapter, model discovery, images for vision models | Built                 | §8        |
| Questions to the owner, answered from the web client                     | Built                 | §10.1     |
| Questions and notifications on chat channels, first answer wins          | Built                 | §10       |
| Plugin host: plugin directory, instances, protocol, reload, Plugins page | Built for channels    | §9, §15   |
| Command plugins: CLI scripts as LLM tools, plus guides                   | Built                 | §9.9      |
| Approvals of tool calls (web, with edits, and Discord reactions)         | Built                 | §9.7, §10 |
| Plugin wait conditions (command plugins), checked on their own thread    | Built                 | §6, §9.9  |
| Email plugin (IMAP/SMTP)                                                 | Built                 | §11       |
| Protocol plugin tools and conditions (JSON-RPC `call_tool`, `check`)     | Planned (milestone 4) | §9        |
| Discord plugin                                                           | Built                 | §12       |
| REST API                                                                 | Partly built          | §14       |
| Web client                                                               | Built                 | §15       |
| SQLite storage                                                           | Built                 | §16       |
| `public_url`, CORS, CSP, body limits                                     | Built                 | §17.3     |
| Docker image (with document tools) and compose file                      | Built                 | §18       |

______________________________________________________________________

## 1. Overview

### 1.1 Problem

Many useful tasks can't be finished in a single LLM session because they depend on
the outside world. You might send an email and wait for a reply, wait for a document,
or check back tomorrow. A chat session cannot wait for days. A cron job cannot reason
about what it found. Clankjob combines the two: a case is an LLM agent whose state is
persisted, and it can **suspend itself on a wait condition** and be resumed later.

### 1.2 Goals

- Cases that run for hours, days or weeks, surviving server restarts.
- **Sleeping is free.** A sleeping case uses no LLM tokens. Checking whether it should
  wake up is done by cheap, deterministic code, not by the model.
- Extensibility through plugins that each provide their own configuration, tools and
  wait conditions.
- Provider-agnostic LLM access.
- Full observability: every LLM turn, tool call and wake-up is recorded and visible
  in the web client.
- Human-in-the-loop: cases can ask a human for input, and sensitive actions can
  require approval.

### 1.3 Non-goals (for v1)

- Multi-node / horizontally scaled deployment (single server process to start).
- Multi-tenant billing, organizations, fine-grained RBAC.
- Sandboxing plugins. Plugins, including Python ones, are trusted code (see §19).
- Real-time push wake-ups such as IMAP IDLE or webhooks (polling first, see §19).

______________________________________________________________________

## 2. Concepts

| Term                | Meaning                                                                                                                                                                 |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Case**            | A long-running task with a goal, e.g. _"Get a quote from ACME for 200 widgets and summarize it."_ Has a state, a transcript, instructions and files.                    |
| **Activation**      | One awake period of a case: the case is loaded, the LLM ↔ tool loop runs, and the activation ends when the case sleeps, asks a human, completes or fails.               |
| **Event**           | An immutable, append-only record of something that happened to a case: a wake-up, an LLM turn, a tool result, a state change.                                           |
| **Transcript**      | The ordered list of events that gets turned into LLM context.                                                                                                           |
| **Wait condition**  | What a sleeping case is waiting for: a condition kind (`core.timer`, or a plugin's, e.g. `reply_received`), its parameters, and an optional timeout.                    |
| **Instruction**     | Short owner-written text that steers a case, always in its system prompt (§7.5).                                                                                        |
| **Case file**       | A PDF, image or text file added to a running case, read by the agent on demand (§7.5).                                                                                  |
| **Profile**         | A reusable prompt snippet, chosen per case, appended to the system prompt (§7.4).                                                                                       |
| **Plugin**          | Code that extends the core with configuration, tools, wait-condition kinds, and optionally a human channel: compiled in, or an external process (§9).                   |
| **Plugin instance** | A plugin plus a concrete configuration, e.g. the `email` plugin configured for `support@example.com`. A plugin can have many instances.                                 |
| **Tool**            | A function the LLM can call. It comes either from the core (`sleep`, `complete`, …) or from a plugin instance (`support_mail.send_email`).                              |
| **Owner**           | The person running a case. Human requests and notifications go to them.                                                                                                 |
| **Human request**   | Something a case needs from its owner: a **question** (from `ask_human`) or an **approval** (a tool that requires one). Owned by the core, answerable from any channel. |
| **Human channel**   | Where human requests are delivered and answered. The web client is the built-in channel; plugins such as Discord can add more.                                          |

______________________________________________________________________

## 3. Architecture

```
┌───────────────────────────┐
│        Web client         │  cases, timeline, inbox, instructions, files, prompts,
└─────────────┬─────────────┘  plugins (status, test, reload)
              │ HTTPS / JSON (REST), same origin
┌─────────────▼──────────────────────────────────────────────────────┐
│ Server process (Rust)                                              │
│                                                                    │
│  ┌──────────────────────┐        ┌───────────────────────────┐     │
│  │ REST API (rouille)   │───────▶│ Engine operations         │     │
│  │ thread per request   │        │ create / message / answer │     │
│  │ + embedded web UI    │        │ wake / cancel / files     │     │
│  └──────────────────────┘        └─────────────┬─────────────┘     │
│                                                │ enqueue           │
│  ┌──────────────────────┐        ┌─────────────▼─────────────┐     │
│  │ Scheduler thread     │───────▶│ Work queue (DB-backed)    │     │
│  │ wait-condition checks│ enqueue└─────────────┬─────────────┘     │
│  │ Channel thread:      │                      │ claim             │
│  │ outbox + answer polls│        ┌─────────────▼─────────────┐     │
│  └──────────┬───────────┘        │ Worker pool (N threads)   │     │
│             │                    │ runs Activations          │     │
│             │                    └───┬──────────────┬────────┘     │
│             │                        │              │              │
│  ┌──────────▼────────────────────────▼───┐   ┌──────▼──────────┐   │
│  │ Plugin host                           │   │ LLM provider    │   │
│  │ registry · processes · watch + reload │   │ + model catalog │   │
│  └──────────┬────────────────────────────┘   └──────┬──────────┘   │
│             │                                       │              │
│  ┌──────────▼───────────────────────────────────────▼───────────┐  │
│  │ Storage: SQLite (rusqlite, WAL) · file bytes in data/files/  │  │
│  └──────────────────────────────────────────────────────────────┘  │
└───────────────┬───────────────────────────────────┬────────────────┘
                │ IMAP/SMTP · Discord REST          │ HTTPS
     Mail server · Discord API                LLM provider API
```

Everything in the diagram is built. The channel thread sends queued channel messages
and polls channels for answers (§10.3). The plugin host so far only serves human
channels: it loads `plugins_dir`, runs plugin processes, checks each instance, and
reloads when the directory changes (§9.4). Plugin tools and plugin wait-condition checks
are planned.

### 3.1 Why this shape

- **rouille is synchronous.** It runs one thread per request with no async runtime.
  The whole server follows the same model: blocking I/O, `std::thread`, mutexes and
  condition variables, and DB-backed queues. Blocking crates fit naturally: `rusqlite`,
  `ureq` for HTTP to the LLM, and later `imap` and `lettre` (sync transport).
- **API threads never run the LLM.** Requests only read or write state and enqueue work.
  Long-running activations happen on the worker pool, so the API stays responsive.
- **The DB is the source of truth for runtime state.** The queue, leases, wait
  conditions and transcripts all live in the database, so a crash or restart loses
  nothing (see §17). Configuration (server settings, plugin instances, prompts) lives in
  files next to the server instead (§9.4, §18).
- **External plugins are child processes** (built). Python plugins run as long-lived
  processes owned by the plugin host and are called over stdio (§9.8). The host treats
  them exactly like compiled-in plugins.

### 3.2 Repository layout

```
crates/
  core/          # domain types and traits (LlmProvider, …); no I/O          (built)
  storage/       # rusqlite repositories, embedded migrations                (built)
  engine/        # activation loop, scheduler, workers, prompts, files       (built)
  llm-openai/    # OpenAI-compatible adapter                                 (built)
  server/        # rouille routes, config, CORS, model catalog,              (built)
                 # plugin manager (load, watch, reload), main()
  plugin-host/   # plugin loading, config files, ProcessPlugin               (built)
  plugin-email/  # IMAP/SMTP plugin, compiled in                             (planned)
plugin/
  discord/       # Discord human channel, an external Python plugin          (built)
web/             # the web client, embedded in the server binary             (built)
docs/
```

Plugins depend on `core`, never the other way round. The repository's `plugin/` directory
is what gets mounted as `/plugins` in a container (§18).

______________________________________________________________________

## 4. Case lifecycle (built)

```
            create
              │
              ▼
          ┌────────┐   worker claims   ┌─────────┐
          │pending │──────────────────▶│ running │◀───────────────────┐
          └────────┘                   └────┬────┘                    │
                                            │                         │
         ┌──────────────┬──────────────┬────┴──────────┐              │
         │ sleep(...)   │ ask_human    │ complete      │ fail/error   │
         ▼              ▼              ▼               ▼              │
    ┌──────────┐ ┌─────────────────┐ ┌───────────┐ ┌────────┐         │
    │ sleeping │ │waiting_for_human│ │ completed │ │ failed │         │
    └────┬─────┘ └───────┬─────────┘ └─────┬─────┘ └───┬────┘         │
         │ condition     │ answered        │ message, file,           │
         │ fired / timed │ / message /     │ instruction change       │
         │ out / manual  │ file / …        │ (reopens the case)       │
         └───────────────┴─────────────────┴──── enqueue activation ──┘

   any non-terminal state ── cancel ──▶ cancelled
```

| From                | Event                                                                                            | To                    |
| ------------------- | ------------------------------------------------------------------------------------------------ | --------------------- |
| `pending`           | a worker claims the case                                                                         | `running`             |
| `running`           | the LLM calls `sleep`                                                                            | `sleeping`            |
| `running`           | the LLM calls `ask_human`, answers twice without a tool call, or (planned) a tool needs approval | `waiting_for_human`   |
| `running`           | the LLM calls `complete`                                                                         | `completed`           |
| `running`           | the LLM calls `fail`, a budget is exhausted, or the LLM fails for good                           | `failed`              |
| `running`           | the LLM is unavailable (retryable errors); retried later                                         | `pending`             |
| `sleeping`          | a wait condition fires or times out, or a manual wake                                            | `pending` → `running` |
| `waiting_for_human` | the question is answered, or an `also_wait_for` condition fires or times out                     | `pending` → `running` |
| any but `cancelled` | a message, a file, or an instruction change                                                      | `pending` → `running` |
| any non-terminal    | cancel                                                                                           | `cancelled`           |

Terminal states: `completed`, `failed`, `cancelled`. A message, a new file or an
instruction change **reopens** a `completed` or `failed` case. A cancelled case cannot
be reopened.

______________________________________________________________________

## 5. Activation loop (built)

An activation is one run of the agent loop for one case, executed by a worker thread.
Every step is committed before the next one starts, and the event log is re-read at
each step, so a crashed activation resumes exactly where it stopped.

```
fn run_activation(case):
    loop:
        stop if the case is no longer running (cancelled) or the server is shutting down
        events = the case's full event log
        if the last LLM turn has tool calls without results:
            run them (one transaction each); stop if one suspends or finishes the case
            continue                         # never ask the LLM twice for the same turn
        fail the case if a budget is exhausted (§5.2)
        request = system prompt + transcript rebuilt from events (§7.1)
        response = llm.complete(request)     # retried on retryable errors (§17.2)
        record llm_message and token usage
        if the response has no tool calls:
            first time: record a nudge ("call a tool")
            second time in a row: ask the owner, using the LLM's text as the question
```

When a call suspends or finishes the case (`sleep`, `ask_human`, `complete`, `fail`),
the remaining calls of the same turn are recorded as "not run" in the same transaction,
so they are never picked up later.

### 5.1 Core tools (built)

| Tool          | Arguments                                                 | Effect                                                                                                             |
| ------------- | --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `sleep`       | `conditions: [WaitCondition]`, `reason`                   | Suspends the case until **any** condition fires or times out.                                                      |
| `ask_human`   | `question`, `timeout?`, `also_wait_for?: [WaitCondition]` | Creates a question for the owner and suspends until it is answered, or an `also_wait_for` condition fires (§10.2). |
| `complete`    | `summary`, `result?` (any JSON)                           | Finishes the case successfully.                                                                                    |
| `fail`        | `reason`                                                  | Finishes the case as failed.                                                                                       |
| `note_set`    | `key`, `value`                                            | Saves a durable note (§7.2).                                                                                       |
| `note_delete` | `key`                                                     | Deletes a note.                                                                                                    |
| `read_file`   | `file`, `offset?`, `max_chars?`                           | Reads a case file's text in chunks (§7.5). Offered only when the case has files.                                   |
| `view_image`  | `file`                                                    | Shows an image file to the model (§7.5). Offered only when the case has files and the model has `vision = true`.   |
| `read_guide`  | `name`                                                    | Returns a plugin guide: instructions for a kind of task (§9.9). Offered only when a plugin offers guides.          |

Next to these, every case is offered the tools of every loaded command plugin (§9.9), e.g.
`youtube_transcript`. A plugin tool cannot take a core tool's name.

A `WaitCondition` as seen by the LLM:

```json
{
    "kind": "core.timer",
    "params": { "after": "2h" },
    "timeout": "3d"
}
```

`core.timer` takes exactly one of `after` (a duration such as `"2h"`) or `at` (an RFC 3339
time). It is the only kind the LLM can request today; plugin kinds such as
`support_mail.reply_received` (with a `check_every` interval) come with the plugin host.
`core.human_input` is internal: `ask_human` creates it, and it cannot be requested
directly.

### 5.2 Budgets (built)

Budgets protect against runaway loops and runaway cost. Defaults come from the server
config and can be overridden per case.

| Budget                     | Default   | Checked                               |
| -------------------------- | --------- | ------------------------------------- |
| `max_activations`          | 20        | when an activation starts             |
| `max_turns_per_activation` | 30        | before each LLM call                  |
| `max_total_tokens`         | 2 000 000 | before each LLM call (input + output) |

An exhausted budget fails the case with a `budget exceeded: …` reason. Sending the case a
message reopens it. Planned: per-activation tool-call and wall-clock limits, a maximum
case age, and a cost budget in dollars (which needs per-model prices).

______________________________________________________________________

## 6. Scheduler & wake-ups (built)

This is the part that makes cases "sleep for free".

### 6.1 Wait conditions table

When a case sleeps, each condition becomes a row in `wait_conditions`:

| column                  | notes                                                                     |
| ----------------------- | ------------------------------------------------------------------------- |
| `id`, `case_id`         |                                                                           |
| `kind`, `params` (JSON) | e.g. `core.timer`, `{"after": "2h"}`, or `email_reply_received`           |
| `next_check_at`         | when it should be evaluated next; `NULL` if never polled                  |
| `deadline_at`           | when it times out; `NULL` for no timeout                                  |
| `status`                | `active` / `fired` / `timed_out` / `cancelled`                            |
| `check_every_ms`        | plugin kinds: the interval, `check_every` clamped to the plugin's minimum |
| `cursor`                | plugin kinds: what the last check handed back                             |
| `failures`              | plugin kinds: checks in a row that failed                                 |
| `created_at`            |                                                                           |

Built-in kinds start with `core.`; any other kind belongs to a plugin (§9.9). When `sleep`
asks for a plugin kind, its params are validated by the plugin and its first check runs
right away (it may already find what the case waits for, and it sets the starting point
for kinds that look for new things).

### 6.2 Scheduler tick and plugin checks

A dedicated **scheduler thread** wakes every 15 s (and whenever work is signalled), takes
up to 100 active conditions that are due, and fires each one: `core.timer` at its time,
and any condition, plugin kinds included, at its deadline (`timed_out`). Because of the
tick, a short timer fires up to 15 s late.

A separate **check thread** runs plugin checks, so a slow mail server can never delay
timers, activations or other cases:

```
every 5 s (or when signalled), for up to 20 due plugin conditions:
    result = condition.check(params, cursor)          # outside any transaction
    Pending { cursor }   → save cursor, next check after the ramp delay (below)
    Fired { events }     → save cursor, fire(cond, events)
    Error(e)             → failures + 1; retry after 1×, 2×, then 4× the interval;
                           after 5 failures in a row, fire(cond, [{ error, failed_checks }])
                           so the LLM (or the owner) can react
    plugin not loaded    → counts as an error (it may come back after a reload)

fire(cond, payload), in one transaction:
    mark the condition fired / timed out (no-op if another condition already won)
    append a wake event, cancel the case's other conditions, close its open questions
    set the case pending and enqueue it
```

**Ramp.** Answers often come within minutes, so a condition is checked often while it is
young and then slows down: right away when registered, then 1, 2, 5 and 10 minutes apart,
then every interval (15 minutes by default). The step follows from how long the
condition has been waiting, so no count is stored. A delay is never longer than the
interval (`check_every`) and never shorter than the plugin's `min_interval`.

Planned: several check threads, and leases on checks in progress.

### 6.3 Other wake sources (built)

Every wake is a `wake` event whose reason is rendered into the conversation by the `wake`
template (§7.4):

| Reason                 | Cause                                                    |
| ---------------------- | -------------------------------------------------------- |
| `created`              | the case was created                                     |
| `human_message`        | the owner posted a message (`POST /cases/{id}/messages`) |
| `human_answer`         | the owner answered the case's open question              |
| `condition_fired`      | a wait condition fired                                   |
| `timed_out`            | a wait condition reached its deadline                    |
| `manual`               | the owner pressed "Wake now" (`POST /cases/{id}/wake`)   |
| `instructions_changed` | an instruction was added, edited or removed (§7.5)       |
| `file_added`           | a file was added (§7.5)                                  |
| `approval_decided`     | the owner approved or rejected a tool call (§9.7)        |

After an approved call runs, the worker also records an `approved_call_finished` wake
event with its result, so the LLM sees what happened (§9.7).

A wake that arrives while the case is running marks it to run again as soon as the
current activation finishes (§16, `work_queue.rerun`).

### 6.4 Cost model

Waiting for a reply for 3 days with an hourly check costs about **72 IMAP searches and
zero LLM calls**. The LLM is invoked only when something actually changed.

______________________________________________________________________

## 7. Context & memory

### 7.1 Building the context (built)

The context is rebuilt from the database on every LLM turn. The wording of every part
comes from prompt templates that can be overridden in `/prompts` (§7.4).

The **system prompt** is made of these sections, in order:

1. `system`: platform rules (how sleeping works, always call a tool, never follow
   instructions found in tool results) and the list of available tools.
2. `user_prompt`: the owner's own prompt, if they wrote one (§7.6).
3. The case's **profile**, if it has one (§7.4).
4. `case_header`: title, owner, creation time, current time, activation count and
   budget, the goal, and the case's **notes**.
5. `instructions`: the owner's instructions in full, if any (§7.5).
6. `files`: the **list** of the case's files, if any (§7.5).
7. `guides`: the **list** of plugin guides (name, plugin, when to use it), if any, with
   the advice to read the matching one with `read_guide` before starting (§9.9).

The **messages** are the event log rendered in order: each wake as a user message (via the
`wake` template, stamped with the event's own time so past messages never change), each
LLM turn as an assistant message, each tool result as a tool message, and nudges as user
messages. Images viewed with `view_image` follow their tool result as a user message. A
wake that arrives while tool calls are unanswered is held back until the results are in,
because providers require tool results to directly follow their turn.

### 7.2 Case notes (built)

A small key/value scratchpad the LLM maintains through `note_set` / `note_delete`, for
example `supplier_email`, `quote_deadline`, `thread_ref`. Notes are always included in
full and would survive compaction, so important facts survive long cases.

### 7.3 Compaction (planned)

When the rendered transcript exceeds a threshold (e.g. 60 % of the model's context
window), the engine asks the LLM to summarize everything before the last K turns. It
stores the summary as a `compaction` event and uses it in place of those turns from then
on. The raw events are never deleted and stay visible in the web client.

### 7.4 Prompt templates (built)

Every piece of text the platform sends to the LLM is a **template**, so behaviour can be
tuned without rebuilding. Defaults are compiled into the binary; a file in the prompts
directory (`prompts_dir` in the config, `/prompts` in a container) replaces the default of
the same name.

```
/prompts/
  system.md          platform rules and the tool list
  case_header.md     title, owner, times, budget, goal, notes
  instructions.md    how the owner's instructions are presented (§7.5)
  files.md           how the list of case files is presented (§7.5)
  guides.md          how the list of plugin guides is presented (§9.9)
  user_prompt.md     how the owner's own prompt is presented (§7.6)
  wake.md            how each wake reason is presented (§6.3)
  nudge.md           sent when the LLM answers without calling a tool (§5)
  profiles/
    quotes.md        e.g. "you negotiate quotes with tradespeople for {{ case.owner }}"
```

- **Templates** use `minijinja` (Jinja2 syntax) with strict undefined variables. The
  variables are `now`, `case` (title, goal, owner, created_at), `budgets`, `usage`,
  `notes`, `tools`, `instructions`, `files`, `guides`, `user_prompt`, and `wake` for the
  wake template.
- **Profiles** are optional behaviour packs. A case chooses one with `"profile": "quotes"`
  when it is created, or gets `default_profile` from the server config. The profile is
  rendered after the system template.
- **Loading and reload.** Prompts are read at startup and on reload (`SIGHUP` or
  `POST /admin/reload`), and each template is rendered against sample data covering every
  variable (every wake reason, for `wake`). A file that fails is reported (log, and
  `GET /prompts`) and the previous version, or the built-in default, stays in use. A bad
  prompt file never stops the server.
- **Effect on running cases.** The context is rebuilt on every turn, so a prompt change
  applies to every case from its next turn, including cases that are asleep.
- **Traceability.** Each activation records the SHA-256 of every template it used
  (`activations.prompt_hashes`), so a change in behaviour can be traced to a prompt edit.
- **Plugin guidance (planned).** A plugin can ship a `prompt.md` with advice on using its
  tools, added only for cases that enable that plugin; `/prompts/plugins/<id>.md`
  overrides it.

### 7.5 Instructions and files (built)

The owner gives a case two kinds of material, kept apart on purpose because the system
prompt is resent with **every** LLM turn:

|               | Instructions                                                                  | Files                                                               |
| ------------- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------- |
| What          | Short markdown or text written to steer the case: tone, constraints, contacts | Material that arrives: PDFs, images, emails, text, CSVs             |
| When          | At creation, so the first run follows them; added, edited or removed later    | Only while the case runs                                            |
| In the prompt | Always, in full (`instructions` template)                                     | Never; only listed by name, kind, size and pages (`files` template) |
| Read how      | Already in context                                                            | On demand: `read_file` (chunks) and `view_image`                    |
| Limits        | 10 per case, 20 000 characters each, 50 000 in total                          | 20 MB per file, 20 files and 100 MB per case                        |
| Stored        | `instructions` table (editable)                                               | Bytes in `data/files/<id>`, metadata and extracted text in `files`  |

Every change is an event: adding, editing or removing an instruction wakes the case with
`instructions_changed`, and adding a file wakes it with `file_added`. Plugins will add
files the same way, e.g. email attachments.

**Files.** Their kind is detected from the bytes, never the name or the client's claim:

- PDF (`%PDF-`): the text layer is extracted at upload with `pdf-extract`, and pages
  counted with `lopdf`. A scanned PDF has no text layer and is kept, marked unreadable.
- Images: PNG, JPEG, GIF and WebP, by their magic bytes.
- Text: anything that is valid UTF-8 without NUL bytes.
- Anything else is refused with an explanation.

The bytes are written first (write, then rename), then the row and the wake event go in
one transaction; if it fails, the bytes are removed again.

**Reading files.** `read_file(file, offset?, max_chars?)` returns up to 20 000 characters
(at most 40 000) with `next_offset` while there is more. A file is found by id, exact name,
or name ignoring case. `view_image(file)` attaches the image as a user message right after
the tool results; only the four most recent images stay in the conversation, since images
are resent too. `view_image` exists only when the case's LLM has `vision = true`;
otherwise images are listed as unreadable for the current model.

Both come from the owner and are treated as the owner's information. Files from third
parties (email attachments) will need to be marked as such.

______________________________________________________________________

### 7.6 The owner's prompt (built)

The owner can write one prompt of their own, added to **every** case's system prompt right
after the platform rules and before the case's profile, instructions and header: standing
preferences such as their name and signature, tone, or limits ("ask me before agreeing
to anything over $500"). A case's own instructions come later and win where they differ.

- It is a plain text file, `user_prompt.md` in the data directory, editable on the web
  client's Prompts page (`GET` / `PUT /user-prompt`) or by hand. No history is kept.
- It is read on every LLM turn, so an edit applies to every case from its next turn,
  with no reload. A missing or blank file means no owner prompt; an unreadable one is
  logged and skipped.
- At most 20 000 characters, since it is resent with every turn of every case. Saves
  are written to a temporary file and renamed into place.
- Each activation records its SHA-256 with the template hashes (`prompt_hashes`), so a
  change in behaviour can be traced to an edit of it (§7.4).

It differs from instructions (§7.5), which belong to one case, and from profiles (§7.4),
which a case opts into.

## 8. LLM providers (built)

```rust
pub trait LlmProvider: Send + Sync {
    /// Model used when a case does not name one.
    fn default_model(&self) -> &str;
    /// One completion; blocks until the provider answers.
    fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, LlmError>;
    /// Tool-capable models the endpoint offers, when it can say (default: unknown).
    fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> { Ok(Vec::new()) }
    /// Whether the model can be shown images (default: no).
    fn supports_images(&self) -> bool { false }
}

pub struct CompletionRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,     // User { text, images } | Assistant | Tool { tool_call_id, content }
    pub tools: Vec<ToolSpec>,       // name, description, JSON Schema of the arguments
}

pub struct CompletionResponse {
    pub message: AssistantMessage,  // text and/or tool calls, normalized
    pub usage: TokenUsage,          // input and output tokens, for budgets
}
```

- **OpenAI-compatible Chat Completions** is the one adapter (`crates/llm-openai`, blocking
  `ureq`). It covers OpenAI, OpenRouter, Ollama, vLLM, LM Studio and anything speaking the
  same API; only `base_url`, `api_key` and `model` change. Images are sent as `image_url`
  parts carrying `data:` URLs. Tool arguments that come back as invalid JSON are kept as a
  string so the tool can report the error to the LLM.
- **Errors.** `LlmError::Retryable` (429, 408, 409, 5xx, network) and `LlmError::Fatal`
  (other 4xx, unparsable responses). See §17.2 for what happens next.
- **Choosing the model.** Each `[llm.<name>]` in the config is one endpoint with a default
  `model`. A case picks an LLM by name (`default_llm` otherwise) and may override the
  model with any id.
- **Model catalog.** A background thread calls each LLM's `{base_url}/models` at startup
  and every hour (`discover_models`, on by default). When the provider says which models
  support tools (OpenRouter does), only those are kept, with their prices per million
  tokens and context size. `GET /api/v1/llms` offers the default model, then the config's
  `models` suggestions, then the discovered ones. The catalog is never fetched from a
  request thread, and a failed refresh keeps the previous list.
- **Vision.** `vision = true` on an LLM enables `view_image` for its cases (§7.5).
- **Cost estimate.** The catalog's prices also give each case an estimated cost so far
  (§14.1), shown in the case header.
- Planned: tool-name sanitizing for providers that forbid `.` (needed for namespaced
  plugin tools, §9.6), and other native adapters behind the same trait.

______________________________________________________________________

## 9. Plugin system (partly built)

**Built** (`crates/plugin-host`): loading `plugins_dir`, manifests, `config.toml`
instances with secret references, external plugins as child processes over JSON-RPC
(§9.8), and the human-channel half of a plugin (§10.3), which is what the Discord plugin
needs. **Planned** (milestone 4): the `Plugin` trait below with tools and wait
conditions, built-in plugins, `plugin_kv`, and approvals.

### 9.1 Principles

- The core knows nothing about email, calendars, HTTP APIs, etc. It only knows the
  `Plugin` trait.
- A plugin contributes up to four things: **configuration**, **tools**,
  **wait-condition kinds**, and a **human channel** (§10.3).
- Plugins are **stateless code**. Their configuration lives in files (§9.4); all
  runtime state lives in the database: condition cursors and a key/value store provided
  by the host.
- A plugin must be configured, as a **plugin instance**, before any case can use it.

### 9.2 The `Plugin` trait

```rust
pub trait Plugin: Send + Sync {
    /// Stable identifier, e.g. "email".
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn version(&self) -> &'static str;

    /// JSON Schema of the instance configuration. Fields marked
    /// `"x-secret": true` hold secrets: they are redacted by the API and never logged.
    fn config_schema(&self) -> serde_json::Value;

    /// Semantic validation beyond the schema. Should also try to connect,
    /// so the web client's "Test connection" button can show a real result.
    fn validate_config(&self, cfg: &serde_json::Value) -> Result<(), PluginError>;

    /// Tools exposed to the LLM for a given instance.
    fn tools(&self, inst: &InstanceCtx) -> Vec<ToolSpec>;

    /// Execute a tool call.
    fn call_tool(&self, inst: &InstanceCtx, case: &CaseCtx,
                 name: &str, args: serde_json::Value) -> ToolOutcome;

    /// Wait-condition kinds this plugin can evaluate.
    fn wait_condition_kinds(&self) -> Vec<WaitConditionSpec>;   // name, params schema, min/max interval

    /// Cheap, deterministic check. MUST NOT call the LLM.
    fn check(&self, inst: &InstanceCtx, case: &CaseCtx,
             kind: &str, params: &serde_json::Value,
             cursor: Option<&serde_json::Value>) -> CheckResult;

    /// Optional health check shown in the web client.
    fn healthcheck(&self, inst: &InstanceCtx) -> Result<(), PluginError> { Ok(()) }

    /// Optional: this plugin can deliver human requests and receive answers (§10.3).
    fn human_channel(&self) -> Option<&dyn HumanChannel> { None }
}

pub enum ToolOutcome {
    Ok(serde_json::Value),
    Err(String),                                  // shown to the LLM as a tool error
    NeedsApproval { summary: String, action: serde_json::Value },
}

pub enum CheckResult {
    Pending { cursor: Option<serde_json::Value> },
    Fired   { events: Vec<serde_json::Value>, cursor: Option<serde_json::Value> },
    Error(PluginError),
}
```

`InstanceCtx` gives the plugin its resolved config (secret references already replaced
by their values, §9.4), the instance's key/value store, and a logger. `CaseCtx` gives it
the case id and a scoped key/value store; for example, the email plugin uses it to
remember which Message-IDs this case sent.

### 9.3 Plugin directory & manifest

Every plugin lives in its own self-contained sub-directory of the plugins directory
(`plugin/` in the repository, `/plugins` in a container, §18). The directory holds the
plugin's manifest, its configuration and, for external plugins, its code:

```
/plugins/
  discord/                     human channel, JSON-RPC process plugin (§12)
    plugin.toml
    config.toml                git-ignored; config.example.toml is the template
    config.example.toml
    check_config.py            checks the setup against Discord (below)
    schema.json
    discord_plugin.py
    test_discord_plugin.py
    README.md
  email/                       command plugin: tools, wait conditions, a guide (§11)
    plugin.toml
    config.toml / config.example.toml
    check_config.py
    email_tool.py
    test_email_tool.py
    guides/email.md
    README.md
  documents/                   command plugin: metadata, OCR, text of case files (`file` args)
    plugin.toml
    check_config.py
    docs_tool.py
    test_docs_tool.py
    README.md
  shell/                       command plugin: bash in the sandbox container (sandbox/sandboxd.py)
    plugin.toml
    config.example.toml
    check_config.py
    shell_tool.py
    test_shell_tool.py
    README.md
  web/                         command plugin: search, pages as text, page and feed watches
    plugin.toml
    config.example.toml
    check_config.py
    web_tool.py
    test_web_tool.py
    README.md
  weather/                     command plugin: forecasts and past weather (Open-Meteo)
    plugin.toml
    config.example.toml
    check_config.py
    weather_tool.py
    test_weather_tool.py
    README.md
  youtube_transcribe/          command plugin: tools and guides (§9.9)
    plugin.toml
    check_config.py
    scripts/yt.py
    guides/*.md
    README.md
```

**Every plugin ships a `check_config.py`** that checks, against the real services, that
the plugin will work on this machine with this configuration: it reads `config.toml` and
resolves secret references exactly as the server does, runs read-only checks (logins,
permissions, folders, required programs), and prints a checklist (`ok`, `FAIL`, `warn`,
with a fix) and exits `1` on failure; anything with an outside effect (a test message) is
opt-in behind a flag. Where the plugin reports problems to the server at load
(`validate_config`), both use the same code. The contract is in `plugin/AGENT.md`.

`plugin.toml` is the manifest:

```toml
id = "weather"                 # must match the directory name
name = "Weather"
version = "0.1.0"
runtime = "python"             # "builtin" | "python" | "exec"
protocol = 1                   # plugin protocol version (§9.8), external plugins only
entrypoint = "plugin.py"       # python: script run with `python`; exec: any executable
config_schema = "schema.json"  # external plugins only; built-ins provide it in code
concurrency = 1                # processes to run in parallel (§9.8)

[[tools]]
name = "forecast"
description = "Get the forecast for the next N days."
args_schema = { type = "object", properties = { days = { type = "integer" } } }

[[wait_conditions]]
name = "rain_expected"
description = "Fires when rain is forecast within the given window."
params_schema = { type = "object", properties = { within = { type = "string" } } }
min_interval = "15m"

[human_channel]                # optional: the plugin implements HumanChannel (§10.3)
```

- **`runtime = "builtin"`**: the plugin's code is compiled into the server (e.g. email),
  registered by id in a static registry. The directory only enables it and holds its
  configuration; tools, schemas and conditions come from the code.
- **`runtime = "python"` / `"exec"`**: an **external plugin**. The host wraps it in a
  `ProcessPlugin` adapter that implements the same `Plugin` trait by talking to a child
  process (§9.8). Tools, wait conditions and the config schema come from the manifest.
- A plugin directory that is missing, or whose manifest is invalid, is logged and skipped.
  It never prevents the server from starting.

The core never knows which kind of plugin it is talking to. Adding a Python plugin is
dropping a directory into `/plugins` and reloading, with no rebuild.

### 9.4 Plugin instances & configuration

`config.toml` in the plugin's directory defines its **instances**. The files are the
source of truth: instances are not created or edited through the API or the web client.

```toml
# /plugins/email/config.toml
[instances.home_mail]
from_address = "joe@example.com"
requires_approval = ["send_email", "reply"]

[instances.home_mail.imap]
host = "imap.mail.yahoo.com"
username = "joe@example.com"
password = { secret = "yahoo_app_password" }   # read from /run/secrets/yahoo_app_password

[instances.home_mail.smtp]
host = "smtp.mail.yahoo.com"
username = "joe@example.com"
password = { env = "YAHOO_APP_PASSWORD" }      # or from an environment variable
```

1. **Loading (built).** At startup (before the server listens) and on every reload, the
   host reads every `config.toml`, resolves secret references (`{ secret = … }` from the
   secrets directory, `{ env = … }` from the server's environment) exactly as the server
   config does (§18.2), then calls `validate_config()`, which checks the whole setup
   (§12). Planned: validating against the plugin's `schema.json` first.
2. **Secrets stay out of the database.** Resolved values live only in memory. Fields
   marked `x-secret` should use a reference; a literal value is accepted but logged as a
   warning.
3. **Reload (built).** The server watches `plugins_dir` (every 3 s; a change must hold
   for 1 s so a save in progress isn't loaded) and reloads on any change to a plugin's
   files, ignoring hidden files, `__pycache__` and `*.log`. `SIGHUP`, `POST /admin/reload`
   and `POST /plugins/reload` reload too. A reload loads and checks everything into a new
   registry, swaps it in whole, then shuts down the old processes; a retired process never
   restarts. Messages queued for a channel that is briefly missing are retried. The
   result, errors included, is shown on the web client's Plugins page (§15). Planned:
   keeping a plugin's old instances when its new config is invalid.
4. **Removed or disabled instances** (`enabled = false`) fail their pending wait
   conditions gracefully, and affected cases are woken with an `instance_disabled` event.
5. Instance names are unique across all plugins; they are the stable id cases and wait
   conditions refer to.

### 9.5 Enabling plugins per case

A case lists the plugin instances it may use (`plugin_instances: ["support_mail"]`).
Only their tools and condition kinds are exposed to the LLM, which keeps the tool list
small and limits the blast radius.

### 9.6 Namespacing

Tools and wait conditions are namespaced by **instance name**:
`support_mail.send_email`, `support_mail.reply_received`. Two email instances can be
enabled on one case without clashing. Providers that forbid `.` in tool names get them
mapped by the adapter (§8).

### 9.7 Approvals (built)

A tool with outside effects (sending an email) can require the owner's approval: in a
command plugin, `requires_approval = true` on the tool, with an `approval` template that
says what a call does (`"Email {to}: {subject}"`, §9.9).

1. **The call.** When the LLM calls such a tool, its arguments are validated first (an
   invalid call is a plain tool error). Then, in one transaction, the engine stores an
   **approval** human request (summary, tool, arguments), a `core.human_input` wait,
   and one channel delivery per human channel (§10.3), and records the tool result
   `{ status: "waiting_for_approval", request_id }`. The case is `waiting_for_human`;
   nothing ran. Other calls of the same turn are recorded as not run.
2. **The decision.** The owner approves or rejects from the web (where they can also edit
   the arguments and add a comment) or from Discord (✅/❌). The first decision wins, as
   for questions (§10.1). An approval becomes `execution = pending` and the case wakes
   with `approval_decided` (decision, comment, edited arguments). A message to the case
   instead of a decision wakes it too, and drops the approval.
3. **Running it, at most once.** At the start of each step, the worker runs the case's
   approved calls: it first moves the call to `running` in its own transaction, then runs
   the tool with the final arguments outside any transaction, then records an
   `approved_call_finished` wake event with the result (stored as a case file if it is
   long) and moves the call to `done`. A call found still `running` was interrupted by a
   crash: it is reported as having an unknown outcome and never run again, since running
   it twice could send an email twice.

A rejection is never run; the LLM is told, with the owner's comment, and decides what to
do next.

### 9.8 External plugin protocol

External plugins run as **child processes** of the server and speak **JSON-RPC 2.0 over
stdin/stdout**, one JSON message per line. stderr is captured into the server log with
the plugin id attached.

**Starting (built).** For `runtime = "python"`, the host runs `python3 <entrypoint>` in
the plugin's directory; `runtime = "exec"` runs the entrypoint directly. The process
starts on first use and answers `initialize` before anything else. It gets a minimal
environment: no server secrets, only `PATH`, `HOME`, `TZ`, `LANG` and `LC_ALL`.
Planned: running with `uv run --with-requirements requirements.txt` when the plugin has
dependencies, with the cache on the data volume (`UV_CACHE_DIR=/data/cache/uv`) so the
plugin directory can stay read-only.

**Methods (host → plugin).** They mirror the `Plugin` and `HumanChannel` traits:

| Method                                     | Params                                                                   | Result                                                                |
| ------------------------------------------ | ------------------------------------------------------------------------ | --------------------------------------------------------------------- |
| `initialize`                               | `protocol`, `plugin_id`                                                  | `{ protocol }`, used to reject mismatched versions                    |
| `validate_config`                          | `instance`, `config`                                                     | `{}` or an error                                                      |
| `call_tool`                                | `instance`, `config`, `case`, `state`, `tool`, `args`, `idempotency_key` | `{ ok }`, `{ error }` or `{ needs_approval }`, plus `state_updates`   |
| `check`                                    | `instance`, `config`, `case`, `state`, `kind`, `params`, `cursor`        | `{ status: pending \| fired, events?, cursor }`, plus `state_updates` |
| `healthcheck`                              | `instance`, `config`                                                     | `{}` or an error                                                      |
| `deliver`, `poll`, `on_resolved`, `notify` | as in §10.3, plus `instance`, `config`                                   | as in §10.3                                                           |
| `shutdown`                                 |                                                                          | the process exits                                                     |

Errors are JSON-RPC errors: `-32700` invalid JSON, `-32600` not a request, `-32601`
unknown method, `-32602` invalid params, `-32000` a failure, with `data.retryable` telling
the host whether trying again can help.

- **Stateless processes.** Every call carries the instance's resolved `config` and the
  case-scoped key/value `state` for that plugin. The plugin returns `state_updates`,
  which the host writes to `plugin_kv` in the same transaction as the call's event. A
  plugin process never needs a database and can be restarted at any time.
- **One process serves every instance** of its plugin, because config comes with each
  call. Each process handles one call at a time. Planned: `concurrency` in the manifest
  sets how many processes run in parallel (one today).
- **Timeouts** per method (built: `poll` 60 s, `initialize` 10 s, the others 30 s;
  planned: `call_tool` 60 s, `check` 30 s). A process that times out, exits or writes
  invalid JSON is killed and started again on the next call, and the failed call is
  reported as a retryable error, which the outbox retries.
- **Checks (built).** Each instance's `validate_config` runs when plugins load (startup,
  before the server listens, and every reload) and on demand (`POST /plugin-instances/{name}/test`, the Plugins page's "Test now"). Its result is kept
  (§14.6) and logged: problems as errors with their fix, warnings as warnings. An instance
  whose check fails or reports problems still loads, so fixing the cause (e.g. inviting
  the bot) and testing again is enough; only an invalid config (a missing secret, no
  `allowed_responders`) keeps it from loading.
- **Shutdown (built).** On exit the host sends `shutdown` and kills what remains.
- **Reload (built)** replaces a plugin's processes after its directory changed (§9.4).
- A small Python helper module that implements the JSON-RPC loop and decorators for tools
  and checks can be shipped later; the protocol above is the contract.

### 9.9 Command plugins (built)

Many useful capabilities already exist as command-line scripts, often written as agent
"slash commands": a script plus instructions on when and how to use it. A **command
plugin** (`runtime = "command"`) turns such a directory into LLM tools without any plugin
protocol: each tool is a command line declared in `plugin.toml`, and instructions become
**guides** the agent reads on demand. `plugin/youtube_transcribe` is the first one.

```toml
id = "youtube_transcribe"
runtime = "command"
requires = ["uv"]                     # programs that must be on PATH, checked at load

[[tools]]
name = "youtube_transcript"           # what the LLM calls; ^[a-zA-Z0-9_-]{1,64}$
description = "Download the transcript of a YouTube video… read it with `read_file`."
command = ["scripts/yt.py", "transcript", "{video}", "--format", "stamped"]
output = "file"                       # auto (default) | text | file | json
file_name = "youtube-{video}.md"
timeout = "3m"                        # default 2m

[tools.options]                       # added only when the optional argument is given
lang = ["--lang", "{lang}"]

[tools.args.video]
description = "YouTube URL or 11-character video id."
required = true

[tools.args.lang]                     # type: string (default) | integer | number | boolean | file
description = "Caption language code, e.g. `de`."

[[guides]]
name = "earnings-call-analysis"
description = "Analysing an earnings call through the Bezos and Buffett frameworks."
file = "guides/earnings-call-analysis.md"
```

- **Who sees them.** Every case is offered every loaded plugin tool, so asking any case
  about a video just works; the LLM picks the tool from its description. Guides are
  listed (name and when to use it) in the system prompt, and read in full only through
  `read_guide`, so long instructions cost nothing until needed. Per-case opt-out is
  future work.
- **Arguments.** The host builds the JSON Schema from `[tools.args]` and checks every call
  against it before anything runs: required arguments, types, `enum`, no unknown
  arguments, no NUL, and at most 2 000 characters unless the argument sets `max_length`
  (up to 100 000, e.g. an email body). An optional argument given as an empty or blank
  string counts as not given (models often send `"cc": ""`). A `{name}` in `command` must be a required
  argument; optional ones go in `options`, and a boolean option adds its arguments when
  true. Values become separate `argv` entries, never shell text. A value that fills a
  whole element (`"{to}"`) may not start with `-`, so the LLM cannot inject options (e.g.
  `--out /etc/passwd`); a value inside a larger element (`"--body={body}"`) may, since it
  can only ever be that option's value.
- **Running.** The program is found relative to the plugin (a path with `/`, which may not
  leave the directory) or on `PATH`. It runs in the plugin directory with the same
  minimal environment as protocol plugins, plus the plugin's `config.toml` `[env]` table,
  whose values may be secret references (e.g. `YT_PROXY_URL = { env = "YT_PROXY_URL" }`).
  stdin is closed; stdout and stderr are read up to 8 MB. On timeout the process is
  killed. A non-zero exit is a tool error for the LLM, carrying the end of stderr.
- **Case files.** An argument of `type = "file"` takes the name of one of the case's
  files (exact, then ignoring case; any other name is an error listing the case's files).
  The command gets a path to it, linked under its own name (so its extension survives)
  in a private temporary directory that is removed after the call; the store itself
  keeps files by id. Plugin tools receive the case's files for this in a `ToolContext`.
- **Approval.** `requires_approval = true` makes every call wait for the owner (§9.7);
  `approval = "Email {to}: {subject}"` is the summary shown to them, with placeholders of
  missing optional arguments dropped.
- **Wait conditions.** `[[conditions]]` declares kinds for `sleep`: `name`,
  `description`, `command`, declared `params` (checked like tool arguments), `interval`
  (default 15m), `min_interval` (default 5m) and `timeout` (default 1m). The command gets
  `{"params": …, "cursor": …}` on stdin and prints `{"status": "pending" | "fired", "events": […], "cursor": …}`; the check thread runs it (§6.2).
- **Output.** `json` parses stdout as the result; `text` returns stdout inline (up to 20 000 characters); `file` stores it as a
  case file (§7.5) and returns its name, size and a preview, so the LLM reads it with
  `read_file` in parts; `auto` returns up to 12 000 characters inline and stores anything
  longer. `file_name` placeholders are made safe (no URL scheme, only letters, digits,
  `.`, `-`, `_`), and a name already used in the case gets ` (2)`. Stored output counts
  toward the case's file limits, and adding it does not wake the case, since the running
  activation already sees it.
- **Outside the transaction.** A plugin tool runs before the tool call's write
  transaction opens, so a slow download never blocks other writers. If the server stops
  mid-call, the call runs again when the case resumes; command tools should therefore be
  safe to repeat (fetching is; tools with side effects wait for the outbox, §17.1).
- **Loading.** A command plugin loads all or nothing: a bad tool, an unreadable guide or
  a missing `requires` program marks the plugin as not loaded, with the reason, on the
  Plugins page. A tool or guide whose name is taken (by a core tool or another plugin) is
  left out and listed as a conflict (§14.6). Reload works as for other plugins (§9.4).

______________________________________________________________________

## 10. Human-in-the-loop

### 10.1 Human requests (built)

Whenever a case needs a person, the core creates a **human request**:

| Kind       | Created by                                                   | Valid answers                                                             | Status |
| ---------- | ------------------------------------------------------------ | ------------------------------------------------------------------------- | ------ |
| `question` | `ask_human`, or two LLM replies in a row without a tool call | free text                                                                 | Built  |
| `approval` | a tool that requires approval (§9.7)                         | approve / reject (with an optional comment); editing the args is web-only | Built  |

Human requests belong to the **core**, not to any plugin. A question can be answered
from the web client (the inbox, the case page, or a message to the case, which answers its
open question) or from any chat channel it was posted to (§10.3). Every answer goes
through one transaction:

```
UPDATE human_requests SET status = 'answered', answer = ?, answered_via = ?, responder = ?
WHERE id = ? AND status = 'open'
  1 row  → fire the case's core.human_input condition, append a human_answer wake
           (with `via`), cancel its other conditions, set the case pending and enqueue it,
           and queue an `on_resolved` update for every channel the question was posted to
  0 rows → already resolved: the API answers 409; a late channel reply is ignored
```

**The first answer wins.** Answering in the web client edits the Discord message to
"Answered via web" and closes its thread; answering in Discord shows "Answered via
discord_joe" in the timeline.

### 10.2 Waiting on a human and something else (built)

`ask_human` accepts `also_wait_for: [WaitCondition]` and `timeout`. For example: _"Ask the
owner for the panel photo, but if Bob replies in the meantime, wake up."_

Internally the question is a `core.human_input` wait condition. The scheduler never polls
it; only the answer fires it, or its deadline times it out. It sits next to any other
conditions, and the first to fire wins. If another condition or the timeout wins, the
question is marked `superseded` and channels update their message to "No longer needed".

### 10.3 Human channels (built for questions and notifications)

The engine only knows this trait (`crates/core/src/channel.rs`). The plugin host
implements it for each instance of an external plugin whose manifest has
`[human_channel]` (§9), by calling the plugin's methods of the same name (§9.8):

```rust
pub trait HumanChannel: Send + Sync {
    fn plugin(&self) -> &str;                       // e.g. "discord"
    fn allowed_responders(&self) -> &[String];      // channel user ids
    fn poll_interval(&self) -> Duration;            // default 20 s, at least 5 s
    fn notifies(&self, event: &str) -> bool;        // `notify_on`

    /// Post a question: `{ kind, case_title, text, case_url }`. The returned value
    /// (e.g. Discord message and thread ids) is stored and handed back later.
    fn deliver(&self, request: &Value) -> Result<Value, ChannelError>;

    /// Look for answers to the questions still open on this channel, since `cursor`.
    /// Called once per instance, not once per case. MUST NOT call the LLM.
    fn poll(&self, open: &[OpenDelivery], cursor: &Value) -> Result<PollResult, ChannelError>;

    /// The question was answered, superseded or cancelled: update the message.
    /// `outcome` is `{ status, via?, responder? }`.
    fn on_resolved(&self, delivery: &Value, outcome: &Value) -> Result<(), ChannelError>;

    /// Informational message: `{ event, case_title, text, case_url }`.
    fn notify(&self, notification: &Value) -> Result<(), ChannelError>;
}

pub struct OpenDelivery { pub request_id: HumanRequestId, pub delivery: Value }

pub struct PollResult {
    pub replies: Vec<ChannelReply>,  // request_id, external_id, responder, text?, decision?, attachments
    pub cursor: Value,               // stored in channel_cursors, handed back next poll
    pub warnings: Vec<String>,       // logged, e.g. a missing permission or intent
}

pub struct ChannelError { pub message: String, pub retryable: bool }
```

Payloads are JSON because they pass through unchanged to the plugin process.

**Outbound.** Messages go through the `channel_deliveries` outbox (§16), written in the
same transaction as what they are about:

| Queued when                                                   | `kind`         | Sent as             |
| ------------------------------------------------------------- | -------------- | ------------------- |
| `ask_human` (or the nudge) opens a question                   | `request`      | `deliver`           |
| a question is answered, superseded or cancelled               | `resolution`   | `on_resolved`       |
| a case becomes `completed` or `failed` (or a budget stops it) | `notification` | `notify`, if wanted |

A **channel thread** in the engine sends due messages in order. Retryable errors are
retried after 10 s, doubling up to 15 minutes, 8 attempts in all; other errors fail the
message at once. A message for a channel that is not loaded (e.g. during a plugin reload,
or while its plugin is broken) counts as retryable, so it goes out once the channel is
back. A failed delivery never blocks the case, because the question is always
in the web inbox. A question already settled before it could be posted is skipped, and so
is the resolution update for a question that was never posted.

**Inbound.** The same thread calls `poll()` for each channel every `poll_interval`, when
it has questions open. The plugin does the channel-specific matching (a thread reply, a
reaction) and returns answers already tied to a `request_id`. For each reply the engine:

1. ignores it unless the question is one of the open ones it passed;
2. checks `responder` against the instance's `allowed_responders` again (the plugin
   already filters; the core does not trust it blindly);
3. records the answer (§10.1) with `answered_via` set to the instance and `responder` to
   the channel user id. An answer that lost the race is ignored.

The cursor is saved after the answers, in `channel_cursors`, so replies sent while the
server was down are picked up after a restart (Discord keeps message history), and a
reply seen twice is harmless because only the first answer counts.

**Activity.** For each channel the thread remembers, in memory, the last successful call,
the last error and the last warning a poll reported (e.g. "a reply had no readable text:
enable the Message Content intent"). The API and the Plugins page show them, and an
error newer than the last success marks the instance as needing attention (§14.6).

**Reload.** The engine's channel set is swapped as a whole when plugins reload (§9.4):
the thread reads the current set on every pass, new cases can only name loaded channels,
and cases keep the channel names they were created with.

Not yet: approval decisions are ignored (approvals don't exist yet, §9.7), and reply
attachments are not downloaded; the answer tells the agent a file was sent and names it.
Replying to a notification to send a case a plain message is future work.

### 10.4 Routing (built)

- Every case has an `owner` and a list of `human_channels`, set at creation (the New case
  form offers the loaded channels). If the list is omitted, `default_human_channels`
  from the server config applies; if that is omitted too, every loaded channel. `[]`
  means the web client only.
- The web client is always a channel and can't be removed.
- Each channel instance chooses which notifications it sends (`notify_on`). Questions are
  always delivered.
- Links back to a case are built from the server's `public_url` (§17.3), as
  `{public_url}/#/cases/{id}`; without it, messages have no link.

______________________________________________________________________

## 11. Email plugin (built)

The email plugin is documented with the plugin, in
[`plugin/email/README.md`](../plugin/email/README.md): configuration, tools, wait
conditions, how replies are matched, and what is planned. It is a command plugin (§9.9)
that sends through approvals (§9.7) and waits with plugin wait conditions (§6.2).

______________________________________________________________________

## 12. Discord plugin (built)

The Discord plugin is documented with the plugin, in
[`plugin/discord/README.md`](../plugin/discord/README.md): configuration, messages,
how it talks to Discord, its setup checks, and what is planned. It is a human channel
(§10.3) running as a JSON-RPC process plugin (§9.8).

Starting cases from Discord is deliberately **not** part of it: the optional
[`intake/discord`](../intake/discord/README.md) service polls a channel for messages that
@mention its bot and creates cases through `POST /api/v1/cases`, like any API client. The
server and the plugin know nothing about it; its cases' questions reach Discord through
`default_human_channels`.

______________________________________________________________________

## 13. End-to-end walkthrough (target)

This is the target experience once the email and Discord plugins are wired. A user
creates a case: _"Ask sales@acme.com for a quote on 200 blue widgets delivered by Oct 15.
Follow up once if there's no answer in 2 days. Summarize the quote when it arrives."_
The case has the `support_mail` instance enabled, `send_email` requires approval, and the
case's human channel is `discord_joe`.

```
User        API/Engine            LLM                  Email plugin        Scheduler
 │ POST /cases │                    │                        │                  │
 │────────────▶│ state=pending      │                        │                  │
 │             │ activation #1 ────▶│                        │                  │
 │             │                    │ send_email(...)        │                  │
 │             │◀───────────────────│                        │                  │
 │             │ requires approval → waiting_for_human       │                  │
 │ approve     │                    │                        │                  │
 │ (web or     │                    │                        │                  │
 │  Discord)   │                    │                        │                  │
 │────────────▶│ activation #2 ─ run send_email ────────────▶│ SMTP send        │
 │             │                    │◀─ {thread_ref} ────────│                  │
 │             │                    │ note_set(thread_ref)   │                  │
 │             │                    │ sleep(reply_received,  │                  │
 │             │                    │   every 1h, timeout 2d)│                  │
 │             │ state=sleeping     │                        │                  │
 │             │                    │                        │◀── check() @+1h ─│ Pending
 │             │                    │                        │◀── check() @+2h ─│ Pending
 │             │                    │                        │◀── check() @+3h ─│ Pending
 │             │                    │                        │◀── check() @+4h ─│ Fired
 │             │ wake event (reply) │                        │                  │
 │             │ activation #3 ────▶│ read_message(id) ─────▶│                  │
 │             │                    │◀── body ───────────────│                  │
 │             │                    │ complete(summary,      │                  │
 │             │                    │   result={price,...})  │                  │
 │             │ state=completed    │                        │                  │
```

Four scheduler checks, three LLM activations. The approval was posted to both the web
inbox and Discord; the first answer resolved it and the other channel's copy was marked
as answered. If the reply had not arrived within 2 days, activation #3 would start with a
`timed_out` wake. The LLM would then send a follow-up with `reply`, sleep again, and
eventually `complete` or `fail`.

What works today is the same flow without the plugins: timers, questions answered in the
web client, messages, instructions and files.

______________________________________________________________________

## 14. REST API (partly built)

Base path `/api/v1`. JSON in and out, except file uploads (raw bytes) and file content.
Auth: `Authorization: Bearer <token>` on everything except `/healthz` and the web client's
static files. Tokens come from the config (`[api] tokens`) and are compared in constant
time. For testing, `[api] require_token = false` (or `CLANKJOB_REQUIRE_TOKEN=false`) serves
every request without one: the server logs a warning at startup, `/healthz` reports
`"token_required": false`, and the web client then skips its sign-in page.

### 14.1 Cases (built)

| Method   | Path                                  | Description                                                                                        |
| -------- | ------------------------------------- | -------------------------------------------------------------------------------------------------- |
| `GET`    | `/cases?state=&cursor=&limit=`        | List cases, newest first. `next_cursor` is set when there may be more. `limit` 1–1000, default 50. |
| `POST`   | `/cases`                              | Create a case (body below). `201` with the case.                                                   |
| `GET`    | `/cases/{id}`                         | The case, notes, active waits, open questions, instructions, files (metadata), and `cost`.         |
| `GET`    | `/cases/{id}/events?after=&limit=`    | Event timeline; poll with `after` = the last `seq` seen.                                           |
| `POST`   | `/cases/{id}/messages`                | `{ "text" }`. Answers the open question if there is one; otherwise wakes (or reopens) the case.    |
| `POST`   | `/cases/{id}/wake`                    | Wake a sleeping or waiting case now.                                                               |
| `POST`   | `/cases/{id}/cancel`                  | Cancel. Returns the case detail.                                                                   |
| `POST`   | `/cases/{id}/instructions`            | Add an instruction `{ name, content }`; wakes the case (§7.5).                                     |
| `PUT`    | `/cases/{id}/instructions/{id}`       | Replace an instruction's name and content; wakes the case.                                         |
| `DELETE` | `/cases/{id}/instructions/{id}`       | Remove an instruction; wakes the case.                                                             |
| `POST`   | `/cases/{id}/files?name=`             | Add a file: raw bytes as the body, up to 20 MB. `201` with the file. Wakes the case.               |
| `GET`    | `/cases/{id}/files/{file_id}`         | A file's metadata and extracted text.                                                              |
| `GET`    | `/cases/{id}/files/{file_id}/content` | The bytes: images inline, everything else as a download (§17.3).                                   |

Create request (every field but `title` is optional):

```json
POST /api/v1/cases
{
  "title": "Electrician quote",
  "goal": "Get a quote from bob@sparky.ca for a 50A EV charger circuit.",
  "owner": "joe",
  "profile": "quotes",
  "llm": "default",
  "model": "openai/gpt-4.1-mini",
  "budgets": { "max_activations": 20, "max_turns_per_activation": 30, "max_total_tokens": 2000000 },
  "instructions": [{ "name": "tone.md", "content": "Be polite. Never offer more than $1,500." }],
  "human_channels": ["discord_joe"]
}
```

`profile`, `llm` and `budgets` default to the server's `default_profile`, `default_llm` and
`[budgets]`; `model` defaults to the LLM's default model; `human_channels` defaults to
`default_human_channels` (§10.4), and an unknown channel is a `400`. Response `201`: the case,
`{ "id": "01J9…", "state": "pending", "usage": {…}, … }`.

**A case without a goal is a draft.** It is created `waiting_for_human` with no question and
no activation queued, so nothing runs and no LLM is called. The owner's first message
(`POST /cases/{id}/messages`) starts it, as a `human_message` wake; files and instructions
added before that do not wake it. The system prompt then says the goal comes from the
owner's messages. This is what the web client's New case form does when only a title is
given; the goal, owner, profile, model, instructions, channels and budgets are under
"Advanced".

`cost` estimates what the case has cost so far: its input and output tokens times its
model's current prices from the model catalog (§8), as `{ usd, model, input_price, output_price, input_tokens, output_tokens }`. Cached-token discounts and price changes
during the case are not counted. When no price is known (an LLM whose provider lists no
prices, or a model it does not list), `usd` is `null` with a `reason`.

Planned: `PATCH /cases/{id}` (title, owner, budgets, channels), and `plugin_instances` on
cases.

### 14.2 Human requests (built)

| Method | Path                               | Description                                                                                                                                                             |
| ------ | ---------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET`  | `/human-requests?status=&case_id=` | Questions and approvals (`kind`, and for approvals `tool`, `args`, `decision`, `execution`); `status` defaults to `open`, `all` lists every status.                     |
| `POST` | `/human-requests/{id}/answer`      | A question: `{ "text" }`. An approval: `{ "decision": "approve" \| "reject", "comment"?, "args"? }`. `400` for the wrong kind of answer, `409` if it is no longer open. |

`args` replaces the tool's arguments when approving, e.g. a corrected email body; it is
only available from the web.

### 14.3 LLMs (built)

| Method | Path    | Description                                                                                                |
| ------ | ------- | ---------------------------------------------------------------------------------------------------------- |
| `GET`  | `/llms` | `default_llm`, and each configured LLM with its default model and the models on offer (prices when known). |

### 14.4 Prompts and reload (built)

| Method | Path                       | Description                                                                     |
| ------ | -------------------------- | ------------------------------------------------------------------------------- |
| `GET`  | `/prompts`                 | Effective templates and profiles: source (built-in or file), hash, load errors. |
| `GET`  | `/prompts/{name}`          | One effective template, with its content.                                       |
| `GET`  | `/prompts/profiles/{name}` | One profile, with its content.                                                  |
| `POST` | `/admin/reload`            | Reload the prompt templates and plugins (same as `SIGHUP`).                     |
| `GET`  | `/user-prompt`             | The owner's prompt (§7.6): `{ content, updated_at, max_chars }`.                |
| `PUT`  | `/user-prompt`             | `{ "content" }` replaces it (empty removes it); `400` over the limit.           |

Planned: reloading `clankjob.toml` itself.

### 14.5 Human channels (built)

| Method | Path        | Description                                                                       |
| ------ | ----------- | --------------------------------------------------------------------------------- |
| `GET`  | `/channels` | `{ "channels": [{ "name", "plugin", "default" }] }`: the loaded channels (§10.3). |

### 14.6 Plugins (built)

Plugins and their instances are defined by files (§9.4), so these endpoints are read-only
apart from testing and reloading.

| Method | Path                            | Description                                                                                      |
| ------ | ------------------------------- | ------------------------------------------------------------------------------------------------ |
| `GET`  | `/plugins`                      | Every plugin found (id, name, version, runtime, what it provides, load error) and its instances. |
| `POST` | `/plugins/reload`               | Load every plugin again (§9.4) and return the same as `GET /plugins`.                            |
| `POST` | `/plugin-instances/{name}/test` | Run the instance's check again; `404` if unknown, `409` if it is off or did not load.            |

Each plugin also lists the `tools` (name, description) and `guides` (name, description)
it offers (§9.9), and the response has `conflicts`: tools or guides left out because
their name was taken.

Each instance has `state` (`on`, `off`: `enabled = false`, `error`: invalid config), the
last check (`report`, `problems`, `warnings`, `findings`, `checked_at`, `error` if the
check could not run), `activity` from the channel thread (`last_ok_at`, `last_error`,
`last_warning`, with times), and `attention`: problems, a failed check, or an error newer
than the last success. The top-level `attention` is set when any plugin or instance needs
a look; the web client shows it in the navigation. Secrets are never included.

### 14.7 Health (built)

`GET /healthz`, without auth: `200` when the database answers and the scheduler ticked in
the last two minutes, `503` otherwise, with
`{ "database", "scheduler", "last_scheduler_tick" }`.

### 14.8 Errors (built)

```json
{ "error": { "code": "bad_request", "message": "`title` and `goal` must not be empty" } }
```

| Status | `code`              | When                                                                                |
| ------ | ------------------- | ----------------------------------------------------------------------------------- |
| `400`  | `bad_request`       | Invalid body or parameters, unknown LLM or profile, invalid instruction or file.    |
| `401`  | `unauthorized`      | Missing or wrong bearer token.                                                      |
| `404`  | `not_found`         | Unknown case, question, instruction, file or route.                                 |
| `409`  | `conflict`          | The case's state forbids it (e.g. messaging a cancelled case), or already answered. |
| `413`  | `payload_too_large` | Body over 2 MB, or a file over 20 MB.                                               |
| `500`  | `internal`          | Anything else; details are logged, not returned.                                    |

### 14.9 rouille notes

- Routing with `router!`. Each handler maps engine and storage errors to the format above.
- Handlers only touch the database, the file store and the work queue; they never call
  the LLM or a plugin. The model catalog is refreshed on its own thread (§8).
- The same process serves the web client (§15).

______________________________________________________________________

## 15. Web client (built)

A single-page app in `web/` (vanilla JavaScript, one stylesheet, system fonts, no
framework, no build step, no CDN), compiled into the server binary and served from `/`,
the same origin as the API. Everything the server sends is inserted as text, never HTML.

- **Sign in** with an API token, kept in the browser's local storage.
- **Status bar**: engine health (scheduler tick), counts (need you / working / sleeping),
  navigation, **New case**, the theme toggle (auto / light / dark) and sign out.
- **Cases**: two panes from 880 px up, one at a time below.
  - The **rail** lists cases grouped by state, "Needs you" first, finished groups folded,
    with a filter box. Selecting a case keeps the rail, its filter and its scroll.
  - The **detail** pane shows the state chip and details, the open question with an answer
    box, the result or failure reason, the goal, the **instructions** (write, upload, edit,
    remove), the **files** (add, preview text, view images), what the case is waiting
    for, budget use, notes, the **timeline** (answers from a chat channel say which), and a
    message box. The details line shows where the case asks ("web, discord_joe") and the
    estimated cost so far ("Cost ≈ $0.0027", the calculation in its tooltip; "unknown"
    when the model has no known price).
- **New case**: title, goal, owner, profile, LLM, model (with the catalog's suggestions,
  prices and context size), instructions, the chat channels to also ask on (when any are
  loaded, pre-ticked from the default), and budgets.
- **Inbox**: every open question and approval across cases, answerable in place. An
  approval card shows what the call will do and its arguments (to, subject, body…), with
  **Edit** (each argument becomes an input), an optional comment, **Reject** and
  **Approve** (or **Approve edited**). The same card appears on the case page, and the
  timeline shows the decision and the result of the call.
- **Plugins**: each plugin and instance with an On / Off / Error / Needs attention chip,
  what the last check found (problems and warnings with their fix, and every check in a
  collapsible list), the channel's last error or warning, "Test now" per instance, and
  "Reload plugins". Command plugins list their tools and guides, offered to every case.
  The navigation link shows a red mark when something needs attention.
- **Prompts**: **Your prompt**, the owner's prompt (§7.6), editable with a character count
  and Save; then the effective templates and profiles, their source and hash, rejected files,
  and "Reload from disk".

**Live updates**: the page polls every few seconds (the case every 2.5 s, the rail every
4 s, the counts every 10 s), fetching only events newer than the last one seen. rouille is
thread-per-request, so long-lived WebSockets are a poor fit; Server-Sent Events are a
possible later upgrade.

Planned: file downloads.

______________________________________________________________________

## 16. Data model

SQLite in WAL mode, with the schema embedded in the binary and applied at startup
(`PRAGMA user_version`). IDs are ULIDs stored as text; timestamps are UTC Unix
milliseconds.

**Built:**

```
cases
  id, title, goal, owner, profile, llm, model, state, budgets (json),
  usage (json: activations, input_tokens, output_tokens), result (json),
  outcome (the complete summary or the failure reason), created_at, updated_at,
  human_channels (json: channel instance names, §10.4)

events                       -- append-only; `seq` orders them and is the polling cursor
  seq, case_id, activation_id, kind, payload (json), created_at
  kind ∈ wake, llm_message, tool_result, nudge, state_changed, error

activations
  id, case_id, started_at, ended_at, end_state, usage (json),
  prompt_hashes (json: template name → SHA-256, §7.4)

work_queue                   -- at most one row per case: "this case should run"
  case_id, available_at, lease_until, rerun, attempts

wait_conditions              -- see §6.1
  id, case_id, kind, params (json), next_check_at, deadline_at, status, created_at,
  check_every_ms, cursor (json), failures

case_notes
  case_id, key, value, updated_at

human_requests               -- questions and approvals (§10.1, §9.7)
  id, case_id, kind (question/approval), question (or approval summary),
  status (open/answered/superseded/cancelled), answer (or approval comment),
  answered_via, responder, created_at, resolved_at,
  tool, args (json), decision (approve/reject), execution (pending/running/done)

instructions                 -- owner guidance, always in the prompt, editable (§7.5)
  id, case_id, name, content, created_at, updated_at

files                        -- bytes in data/files/<id> (§7.5)
  id, case_id, name, media_type, kind (text/pdf/image), size, sha256,
  text (extracted), pages, created_at

channel_deliveries           -- outbox for channel messages (§10.3)
  id, case_id, human_request_id (nullable), channel,
  kind (request/notification/resolution), payload (json),
  status (pending/sent/failed/skipped), external (json: what `deliver` returned),
  attempts, next_attempt_at, last_error, created_at, updated_at

channel_cursors              -- where each channel's poll continues
  channel, cursor (json), updated_at
```

**Planned:**

```
wait_conditions              + lease_until (for several check threads)
files                        + source (owner / third party)

case_plugins                 -- plugin instances are defined in files (§9.4) and
  case_id, instance_name     -- referenced everywhere by their unique name

plugin_kv                    -- host-provided storage for plugins
  instance_name, case_id (nullable), key, value (json)

outbox                       -- side effects of plugin tools (§17.1)
  id, case_id, instance_name, tool, args (json), idempotency_key,
  status (pending/sent/failed), result (json), attempts, created_at
```

______________________________________________________________________

## 17. Reliability & safety

### 17.1 Durability and idempotency

- **Leases (built)**: a worker claims a case by leasing its `work_queue` row (10 minutes,
  renewed before every LLM call). A second worker cannot claim it while the lease holds,
  which guarantees one activation per case. On startup every lease is cleared, since a
  fresh process owns no work.
- **Activation replay (built)**: every LLM response and tool result is committed before
  the next step. After a crash, the next activation rebuilds the context from events and
  runs the tool calls that have no result yet, without asking the LLM again.
- **One answer per question (built)**: an answer only updates a request that is still
  `open`, so two simultaneous answers cannot both win (§10.1).
- **Files (built)**: bytes are written and renamed before the database row, and removed
  again if the transaction fails, so a row never points at a missing file.
- **Approved calls run at most once (built)**: a call that needs approval (sending
  email) is moved to `running` in its own transaction before it runs, and to `done` with
  its result after; one found `running` after a crash is reported as having an unknown
  outcome, never run again (§9.7). Planned: reconciling such calls automatically (e.g.
  the email plugin looking for the Message-ID in the Sent folder), and an outbox for tools
  with side effects that do not need approval.
- **Channel messages go through an outbox (built)**: `channel_deliveries` rows are
  written in the same transaction as the question or state change, so a crash never loses
  a message. A message sent but not yet marked sent when the server dies is sent again
  after the restart (at-least-once). The poll cursor is saved after the answers it led
  to, and a reply seen twice can only lose the first-answer race, so each reply counts at
  most once.
- **Plugin-check leases (planned)**: wait-condition checks are claimed with `lease_until`
  so a slow check is never run twice.

### 17.2 Retries (built)

- **LLM retryable errors**: retried within the activation up to 3 times, with a backoff of
  2 s doubling each time. If it still fails, the case goes back to `pending` and is retried
  after `60 s × attempt`; after 5 attempts it fails.
- **LLM fatal errors**: the case fails with the provider's error.
- **Tool errors**: returned to the LLM as a tool error so it can adapt. They are not
  retried silently.
- **Channel messages (built)**: retried with backoff, 8 attempts over about an hour (§10.3).
- **Plugin processes (built)**: a process that times out, exits or writes invalid JSON is
  killed and started again on the next call (§9.8).
- **Plugin check errors (planned)**: backoff on the condition (§6.2).

### 17.3 Security

- **Untrusted content (built)**: tool results are data, not instructions, and the system
  prompt says so, and so does `read_email`'s result. Sending email always needs the
  owner's approval (§9.7), and `EMAIL_ALLOWED_RECIPIENTS` can limit where it can go even
  when approved (see `plugin/email/README.md`).
- **API (built)**: bearer tokens compared in constant time. Request bodies are limited to
  2 MB (20 MB for file uploads). TLS is terminated by a reverse proxy in front of rouille.
- **Web client (built)**: server data is inserted as text, never HTML. The page's
  Content-Security-Policy allows only this origin (`default-src 'none'`, scripts, styles,
  requests and images from `'self'`, plus `blob:` and `data:` images). No CDN or web fonts.
- **Serving files (built)**: only images are served inline; PDFs and text are downloads,
  never rendered by the browser as a page. Responses carry `nosniff`, a sandboxing CSP and
  a sanitized file name.
- **Server address and CORS (built)**: `public_url` (e.g. `https://clank.acme.com`) is where
  people reach the server; it is logged at startup and will be used for links in
  notifications. The web client is served from the same origin as the API, so it never
  needs CORS. Browser pages on `public_url` or `allowed_origins` get CORS headers
  (including preflight answers); every other origin gets none, and the browser blocks it.
- **Channel identity (built)**: an answer from a chat channel carries the owner's
  authority, so only replies from the instance's `allowed_responders` are accepted, checked
  by the plugin and again by the engine. Messages can only mention those users.
  Channel text is still treated as data when it's rendered into the LLM context.
- **Secrets (built)**: secrets are references in config files (`{ secret = … }`,
  `{ env = … }`), resolved at startup, for the server and for plugin instances alike. They
  are never written to the database, never logged, never returned by the API, never shown
  to the LLM, and only passed to a plugin process inside its own instance config. A
  `{ secret = … }` whose name looks like a secret value is refused without echoing it.
- **Plugins are trusted code (built)**: an external plugin runs with the server's user.
  It gets a minimal environment and only its own config, but nothing stops a malicious
  plugin from reading the data volume. Only install plugins you trust.
- **Audit (built)**: events and human requests form an audit trail of what each case did
  and why, including who answered each question and through which channel.

### 17.4 Observability

- **Built**: the Plugins page and `GET /plugins` (§14.6) show each plugin instance's
  state, last check, and recent errors and warnings, with a mark in the navigation when
  one needs attention.
- **Built**: logs through `tracing`, as text or JSON (`CLANKJOB_LOG_FORMAT=json`), with the
  level from `RUST_LOG` (default `info`). Activations log their case and activation ids;
  failures, retries, rejected prompts and model-catalog refreshes are logged.
- **Planned**: metrics for active and sleeping cases, checks per minute, check error
  rate, activations, tokens and cost per case and provider, and queue lag.

______________________________________________________________________

## 18. Deployment

Clankjob runs as **one process**: the `clankjob` binary serves the API and the web client
on one port. It reads its configuration from the path given as its first argument, else
`$CLANKJOB_CONFIG`, else `/config/clankjob.toml`. `clankjob.example.toml` in the
repository documents every option.

### 18.1 Container layout (built)

`Dockerfile` and `compose.yaml` are at the root of the repository. The compose file runs
the image from a checkout, with the same files as a local `cargo run`:

```
host (the checkout)                  container
  clankjob.toml                →    /config/clankjob.toml   read-only   server settings
  plugin/<id>/...              →    /plugins/<id>/...       read-only   plugins + their config (§9.3)
  prompts/... (optional)       →    /prompts/...            read-only   prompt overrides, profiles (§7.4)
  secrets/ (optional)          →    /run/secrets/           read-only   Docker secrets
  data/                        →    /data/                  read-write  clankjob.db, files/, user_prompt.md, cache/
  .env                         →    environment                         secrets for `{ env = … }`
```

`/data` is the only writable mount and holds all runtime state: the database, the bytes
of case files, the owner's prompt and the `uv` cache (`UV_CACHE_DIR=/data/cache/uv`, so
plugin dependencies survive container restarts). The image creates empty `/plugins` and
`/prompts`, so either can be left unmounted.

### 18.2 Server configuration (built)

```toml
# /config/clankjob.toml
listen = "0.0.0.0:8080"
public_url = "https://clank.acme.com"       # optional
allowed_origins = ["http://localhost:5173"] # optional, for UIs hosted elsewhere
data_dir = "/data"
prompts_dir = "/prompts"                    # optional
secrets_dir = "/run/secrets"                # the default
plugins_dir = "/plugins"                    # optional; one directory per plugin (§9.3)
default_human_channels = ["discord_joe"]    # optional; omitted = every loaded channel
workers = 4                                 # activation threads
shutdown_grace = "30s"
default_llm = "default"
default_profile = "general"                 # optional; must exist in prompts/profiles

[api]
tokens = [{ secret = "api_token" }]

[llm.default]
provider = "openai-compatible"
base_url = "https://openrouter.ai/api/v1"
api_key = { secret = "llm_api_key" }
model = "openai/gpt-4.1-mini"
models = ["openai/gpt-4.1", "openai/gpt-4.1-nano"]   # suggestions, optional
discover_models = true                               # the default
vision = false                                       # the default
timeout = "5m"

[budgets]
max_activations = 20
max_turns_per_activation = 30
max_total_tokens = 2000000
```

Unknown keys are rejected at startup, so a typo is an error, not a silently ignored
setting. Environment variables override the listen address and the directories, so one
file serves both a local run and the container, whose image sets them: `CLANKJOB_LISTEN`
(`0.0.0.0:8080`), `CLANKJOB_DATA_DIR` (`/data`), `CLANKJOB_PLUGINS_DIR` (`/plugins`),
`CLANKJOB_PROMPTS_DIR` (`/prompts`) and `CLANKJOB_SECRETS_DIR`, and
`CLANKJOB_REQUIRE_TOKEN=false` turns off the API token for testing (§14); the startup log lists the
ones that took effect. `public_url` and `allowed_origins` must be `http(s)://` URLs. Secrets are
`{ secret = "name" }` (a file in `secrets_dir`), `{ env = "NAME" }`, or a literal string
(accepted, but logged as a warning). Planned: `check_workers`, with plugin wait
conditions.

### 18.3 Image (built)

- **Multi-stage build.** `rust:1-slim-bookworm` compiles the release binary (the web client
  is embedded) with BuildKit cache mounts for the registry and target directory; the
  binary is copied into `debian:bookworm-slim`. `.dockerignore` lets only `Cargo.*`,
  `crates/` and `web/` into the build context: never data, configs or secrets.
- **Runtime.** `python3` for plugins, `uv` for plugins with dependencies, CA certificates,
  `tzdata`, and `tini` as PID 1, which reaps plugin processes and passes signals to the
  server.
- **Tools for case files.** `file`, ExifTool, Poppler (`pdfinfo`, `pdftotext`,
  `pdftoppm`), `qpdf`, Tesseract with English and French, ImageMagick, `pandoc`, FFmpeg
  (`ffprobe`), `jq` and `unzip`, in their own layer. The `documents` plugin turns them into
  tools (metadata, OCR, text extraction, media details; `plugin/documents/README.md`).
- **User.** A non-root `clankjob` user (UID/GID 1000); `compose.yaml` runs as
  `${UID:-1000}:${GID:-1000}`, which must own the host's `data/`.
- **Health.** `HEALTHCHECK` calls `/healthz` with Python (no curl in the image).
- One port is exposed (8080), published on localhost by the compose file: TLS is
  terminated by a reverse proxy (Caddy, Traefik, nginx) in front. Every integration is
  outbound (IMAP, SMTP, Discord REST, the LLM API), so no other port is needed.

### 18.4 Operational rules

- **Exactly one replica.** The design is single-node: one SQLite writer and one scheduler.
  Never run two processes against the same `data_dir`.
- **Local volume only.** The data directory must be a local disk or named volume, not
  NFS/SMB. SQLite's WAL mode relies on file locking that network filesystems don't
  provide reliably.
- **Graceful shutdown (built).** On `SIGTERM` or `SIGINT` the server stops accepting
  requests, workers stop after their current step, and the process exits once they have,
  or after `shutdown_grace`. Anything cut off resumes after the next start. In Docker, set
  `stop_grace_period` above `shutdown_grace`, since the default of 10 s is short for an
  LLM call.
- **Fast recovery after restart (built).** Leases are cleared at startup, so interrupted
  cases resume at once.
- **Migrations (built)** run at startup, before any thread starts.
- **Health check (built).** `GET /healthz` (§14.7), for Docker's `HEALTHCHECK`.
- **Backups.** Don't copy the live database file. Use `VACUUM INTO` on a timer, or a
  Litestream sidecar that streams the database to S3-compatible storage. Back up
  `data/files/` alongside it.
- **Reload (built).** `SIGHUP` or `POST /admin/reload` reloads the prompt templates and
  the plugins; plugins also reload by themselves when their files change (§9.4). A new
  or changed secret in the environment still needs a restart. Planned: reloading
  `clankjob.toml` without a restart.

### 18.5 Published image and CI (built)

- **CI** (`.github/workflows/ci.yml`): every push to `main` and every pull request runs
  `cargo fmt --check`, clippy with `-D warnings` and the Rust tests, and for each Python
  plugin its tests, ruff and basedpyright.
- **Image** (`.github/workflows/docker.yml`): pushes to `main` and `v*` tags build the image
  natively on amd64 and arm64 runners, push each by digest, and join them into one
  multi-platform image at `ghcr.io/uintptr/clankjob`: `latest` and `sha-<commit>` from
  `main`, `1.2.3` and `1.2` from a `v1.2.3` tag. Layers are cached per platform in the
  GitHub Actions cache.
- **Bundled plugins.** The image carries the repository's `plugin/` in `/plugins`, without
  any `config.toml` (excluded by `.dockerignore`). Settings are mounted per plugin
  (`/plugins/<id>/config.toml`), or a whole directory replaces `/plugins`. Plugins that
  cannot work unconfigured set `requires_config` and stay unloaded until then.
- **Deploying without the source** (`deploy/compose.yaml`, `deploy/README.md`): download
  the compose file and `clankjob.example.toml`, write `.env`, `mkdir data`, and
  `docker compose up -d`; `docker compose pull` updates.

### 18.6 Running it from a checkout (built)

```sh
docker compose up -d --build     # build and start
docker compose logs -f           # follow the log
docker compose exec clankjob /plugins/documents/check_config.py   # any plugin's check
docker compose down              # stop: waits for running steps (stop_grace_period 60s)
```

Secrets live in `.env` next to `compose.yaml` (git-ignored), for the `{ env = … }`
references in `clankjob.toml` and the plugins' `config.toml`. Docker secrets work too:
mount `./secrets` on `/run/secrets` and use `{ secret = … }`. Stop any local
`cargo run` first: both would use the same `data/` (§18.4).

______________________________________________________________________

## 19. Open questions & future work

- **Per-case plugin tools**: opt a case out of some tools, or limit a case to some.
- **Tool guides by skill format**: load `SKILL.md`-style directories (frontmatter with a
  name and description) as guides without a manifest entry.
- **Next milestones**: email attachments both ways (case files attached to outgoing mail,
  incoming attachments imported; see `plugin/email/README.md`), protocol plugins with tools and conditions
  (JSON-RPC `call_tool` and `check`, milestone 4, §9), and the Docker image (§18).
- **Channel attachments**: download files sent in a Discord reply into the case (§7.5)
  before the links expire.
- **Compaction** of long transcripts (§7.3).
- **Cost budgets in dollars**, using the prices the model catalog already collects (§8).
- **Scheduler precision**: sleep until the next due condition instead of a fixed 15 s tick.
- **Plugin sandboxing**: run external plugins in their own container or as WASM
  (wasmtime), so an untrusted plugin can't read the data volume.
- **Python plugin SDK**: a helper module implementing the JSON-RPC loop of §9.8.
- **Editing plugin config from the web client**, writing back to the files.
- **Finer plugin reload**: reload only the plugin whose files changed, and keep its old
  instances running when its new config is invalid.
- **Plugin trouble notifications**: tell the owner (e.g. by email, or another channel)
  when a channel starts failing, instead of relying on the Plugins page.
- **Retry empty Discord replies**: stop the poll cursor before a reply whose text arrived
  empty (Message Content intent off), so turning the intent on later picks it up.
- **Push wake-ups**: IMAP IDLE, provider webhooks, or Discord Interactions (buttons), via
  a new optional `Plugin::subscribe()` that writes to a durable inbox table and can fire
  conditions or resolve human requests immediately. Polling stays the fallback.
- **More human channels**: Slack, SMS, or email itself (reply to a notification email),
  all built on the same `HumanChannel` trait.
- **Cases created by events** (inbound email → new case).
- **Case templates**: reusable goals, instructions, plugin sets, budgets and a profile.
- **Prompt evaluation**: replay recorded cases against an edited prompt to compare
  behaviour before deploying it.
- **Sub-cases**: a case spawning and waiting on child cases (a `core.case_completed`
  wait condition).
- **Multi-user**: users, roles, per-user plugin instances, and mapping each user to their
  channel identities (Discord id, phone number, …) instead of a per-instance
  `allowed_responders` list.
- **Scaling out**: move to Postgres (`SELECT … FOR UPDATE SKIP LOCKED` for queue claims)
  and run several worker processes.
- **Open question:** should the LLM choose `check_every` freely within plugin bounds,
  or should instances enforce a fixed schedule to keep IMAP load predictable?
- **Open question:** how much of the transcript should a human be able to edit or
  redact (e.g. to remove sensitive content before compaction)?
