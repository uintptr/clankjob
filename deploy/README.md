# Deploying clankjob with Docker Compose

The image `ghcr.io/uintptr/clankjob` is built and published by GitHub Actions for amd64
and arm64 (`.github/workflows/docker.yml`) for each release: a `v1.2.3` tag publishes
`1.2.3`, `1.2` and `latest`, so `latest` is always the newest release. It contains the server, the web UI, every plugin of
this repository, Python and `uv` for plugins, the programs behind the document tools
(ExifTool, Poppler, Tesseract OCR, pandoc, FFmpeg), and the setup wizard. On the server
you only need Docker with Compose: nothing is built or installed there.

**The image is the version.** Your directory holds only your settings, secrets and data;
the plugins, the templates and the wizard come with the image. Updating the image updates
everything else, so nothing on the host drifts from it.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/uintptr/clankjob/main/deploy/install.sh | sh -s -- ~/clankjob
```

Or download it and read it first: `curl -fsSLO …/install.sh && sh install.sh ~/clankjob`.
The directory defaults to `./clankjob`; `.` is the current one. It pulls the image and runs its `setup` on the directory. That asks for your public URL,
LLM endpoint, model and API key, generates the token you sign in with, and offers to
configure each plugin (email, Discord, …), walking through its example settings. Secrets
are typed hidden and only written to `.env` (mode 600). Then it offers to start the
containers. It creates:

```
~/clankjob/
  compose.yaml              the image's own, replaced by every update: never edit it
  compose.override.yaml     your changes to it, if you need any (you create it)
  update                    updates the setup, see below
  .env                      CLANKJOB_TAG, CLANKJOB_PORT, UID, GID and the secrets
  config/clankjob.toml      server settings
  config/plugins/<id>.toml  settings of each plugin you configured
  plugins/                  your own plugins, if any (they replace bundled ones by id)
  prompts/                  your prompt overrides and profiles
  data/                     database, case files, database backups
```

Options after the directory go to `setup`: `--configure email` (configure a plugin,
repeatable), `--port 8081`, `--yes` (no questions); piped, they follow the directory
(`| sh -s -- ~/clankjob --port 8081`). `CLANKJOB_TAG=1.2` before `sh` installs another tag. The web UI is then on `http://127.0.0.1:8080` (or your port); sign
in with `CLANKJOB_TOKEN` from `.env`.

To configure a plugin later, run `setup` again from the directory; it asks only for what
is missing and never overwrites a setting or secret you have:

```sh
cd ~/clankjob
docker compose run --rm --no-deps -v "$PWD:/setup" clankjob setup --configure email
```

Or write `config/plugins/<id>.toml` yourself from the plugin's `config.example.toml`. The
server picks it up within seconds. Every plugin has a check against the real services;
the web UI's **Plugins** page shows the same status:

```sh
docker compose exec clankjob /usr/share/clankjob/plugins/email/check_config.py
```

## Update

```sh
cd ~/clankjob && ./update
```

It pulls the image named in `.env`, lets the new image refresh `compose.yaml` (and
convert anything old, see below), then restarts the containers on it. Before the new
version migrates the database, the server copies it to
`data/backups/before-schema-<n>-<time>.db` (the newest 3 are kept).

- **Stay on a version:** set `CLANKJOB_TAG=1.2` (or `1.2.3`) in `.env`, or run
  `CLANKJOB_TAG=1.2 ./update` once (it saves the tag).
- **Go back a version:** `CLANKJOB_TAG=<previous> ./update`. If the newer version had
  migrated the database, the older one refuses it: stop the containers
  (`docker compose down`), copy the latest `data/backups/before-schema-*.db` over
  `data/clankjob.db` (and delete `data/clankjob.db-wal` and `data/clankjob.db-shm`), then
  `docker compose up -d`.

## Changing compose.yaml

Never edit `compose.yaml`: updates replace it. Docker Compose merges
`compose.override.yaml` from the same directory by itself, so put changes there, e.g.
Docker secrets and a different DNS server:

```yaml
services:
  clankjob:
    dns: !reset [192.168.1.1]
    volumes:
      - ./secrets:/run/secrets:ro
```

## Setups from before

A directory set up by the former `deploy.py` (copies of the plugins in `plugins/`, their
settings inside) or by hand (`clankjob.toml` next to `compose.yaml`, `plugins/<id>.toml`
mounted per plugin) keeps working with a new image. To bring it to this layout, run the
installer on it once, from inside it (stop it first with `docker compose down`):
`curl -fsSL …/install.sh | sh -s -- .`. It moves the settings to `config/`, the
plugin copies to `plugins.old/` (delete it once all works), and the old compose file's
port and image tag to `.env`, keeping the file as `compose.yaml.old`: carry any changes
of your own over to `compose.override.yaml`. Then `./update` as above.

## Public access

Put a reverse proxy with TLS in front, e.g. Caddy:

```
clank.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

and set `public_url = "https://clank.example.com"` in `config/clankjob.toml`. No other
port is needed: every integration (LLM, email, Discord, YouTube) is an outbound
connection.

## Good to know

- **Data.** `data/` holds everything that changes: the database, case files, your prompt
  (`user_prompt.md`), plugin caches and database backups. Back it up while the server is
  stopped, or with `VACUUM INTO` (design §18.4). Never run two servers on the same
  `data/`.
- **Ownership.** The containers run as `UID:GID` from `.env`, the user who ran the setup,
  so they can write `data/`.
- **Docker secrets.** Instead of `.env`, mount a directory on `/run/secrets` (in
  `compose.override.yaml`, above) and use `{ secret = "name" }` references.
- **Your own plugins.** A directory in `plugins/` with a `plugin.toml` is loaded after the
  bundled ones, and replaces a bundled plugin with the same id.
- **Stopping** waits up to 60 seconds for running steps; anything cut off resumes at the
  next start.
- **Without the installer.** Run the image's setup yourself:
  `docker run --rm -it --user "$(id -u):$(id -g)" -v "$PWD:/setup" ghcr.io/uintptr/clankjob setup`.
