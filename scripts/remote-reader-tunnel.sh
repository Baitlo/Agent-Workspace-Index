#!/usr/bin/env bash
set -Eeuo pipefail

if (($# != 4)); then
  printf 'usage: %s <local-socket> <remote-socket> <host> <port>\n' "$0" >&2
  exit 2
fi

local_socket=$1
remote_socket=$2
host=$3
port=$4
child=

cleanup() {
  if [[ -n "$child" ]] && kill -0 "$child" 2>/dev/null; then
    kill -TERM "$child"
    wait "$child" 2>/dev/null || true
  fi
  rm -f "$local_socket"
}
trap cleanup EXIT
trap 'exit 0' INT TERM

while true; do
  rm -f "$local_socket"
  status=0
  ssh -o BatchMode=yes -o ExitOnForwardFailure=yes \
    -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
    -o StreamLocalBindUnlink=yes -p "$port" \
    -L "$local_socket:$remote_socket" "$host" -N &
  child=$!
  wait "$child" || status=$?
  child=
  printf '%s ssh_exit_status=%s; reconnecting\n' "$(date -Is)" "$status" >&2
  sleep 1
done
