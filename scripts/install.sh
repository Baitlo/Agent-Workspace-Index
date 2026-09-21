#!/usr/bin/env bash
set -euo pipefail
umask 077

usage() {
    cat <<'EOF'
Install AWI, build an initial workspace index, and register detected Agent clients.

Usage:
  scripts/install.sh [options]

Options:
  --workspace PATH       Workspace to index (default: current directory)
  --index-dir PATH       Local index directory (default: XDG cache, keyed by workspace)
  --bin-dir PATH         Binary install directory (default: ~/.local/bin)
  --clients LIST         Comma-separated clients passed to `awi integrate` (default: all).
                         Supports codex, gemini, claude, copilot, trae, zcode,
                         kimi, opencode, pi, cursor, windsurf, qwen, cline,
                         zed, amazon-q, and crush
  --source-binary PATH   Install an existing AWI binary instead of building from source
  --skip-pi-adapter      Do not install pi-mcp-adapter when Pi is detected
  -h, --help             Show this help
EOF
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

command_exists() {
    command -v "$1" >/dev/null 2>&1
}

absolute_dir() {
    (
        cd -- "$1"
        pwd -P
    )
}

workspace_key() {
    local path="$1"
    local digest
    if command_exists sha256sum; then
        digest="$(printf '%s' "$path" | sha256sum | awk '{print substr($1, 1, 12)}')"
    elif command_exists shasum; then
        digest="$(printf '%s' "$path" | shasum -a 256 | awk '{print substr($1, 1, 12)}')"
    else
        digest="$(printf '%s' "$path" | cksum | awk '{print $1}')"
    fi
    printf '%s-%s\n' "$(basename "$path" | tr -cs 'A-Za-z0-9._-' '_')" "$digest"
}

client_requested() {
    local requested="$1"
    case ",$clients," in
        *,all,* | *,"$requested",*) return 0 ;;
        *) return 1 ;;
    esac
}

repo_root="$(absolute_dir "$(dirname -- "${BASH_SOURCE[0]}")/..")"
workspace="$PWD"
index_dir=""
bin_dir="${AWI_INSTALL_BIN_DIR:-$HOME/.local/bin}"
clients="all"
source_binary="${AWI_SOURCE_BINARY:-}"
install_pi_adapter=true
pi_adapter_spec="${AWI_PI_MCP_ADAPTER_SPEC:-npm:pi-mcp-adapter@2.34.0}"

while (($# > 0)); do
    case "$1" in
        --workspace)
            (($# >= 2)) || fail "--workspace requires a path"
            workspace="$2"
            shift 2
            ;;
        --index-dir)
            (($# >= 2)) || fail "--index-dir requires a path"
            index_dir="$2"
            shift 2
            ;;
        --bin-dir)
            (($# >= 2)) || fail "--bin-dir requires a path"
            bin_dir="$2"
            shift 2
            ;;
        --clients)
            (($# >= 2)) || fail "--clients requires a comma-separated list"
            clients="$2"
            shift 2
            ;;
        --source-binary)
            (($# >= 2)) || fail "--source-binary requires a path"
            source_binary="$2"
            shift 2
            ;;
        --skip-pi-adapter)
            install_pi_adapter=false
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            fail "unknown option: $1"
            ;;
    esac
done

[[ -d "$workspace" ]] || fail "workspace does not exist: $workspace"
workspace="$(absolute_dir "$workspace")"

if [[ -z "$index_dir" ]]; then
    index_dir="${XDG_CACHE_HOME:-$HOME/.cache}/awi/indexes/$(workspace_key "$workspace")"
elif [[ "$index_dir" != /* ]]; then
    index_dir="$PWD/$index_dir"
fi

if [[ "$bin_dir" != /* ]]; then
    bin_dir="$PWD/$bin_dir"
fi

if [[ -z "$source_binary" ]]; then
    command_exists cargo || fail "cargo is required to build AWI"
    cargo_target_dir="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/awi-build-${UID:-$(id -u)}}"
    printf 'Building AWI release binary...\n'
    CARGO_TARGET_DIR="$cargo_target_dir" cargo build \
        --manifest-path "$repo_root/Cargo.toml" \
        --release \
        --locked
    source_binary="$cargo_target_dir/release/awi"
elif [[ "$source_binary" != /* ]]; then
    source_binary="$PWD/$source_binary"
fi

[[ -x "$source_binary" ]] || fail "AWI binary is not executable: $source_binary"

mkdir -p "$bin_dir" "$index_dir"
install_target="$bin_dir/awi"
temporary_target="$bin_dir/.awi.install.$$"
install -m 0755 "$source_binary" "$temporary_target"
mv -f "$temporary_target" "$install_target"

printf 'Building initial index for %s...\n' "$workspace"
"$install_target" --index-dir "$index_dir" reconcile "$workspace" --json

if client_requested pi && command_exists pi && "$install_pi_adapter"; then
    if ! pi list 2>/dev/null | grep -Fq "pi-mcp-adapter"; then
        printf 'Installing Pi MCP adapter (third-party package %s)...\n' "$pi_adapter_spec"
        pi install "$pi_adapter_spec"
        pi list 2>/dev/null | grep -Fq "pi-mcp-adapter" ||
            fail "Pi reported success but pi-mcp-adapter is not listed"
    fi
fi

printf 'Registering AWI with detected Agent clients...\n'
integrate_args=(
    --index-dir "$index_dir"
    integrate
    --project-root "$workspace"
)
if [[ "$clients" != "all" ]]; then
    integrate_args+=(--client "$clients")
fi
"$install_target" "${integrate_args[@]}"

printf '\nAWI installation complete.\n'
printf '  binary:    %s\n' "$install_target"
printf '  workspace: %s\n' "$workspace"
printf '  index:     %s\n' "$index_dir"
if [[ ":$PATH:" != *":$bin_dir:"* ]]; then
    printf '  note: add %s to PATH for direct CLI use\n' "$bin_dir"
fi
