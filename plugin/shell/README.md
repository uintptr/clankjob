# Shell plugin (sandbox)

Gives the agent a shell: `run_command` runs a bash command in the **sandbox**, a container
from the clankjob image with every program installed (curl, ping, dig, nc, nmap, tcpdump, mtr, git, jq, Python and `uv`,
pandoc, Poppler, Tesseract, ImageMagick, FFmpeg, ExifTool, SQLite, ripgrep…) and internet
access, but none of the server's secrets, data or settings. The agent can fetch pages,
call APIs, clone repositories, convert files and write scripts, without being able to
read an API key, change its own database or get around approvals.

| Tool          | What it does                                                                                                                            |
| ------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `run_command` | `bash -c` in `/work`; optionally copies a case file to `/work/case-files/` and a skill's files to `/work/skills/<name>/` first; 1–600 s |

## Setup

`compose.yaml` (and `deploy/compose.yaml`) run the `sandbox` service next to the server.
Turn the plugin on by copying `config.example.toml` to `config.toml` in a checkout, or to
`config/plugins/shell.toml` in a deployment (the setup's `--configure shell` does it)
(`SANDBOX_URL = "http://sandbox:8000"`), then check it from the server's container:

```sh
docker compose up -d
docker compose exec clankjob /usr/share/clankjob/plugins/shell/check_config.py
```

```
  ok   sandbox at http://sandbox:8000: user clankjob, work directory /work
  ok   runs a command and writes to its work directory
  ok   programs: bash, curl, wget, git, jq, python3, uv, sqlite3, pandoc, pdftotext, tesseract, convert, ffmpeg, exiftool
  ok   sees nothing of the server (secrets, /data, its processes)
  ok   reaches the internet (deb.debian.org: HTTP 200)
```

The isolation check fails if the sandbox can see a secret-looking environment variable,
anything in `/data`, `/config` or `/run/secrets`, or the server's process.

Tests use a fake sandbox; `SHELL_LIVE_TEST=1` also runs the checks above:

```sh
python3 -m unittest -v test_shell_tool.py
```

## Design

Why a separate container: a shell in the server's own container runs as the server's
user, so it could read every secret (`/proc/<pid>/environ`, `/run/secrets`, token files),
write the SQLite database (approving its own requests), and send email with the
password it found. The sandbox runs the same image with a different entrypoint and, in
compose, no `env_file`, no `/data` or secrets volumes and its own process namespace.

- **Exec service.** `sandbox/sandboxd.py` (standard library) listens on port 8000 of the
  compose network, with no published port and no authentication: whoever can reach it
  could already run commands there. `POST /run` takes the command, a timeout and files
  (base64), and answers with the exit code, stdout and stderr (1 MB each at most).
- **Commands** run with `bash -c` in `/work` as the image's unprivileged user, in their
  own session: a timeout, or the end of the command, kills everything it started.
- **Files.** A `file` argument is sent along (40 MB at most) and written to
  `/work/case-files/<name>`. Nothing comes back but the output; long output becomes a
  case file (`output = "auto"`).
- **Skills.** `skill` names a skill (design §9.10); its approved files are sent along
  and replace `/work/skills/<name>/` before the command runs, so a copy a case changed
  never outlives the next run.
- **`/work`** is the `sandbox-work` volume: it persists across calls, restarts and cases,
  and every case shares it. `docker compose down -v` or removing the volume resets it.
- `shell_tool.py` forwards the call; a failing command is a result (with its exit code),
  not a tool error, while an unreachable sandbox is.

Planned: a work directory per case, getting files back from the sandbox as case files,
and resource limits (CPU, memory, disk) in compose.
