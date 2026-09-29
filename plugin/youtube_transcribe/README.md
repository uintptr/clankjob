# YouTube transcripts plugin

Lets cases read YouTube videos. Ask a case anything about a video ("summarise this
talk", "what did the CFO say about capex?") and the agent fetches the captions,
reads them, and answers with `[MM:SS]` citations.

It is a **command plugin** (`runtime = "command"`, design §9.9): `plugin.toml` turns
`scripts/yt.py` into tools, with no plugin protocol to implement.

| Tool                        | What it does                                                         |
| --------------------------- | -------------------------------------------------------------------- |
| `youtube_transcript`        | Downloads the transcript; saved as a case file read with `read_file` |
| `youtube_caption_languages` | Lists the caption tracks                                             |
| `youtube_video_info`        | Title and channel                                                    |

| Guide                    | Read by the agent when…                                     |
| ------------------------ | ----------------------------------------------------------- |
| `youtube-questions`      | answering questions about, or summarising, a video          |
| `earnings-call-analysis` | the owner wants an earnings call analysed (Bezos & Buffett) |

## Setup

- Needs [`uv`](https://docs.astral.sh/uv/) on the server's `PATH`; the first call
  installs the script's dependencies (`youtube-transcript-api`, `httpx`, `tabulate`).
- No API key. YouTube blocks many cloud and VPN addresses; if transcripts fail with
  `RequestBlocked`, copy `config.example.toml` to `config.toml` and pass a proxy through
  `YT_PROXY_URL`.

The server loads it from `plugins_dir` and reloads it when these files change. The
Plugins page lists its tools and guides.

## Standalone use

`scripts/yt.py` works on its own:

```sh
scripts/yt.py transcript https://www.youtube.com/watch?v=dQw4w9WgXcQ --format stamped
scripts/yt.py langs dQw4w9WgXcQ
```

`command.md` is the original Claude Code `/transcript` command this plugin came from.
