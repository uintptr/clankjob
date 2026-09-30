#!/bin/sh
# The image's entry point: `setup` and `update` run the setup wizard on the directory
# mounted at /setup (deploy/clankjob_setup.py); anything else starts the server.
case "${1:-}" in
    setup | update) exec python3 /usr/local/lib/clankjob/clankjob_setup.py "$@" ;;
esac
exec /usr/local/bin/clankjob "$@"
