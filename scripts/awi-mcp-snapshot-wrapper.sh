#!/usr/bin/env bash
set -euo pipefail
umask 077

semantic_env="${AWI_SEMANTIC_ENV_FILE:-$HOME/.config/awi/semantic.env}"
if [[ -f "$semantic_env" ]]; then
	set -a
	# shellcheck source=/dev/null
	source "$semantic_env"
	set +a
fi

: "${AWI_SNAPSHOT_SOURCE:?set AWI_SNAPSHOT_SOURCE to an immutable AWI publication}"
: "${AWI_MCP_AUDIT_LOG:?set AWI_MCP_AUDIT_LOG to a persistent JSONL path}"

awi_bin="${AWI_BIN:-$HOME/.local/bin/awi}"
runtime_dir="${AWI_RUNTIME_DIR:-${TMPDIR:-/tmp}/awi-${UID}}"
index_dir="$runtime_dir/index"
state_dir="${AWI_STATE_DIR:-$HOME/.local/state/awi-${UID}}"
socket_path="${AWI_SOCKET_PATH:-$state_dir/awi.sock}"
start_lock="$state_dir/start.lock"
daemon_log="$(dirname "$AWI_MCP_AUDIT_LOG")/daemon.log"
startup_timeout_ms="${AWI_START_TIMEOUT_MS:-${START_MCP_TIMEOUT_MS:-600000}}"
case "$startup_timeout_ms" in
	'' | *[!0-9]*) echo "AWI startup timeout must be a positive integer" >&2; exit 2 ;;
esac
((startup_timeout_ms >= 600000)) || startup_timeout_ms=600000
poll_interval_ms=100
startup_attempts=$((startup_timeout_ms / poll_interval_ms))
((startup_attempts > 0)) || startup_attempts=1

mkdir -p "$runtime_dir" "$index_dir" "$state_dir" "$(dirname "$AWI_MCP_AUDIT_LOG")"
chmod 700 "$runtime_dir" "$state_dir" "$(dirname "$AWI_MCP_AUDIT_LOG")"
touch "$daemon_log"
chmod 600 "$daemon_log"

daemon_healthy() {
	"$awi_bin" --index-dir "$index_dir" --socket "$socket_path" \
		ping --json >/dev/null 2>&1
}

if ! daemon_healthy; then
	exec 9>"$start_lock"
	flock -x 9
	if ! daemon_healthy; then
		nohup "$awi_bin" --index-dir "$index_dir" --socket "$socket_path" \
			serve --snapshot-source "$AWI_SNAPSHOT_SOURCE" \
			>>"$daemon_log" 2>&1 </dev/null 9>&- &

		for _ in $(seq 1 "$startup_attempts"); do
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
