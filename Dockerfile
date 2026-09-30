# clankjob server image (design §18.3). Published as ghcr.io/uintptr/clankjob by
# .github/workflows/docker.yml; deploy/compose.yaml runs it.
#
#   docker build -t clankjob .
#
# The web UI is compiled into the binary, and the plugins of this repository are bundled
# in /usr/share/clankjob/plugins without any settings, with the templates the setup
# wizard writes (`setup`, `update`: deploy/clankjob_setup.py). Settings, secrets, prompts, the
# owner's own plugins and data are mounted: /config (clankjob.toml, plugins/<id>.toml),
# /prompts and /plugins (read-only), /data.

# ---- build ---------------------------------------------------------------------------
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
# The commit shown in the web UI and /healthz; the build context has no .git.
ARG CLANKJOB_COMMIT=""
ENV CLANKJOB_COMMIT=${CLANKJOB_COMMIT}
# Cache mounts keep the registry and build outputs between builds; the binary is copied
# out because the target directory is not part of the image layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p clankjob-server \
    && cp target/release/clankjob /usr/local/bin/clankjob

# ---- runtime -------------------------------------------------------------------------
FROM debian:bookworm-slim

LABEL org.opencontainers.image.source="https://github.com/uintptr/clankjob" \
      org.opencontainers.image.description="clankjob: AI agents that know how to wait"

# python3: the plugins (Discord, email, documents, weather, ntfy and Home Assistant use the
# standard library only).
# uv: plugins whose scripts need dependencies (youtube_transcribe, finance).
# tini: PID 1 that reaps plugin processes and forwards signals to the server.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates python3 tini tzdata \
    && rm -rf /var/lib/apt/lists/*

# Programs for the tools that work on case files (plugin/documents): metadata of
# images, PDFs and office files; PDF inspection, text and rendering; OCR in English and
# French; conversion of Word, ODT, RTF, HTML…; audio and video details. Their own layer,
# so changing it does not rebuild the one above.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
    file \
    libimage-exiftool-perl \
    poppler-utils \
    qpdf \
    tesseract-ocr tesseract-ocr-eng tesseract-ocr-fra \
    imagemagick \
    pandoc \
    ffmpeg \
    jq unzip \
    && rm -rf /var/lib/apt/lists/*

# The agent's shell runs in the sandbox container, from this same image (plugin/shell,
# sandbox/sandboxd.py): everything above, plus what a shell user expects.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
    bash-completion \
    curl wget \
    git \
    sqlite3 \
    ripgrep \
    procps less \
    xz-utils bzip2 zip \
    && rm -rf /var/lib/apt/lists/*

# Network tools for the sandbox. The ones that need raw sockets get CAP_NET_RAW as a file
# capability, so they work for the unprivileged user (NMAP_PRIVILEGED=1 in compose lets
# nmap use it). Only NET_RAW: Docker's default set lacks NET_ADMIN, and a binary asking
# for a capability outside that set cannot run at all.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
    iputils-ping traceroute mtr-tiny \
    dnsutils whois \
    netcat-openbsd ncat socat telnet \
    nmap arp-scan tcpdump \
    iproute2 iperf3 \
    openssl openssh-client \
    libcap2-bin \
    && for program in nmap tcpdump arp-scan mtr-packet; do setcap cap_net_raw+eip "$(command -v "$program")"; done \
    && rm -rf /var/lib/apt/lists/*
COPY sandbox/sandboxd.py /usr/local/lib/clankjob/sandboxd.py
# Optional services next to the server, each started by its own compose service.
COPY intake/discord/discord_intake.py /usr/local/lib/clankjob/discord_intake.py
COPY --from=ghcr.io/astral-sh/uv:latest /uv /usr/local/bin/uv
# hacli: the Home Assistant plugin's client (plugin/home_assistant), a static musl binary
# released for amd64 and arm64, the same names as Docker's TARGETARCH.
ARG TARGETARCH
ADD --chmod=755 https://github.com/uintptr/hacli/releases/download/v0.0.2/hacli-linux-${TARGETARCH} /usr/local/bin/hacli
COPY --from=build /usr/local/bin/clankjob /usr/local/bin/clankjob

# A fixed, non-root user; match it to the owner of the host's data directory with
# `user:` in compose.yaml (default 1000:1000).
RUN groupadd --gid 1000 clankjob \
    && useradd --uid 1000 --gid 1000 --home-dir /home/clankjob --create-home --shell /usr/sbin/nologin clankjob \
    && mkdir -p /config/plugins /plugins /prompts /data /work /state \
    && chown clankjob:clankjob /data /work /state

# The setup wizard and what it writes into a setup directory.
COPY deploy/clankjob_setup.py /usr/local/lib/clankjob/clankjob_setup.py
COPY --chmod=755 deploy/entrypoint.sh /usr/local/bin/clankjob-entrypoint
COPY deploy/compose.yaml clankjob.example.toml /usr/share/clankjob/
COPY --chmod=755 deploy/update /usr/share/clankjob/update

# Container paths override the ones in clankjob.toml, so the same file works for a local
# `cargo run` and here (design §18.2).
# The bundled plugins, last because they change most often. .dockerignore leaves out
# their config.toml files: settings live in /config/plugins/<id>.toml.
COPY plugin/ /usr/share/clankjob/plugins/

ENV CLANKJOB_CONFIG=/config/clankjob.toml \
    CLANKJOB_LISTEN=0.0.0.0:8080 \
    CLANKJOB_DATA_DIR=/data \
    CLANKJOB_BUNDLED_PLUGINS_DIR=/usr/share/clankjob/plugins \
    CLANKJOB_PLUGINS_DIR=/plugins \
    CLANKJOB_PLUGIN_CONFIG_DIR=/config/plugins \
    CLANKJOB_PROMPTS_DIR=/prompts \
    UV_CACHE_DIR=/data/cache/uv \
    PYTHONDONTWRITEBYTECODE=1

USER clankjob
WORKDIR /data
EXPOSE 8080
VOLUME ["/data"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD ["python3", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/healthz', timeout=4)"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/clankjob-entrypoint"]
