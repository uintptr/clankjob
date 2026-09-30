# clankjob server image (design §18.3). Published as ghcr.io/uintptr/clankjob by
# .github/workflows/docker.yml; deploy/compose.yaml runs it.
#
#   docker build -t clankjob .
#
# The web UI is compiled into the binary, and the plugins of this repository are bundled
# in /plugins without any configuration. Settings, secrets, prompts and data are mounted:
# /config/clankjob.toml, /plugins/<id>/config.toml, /prompts (read-only), /data.

# ---- build ---------------------------------------------------------------------------
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
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

# python3: the plugins (Discord, email, documents and weather use the standard library only).
# uv: plugins whose scripts need dependencies (youtube_transcribe).
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
COPY --from=build /usr/local/bin/clankjob /usr/local/bin/clankjob

# A fixed, non-root user; match it to the owner of the host's data directory with
# `user:` in compose.yaml (default 1000:1000).
RUN groupadd --gid 1000 clankjob \
    && useradd --uid 1000 --gid 1000 --home-dir /home/clankjob --create-home --shell /usr/sbin/nologin clankjob \
    && mkdir -p /config /plugins /prompts /data /work /state \
    && chown clankjob:clankjob /data /work /state

# Container paths override the ones in clankjob.toml, so the same file works for a local
# `cargo run` and here (design §18.2).
# The bundled plugins, last because they change most often. .dockerignore leaves out
# their config.toml files: settings are mounted per plugin, or /plugins as a whole.
COPY plugin/ /plugins/

ENV CLANKJOB_CONFIG=/config/clankjob.toml \
    CLANKJOB_LISTEN=0.0.0.0:8080 \
    CLANKJOB_DATA_DIR=/data \
    CLANKJOB_PLUGINS_DIR=/plugins \
    CLANKJOB_PROMPTS_DIR=/prompts \
    UV_CACHE_DIR=/data/cache/uv \
    PYTHONDONTWRITEBYTECODE=1

USER clankjob
WORKDIR /data
EXPOSE 8080
VOLUME ["/data"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD ["python3", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/healthz', timeout=4)"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/clankjob"]
