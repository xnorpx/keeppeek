#!/usr/bin/env sh

set -eu

if ! command -v pgrep >/dev/null 2>&1; then
        printf '%s\n' 'pgrep is required to stop KeepPeek.' >&2
        exit 1
fi

server_pids=$(pgrep -u "$(id -u)" -x keeppeek || true)
if [ -z "$server_pids" ]; then
        printf '%s\n' 'KeepPeek is not running.'
        exit 0
fi

is_running() {
        for server_pid in $server_pids; do
                if kill -0 "$server_pid" 2>/dev/null; then
                        return 0
                fi
        done
        return 1
}

wait_for_stop() {
        attempts=$1
        while is_running && [ "$attempts" -gt 0 ]; do
                sleep 0.1
                attempts=$((attempts - 1))
        done
        ! is_running
}

printf '%s\n' 'Stopping KeepPeek...'
for server_pid in $server_pids; do
        kill -INT "$server_pid" 2>/dev/null || true
done

if ! wait_for_stop 100; then
        printf '%s\n' 'KeepPeek did not stop within 10 seconds; forcing shutdown.' >&2
        for server_pid in $server_pids; do
                kill -KILL "$server_pid" 2>/dev/null || true
        done
        if ! wait_for_stop 20; then
                printf '%s\n' 'Unable to stop KeepPeek.' >&2
                exit 1
        fi
fi

printf '%s\n' 'KeepPeek stopped.'