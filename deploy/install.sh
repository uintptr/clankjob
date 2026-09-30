#!/bin/sh
# Install clankjob, or bring an existing setup (even one made by the former deploy.py) to
# the current layout. Needs only Docker with Compose; the setup runs inside the image.
#
#   curl -fsSL https://raw.githubusercontent.com/uintptr/clankjob/main/deploy/install.sh | sh -s -- [DIRECTORY] [options]
#
# or downloaded first:
#
#   curl -fsSLO https://raw.githubusercontent.com/uintptr/clankjob/main/deploy/install.sh
#   sh install.sh [DIRECTORY] [setup options, e.g. --configure email --port 8081]
#
# DIRECTORY defaults to ./clankjob (`.` for the current one). CLANKJOB_TAG picks the image
# tag (default: the one in the directory's .env, else latest).
# Afterwards, `./update` in the directory updates it.
set -eu

fail() {
    echo "failed: $*" >&2
    exit 1
}

# Everything runs from this function, called on the last line: piped into a shell, the
# script is read from stdin as it runs, so nothing may start before all of it is read.
main() {
    command -v docker >/dev/null 2>&1 || fail "Docker is not installed: https://docs.docker.com/engine/install/"
    docker compose version >/dev/null 2>&1 || fail "Docker Compose v2 is missing (docker compose version)"

    dir="clankjob"
    case "${1:-}" in
        "" | -*) ;;
        *) dir="$1"; shift ;;
    esac

    mkdir -p "$dir"
    cd "$dir"
    if [ -n "${CLANKJOB_TAG:-}" ]; then
        tag="$CLANKJOB_TAG"
    elif [ -f .env ] && grep -q '^CLANKJOB_TAG=' .env; then
        tag="$(sed -n 's/^CLANKJOB_TAG=//p' .env | tail -n 1)"
    else
        tag="latest"
    fi
    image="ghcr.io/uintptr/clankjob:$tag"
    echo "Setting up $(pwd) with $image"
    # A tag only built locally (e.g. to try a change) cannot be pulled; use it as it is.
    docker pull "$image" || docker image inspect "$image" >/dev/null 2>&1 || fail "cannot pull $image"

    # Questions go to the terminal, even when stdin is this script (piped) or not a
    # terminal at all; without one, the setup takes its defaults and nothing is started.
    if [ -t 0 ]; then
        terminal="/dev/stdin"
    elif (: </dev/tty) 2>/dev/null; then
        terminal="/dev/tty"
    else
        terminal=""
    fi
    # As the invoking user, so everything it writes belongs to them.
    if [ -n "$terminal" ]; then
        docker run --rm -it --user "$(id -u):$(id -g)" -v "$(pwd):/setup" "$image" setup --tag "$tag" "$@" <"$terminal"
        printf 'Start it now (docker compose up -d)? [Y/n] '
        read -r answer <"$terminal" || answer="n"
    else
        docker run --rm --user "$(id -u):$(id -g)" -v "$(pwd):/setup" "$image" setup --tag "$tag" "$@" </dev/null
        answer="n"
    fi
    case "$answer" in
        [nN]*) echo "Later: cd $(pwd) && docker compose up -d" ;;
        *) docker compose up -d --remove-orphans && docker compose ps ;;
    esac
}

main "$@"
