#!/bin/sh
# Runs as the configured user, independently of a desktop login.
# launchd retries after network failures. Never reset Tailscale preferences
# or create a new device identity in a background job.
set -eu
cli=$1
variant=$2
if [ "$variant" = standalone ]; then
    /usr/sbin/scutil --nc start Tailscale
fi
state=$("$cli" status --json 2>/dev/null) || state=''
case "$state" in
    *'"BackendState": "Running"'*|*'"BackendState":"Running"'*) exit 0 ;;
    *'"BackendState": "NeedsLogin"'*|*'"BackendState":"NeedsLogin"'*)
        echo 'Tailscale requires an interactive sign-in. Open VNC Beach server settings.' >&2
        exit 1 ;;
esac
# No flags means reconnect using all existing settings. Bound the command
# externally: --timeout is itself a flag and would require restating prefs.
"$cli" up >/dev/null 2>&1 &
up_pid=$!
trap 'kill "$up_pid" 2>/dev/null || true' EXIT HUP INT TERM
elapsed=0
while kill -0 "$up_pid" 2>/dev/null; do
    if [ "$elapsed" -ge 20 ]; then
        echo 'Tailscale reconnect timed out; launchd will retry.' >&2
        exit 1
    fi
    sleep 1
    elapsed=$((elapsed + 1))
done
wait "$up_pid"
