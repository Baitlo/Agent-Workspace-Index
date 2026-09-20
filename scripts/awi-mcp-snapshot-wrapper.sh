#!/usr/bin/env bash
set -euo pipefail
umask 077

: "${AWI_SNAPSHOT_SOURCE:?set AWI_SNAPSHOT_SOURCE to an immutable AWI publication}"
: "${AWI_MCP_AUDIT_LOG:?set AWI_MCP_AUDIT_LOG to a persistent JSONL path}"

awi_bin="${AWI_BIN:-$HOME/.local/bin/awi}"
runtime_dir="${AWI_RUNTIME_DIR:-${TMPDIR:-/tmp}/awi-${UID}}"
index_dir="$runtime_dir/index"
socket_path="$runtime_dir/awi.sock"
daemon_log="$(dirname "$AWI_MCP_AUDIT_LOG")/daemon.log"

mkdir -p "$runtime_dir" "$index_dir" "$(dirname "$AWI_MCP_AUDIT_LOG")"
chmod 700 "$runtime_dir" "$(dirname "$AWI_MCP_AUDIT_LOG")"
touch "$daemon_log"
chmod 600 "$daemon_log"

daemon_healthy() {
    "$awi_bin" --index-dir "$index_dir" --socket "$socket_path" \
        ping --json >/dev/null 2>&1
}

if ! daemon_healthy; then
    exec 9>"$runtime_dir/start.lock"
    flock -x 9
    if ! daemon_healthy; then
        nohup "$awi_bin" --index-dir "$index_dir" --socket "$socket_path" \
            serve --snapshot-source "$AWI_SNAPSHOT_SOURCE" \
            >>"$daemon_log" 2>&1 < /dev/null 9>&- &

        for _ in $(seq 1 300); do
            daemon_healthy && break
            sleep 0.1
        done
    fi
    flock -u 9
fi

if ! daemon_healthy; then
    echo "AWI daemon did not become healthy; see $daemon_log" >&2
    exit 1
fi

exec "$awi_bin" --index-dir "$index_dir" --socket "$socket_path" \
    mcp --audit-log "$AWI_MCP_AUDIT_LOG"
