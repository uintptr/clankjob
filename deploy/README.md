# Deploying clankjob with Docker Compose

The image `ghcr.io/uintptr/clankjob` is built and published by GitHub Actions for amd64
and arm64 (`.github/workflows/docker.yml`): `latest` follows `main`, and `v1.2.3` tags
publish `1.2.3` and `1.2`. It contains the server, the web UI, the plugins of this
repository (without settings), Python and `uv` for plugins, and the programs behind the
document tools (ExifTool, Poppler, Tesseract OCR, pandoc, FFmpeg). On the server you only
need Docker with Compose; nothing is built there.

## Quick setup with deploy.py

`deploy.py` (Python 3.11+, standard library only) does the whole setup: it downloads the
plugins and templates, asks for your settings and secrets, and writes everything Docker
Compose needs.

```sh
curl -fsSLO https://raw.githubusercontent.com/uintptr/clankjob/main/deploy/deploy.py
python3 deploy.py ~/clankjob
```

It asks for your public URL, LLM endpoint, model and API key, generates the token you sign
in with, and offers to configure each plugin (email, Discord, …), walking through its
example settings. Secrets are typed hidden and only written to `.env` (mode 600). It
creates:

```
~/clankjob/
  compose.yaml      the published image, with ./plugins and ./prompts mounted
  clankjob.toml     server settings
  .env              secrets
  plugins/<id>/     every plugin; config.toml for the ones you configured
  prompts/          your prompt overrides and profiles
  data/             database and case files
```

Run it again at any time to update the plugins and `compose.yaml`, or configure a plugin
you skipped (`--configure email`); it never overwrites a setting or secret you already
have. `compose.yaml` is regenerated from the current template with your port and image
tag; if that changes it, the previous file is saved as `compose.yaml.bak` and the changed
lines are shown (edits of your own must be carried over by hand, or use
`--keep-compose`). Run
from inside a setup, it updates that setup (`cd ~/clankjob && python3 deploy.py`); from
anywhere else, give the directory, or it creates a new one in `./clankjob`. Other
options: `--ref v1.2.3` (plugins of a release), `--image-tag 1.2`, `--port 8081`,
`--source ~/src/clankjob` (a local checkout), `--yes` (no questions).

At the end it offers to pull the image and start the containers (`--start` does it
without asking): `docker compose pull`, `docker compose up -d`, and, on an existing setup
whose plugins changed, `docker compose restart clankjob`, since the server reads its
plugins at startup and `up -d` only recreates a container for a new image or setting. By
hand:

```sh
cd ~/clankjob && docker compose pull && docker compose up -d
```

The rest of this page does the same by hand.

## First start

```sh
mkdir clankjob && cd clankjob
curl -fsSLO https://raw.githubusercontent.com/uintptr/clankjob/main/deploy/compose.yaml
curl -fsSL -o clankjob.toml https://raw.githubusercontent.com/uintptr/clankjob/main/clankjob.example.toml
mkdir data                        # before the first start, so it belongs to your user
```

1. **Settings.** Edit `clankjob.toml`: your LLM and `public_url`. Leave `listen` and the
   directories as they are: the image sets its own (`0.0.0.0:8080`, `/data`, `/plugins`,
   `/prompts`).

2. **Secrets.** Put the values of the `{ env = … }` references in `.env`, next to
   `compose.yaml`, and keep it private (`chmod 600 .env`):

   ```sh
   CLANKJOB_TOKEN=a-long-random-token
   OPENROUTER_API_KEY=sk-or-...
   ```

3. **Start.**

   ```sh
   docker compose up -d
   docker compose ps               # "healthy" after a few seconds
   ```

   The web UI is on `http://127.0.0.1:8080`; sign in with `CLANKJOB_TOKEN`.

4. **Public access.** Put a reverse proxy with TLS in front, e.g. Caddy:

   ```
   clank.example.com {
       reverse_proxy 127.0.0.1:8080
   }
   ```

   and set `public_url = "https://clank.example.com"` in `clankjob.toml`. No other port
   is needed: every integration (LLM, email, Discord, YouTube) is an outbound connection.

## Plugins

All plugins of the repository are in the image. The document tools and YouTube
transcripts work as they are; email and Discord stay inactive until you give them a
`config.toml`:

```sh
mkdir -p plugins
curl -fsSL -o plugins/email.toml https://raw.githubusercontent.com/uintptr/clankjob/main/plugin/email/config.example.toml
# edit it, add its secret (EMAIL_PASSWORD=…) to .env, then enable its line in compose.yaml:
#   - ./plugins/email.toml:/plugins/email/config.toml:ro
docker compose up -d
docker compose exec clankjob /plugins/email/check_config.py
```

Every plugin has a `check_config.py` that checks it against the real services; the
web UI's **Plugins** page shows the same status. To use your own plugins instead of the
bundled ones, mount a whole directory on `/plugins` (`- ./plugins:/plugins:ro`, one
sub-directory per plugin, like `plugin/` in the repository).

## Updating

From inside the setup, with the latest `deploy.py` (it updates the plugins and
`compose.yaml` too):

```sh
python3 deploy.py --yes --start
```

Or only the image: `docker compose pull && docker compose up -d`.

Database migrations run at startup. To stay on a version, replace `latest` in
`compose.yaml` with a version tag such as `1.2`.

## Good to know

- **Data.** `data/` holds everything that changes: the database, case files, your prompt
  (`user_prompt.md`) and plugin caches. Back it up while the server is stopped, or with
  `VACUUM INTO` (design §18.4). Never run two servers on the same `data/`.
- **Another user.** The container runs as `1000:1000` by default. If `data/` belongs to
  another user, start with `UID=$(id -u) GID=$(id -g) docker compose up -d`.
- **Docker secrets.** Instead of `.env`, mount a directory on `/run/secrets` and use
  `{ secret = "name" }` references.
- **Stopping** waits up to 60 seconds for running steps; anything cut off resumes at the
  next start.
