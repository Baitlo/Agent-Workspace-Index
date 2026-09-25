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
  --skip-agent-instructions
                         Do not add the managed AWI block to workspace/AGENTS.md
  --skip-agent-knowledge Do not index ancestor AGENTS.md or discovered SKILL.md files
  --skip-agent-memory    Do not discover or index Agent memory for this workspace
  --include-raw-memory   Also index matching raw chat/session history (opt-in)
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

append_agent_root() {
    local candidate="$1"
    local resolved
    [[ -e "$candidate" ]] || return 0
    if [[ -d "$candidate" ]]; then
        resolved="$(absolute_dir "$candidate")"
    else
        resolved="$(absolute_dir "$(dirname -- "$candidate")")/$(basename -- "$candidate")"
    fi
    case "$resolved" in
        "$workspace" | "$workspace"/*) return 0 ;;
    esac
    local existing
    for existing in "${agent_roots[@]-}"; do
        [[ "$existing" == "$resolved" ]] && return 0
    done
    agent_roots+=("$resolved")
}

append_skill_manifests() {
    local directory="$1"
    local depth="${2:-0}"
    local resolved
    local existing
    local child
    [[ -d "$directory" ]] || return 0
    resolved="$(absolute_dir "$directory")"
    case "$(basename -- "$resolved")" in
        .git | node_modules | target | .cache | .archive | .backups | skill-backups) return 0 ;;
    esac
    for existing in "${agent_scan_dirs[@]-}"; do
        [[ "$existing" == "$resolved" ]] && return 0
    done
    agent_scan_dirs+=("$resolved")
    if [[ -f "$resolved/SKILL.md" ]]; then
        append_agent_root "$resolved/SKILL.md"
        return 0
    fi
    ((depth < 4)) || return 0
    for child in "$resolved"/* "$resolved"/.[!.]* "$resolved"/..?*; do
        [[ -d "$child" ]] || continue
        append_skill_manifests "$child" "$((depth + 1))"
    done
}

awi_agent_instructions() {
    cat <<'EOF'
<!-- BEGIN AWI MANAGED INSTRUCTIONS -->
## AWI Workspace Retrieval

- For unknown code symbols, files, configuration, SQL, data schemas, Agent
  instructions, Skills, or project history, use AWI `workspace_search` before
  shell discovery.
- If the AWI namespace is deferred, use `tool_search` to load
  `mcp__awi.workspace_search`, then make an actual `workspace_search` call.
- Start with one identifier-rich query and `limit=5`. Do not issue parallel
  near-synonym searches. Use `source`, `agent_instructions`, `agent_skill`, or
  `agent_memory` kinds when known; historical queries also pass this workspace
  as `context_path`.
- Treat a non-empty preview as direct indexed evidence. If more detail is
  required, inspect only the best hit and pass its `path`, `search_id`, and
  exact `symbol` when available.
- For CSV, JSON/JSONL, or Parquet aggregation, locate inputs with
  `workspace_search`, then use `workspace_query` with exact registered roots.
- Read exact user-provided paths directly. Fall back to `rg` only for unindexed
  paths, explicit regex matching, or after one AWI refinement fails.
<!-- END AWI MANAGED INSTRUCTIONS -->
EOF
}

inject_awi_agent_instructions() {
    local instructions="$workspace/AGENTS.md"
    local begin="<!-- BEGIN AWI MANAGED INSTRUCTIONS -->"
    local end="<!-- END AWI MANAGED INSTRUCTIONS -->"
    local block
    local begin_count=0
    local end_count=0
    local mode=0644
    local temporary="$workspace/.AGENTS.md.awi.$$"

    [[ ! -L "$instructions" ]] ||
        fail "refusing to replace symlinked Agent instructions: $instructions"
    if [[ -e "$instructions" ]]; then
        [[ -f "$instructions" ]] ||
            fail "Agent instructions path is not a regular file: $instructions"
        begin_count="$(grep -Fxc -- "$begin" "$instructions" || true)"
        end_count="$(grep -Fxc -- "$end" "$instructions" || true)"
        [[ "$begin_count" == "$end_count" && "$begin_count" -le 1 ]] ||
            fail "malformed AWI managed block in $instructions"
        mode="$(
            stat -c '%a' "$instructions" 2>/dev/null ||
                stat -f '%Lp' "$instructions"
        )"
    fi

    block="$(awi_agent_instructions)"
    local block_file
    block_file="$(mktemp "${TMPDIR:-/tmp}/awi-block.XXXXXX")"
    printf '%s\n' "$block" >"$block_file"
    if [[ "$begin_count" == 1 ]]; then
        if ! awk -v begin="$begin" -v end="$end" -v block_file="$block_file" '
            $0 == begin {
                while ((getline block_line < block_file) > 0) print block_line
                close(block_file)
                managed = 1
                next
            }
            managed && $0 == end { managed = 0; next }
            !managed { print }
            END { if (managed) exit 42 }
        ' "$instructions" >"$temporary"; then
            rm -f "$temporary" "$block_file"
            fail "replace AWI managed block in $instructions"
        fi
    elif [[ -f "$instructions" && -s "$instructions" ]]; then
        { cat "$instructions"; printf '\n'; cat "$block_file"; } >"$temporary"
    else
        cat "$block_file" >"$temporary"
    fi
    rm -f "$block_file"
    chmod "$mode" "$temporary"
    mv -f "$temporary" "$instructions"
    printf 'Updated AWI instructions in %s\n' "$instructions"
}

repo_root="$(absolute_dir "$(dirname -- "${BASH_SOURCE[0]}")/..")"
workspace="$PWD"
index_dir=""
bin_dir="${AWI_INSTALL_BIN_DIR:-$HOME/.local/bin}"
clients="all"
source_binary="${AWI_SOURCE_BINARY:-}"
install_pi_adapter=true
inject_agent_instructions=true
index_agent_knowledge=true
index_agent_memory=true
include_raw_memory=false
pi_adapter_spec="${AWI_PI_MCP_ADAPTER_SPEC:-npm:pi-mcp-adapter@2.36.0}"

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
        --skip-agent-instructions)
            inject_agent_instructions=false
            shift
            ;;
        --skip-agent-knowledge)
            index_agent_knowledge=false
            shift
            ;;
        --skip-agent-memory)
            index_agent_memory=false
            shift
            ;;
        --include-raw-memory)
            include_raw_memory=true
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

if "$inject_agent_instructions"; then
    inject_awi_agent_instructions
fi

printf 'Building initial index for %s...\n' "$workspace"
"$install_target" --index-dir "$index_dir" reconcile "$workspace" --json

if "$index_agent_knowledge"; then
    agent_roots=()
    agent_scan_dirs=()
    ancestor="$(dirname -- "$workspace")"
    while [[ "$ancestor" != "/" ]]; do
        append_agent_root "$ancestor/AGENTS.md"
        for relative in .agents/skills .claude/skills .codex/skills .gemini/skills .trae/skills; do
            append_skill_manifests "$ancestor/$relative"
        done
        parent="$(dirname -- "$ancestor")"
        [[ "$parent" != "$ancestor" ]] || break
        ancestor="$parent"
    done
    for candidate in \
        "$workspace/.agents/skills" \
        "$workspace/.claude/skills" \
        "$workspace/.codex/skills" \
        "$workspace/.gemini/skills" \
        "$workspace/.trae/skills" \
        "$workspace/skills" \
        "$HOME/.agents/skills" \
        "$HOME/.codex/skills" \
        "$HOME/.claude/skills" \
        "$HOME/.gemini/skills" \
        "$HOME/.trae/skills" \
        "$HOME/.trae-cn/skills" \
        "$HOME/.trae-cn/builtin_skills" \
        "$HOME/.zcode/cli/skills" \
        "${XDG_CONFIG_HOME:-$HOME/.config}/opencode/skills" \
        "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/skills"; do
        append_skill_manifests "$candidate"
    done
    for plugin_skills in "$HOME"/.trae-cn/plugins/*/*/skills; do
        append_skill_manifests "$plugin_skills"
    done
    if ((${#agent_roots[@]} > 0)); then
        printf 'Indexing %s Agent knowledge root(s)...\n' "${#agent_roots[@]}"
        for agent_root in "${agent_roots[@]}"; do
            "$install_target" --index-dir "$index_dir" reconcile "$agent_root"
        done
    fi
fi

if "$index_agent_memory"; then
    printf 'Discovering Agent memory for %s...\n' "$workspace"
    memory_args=(
        --index-dir "$index_dir"
        memory
        --project-root "$workspace"
    )
    if "$include_raw_memory"; then
        memory_args+=(--include-raw)
    fi
    "$install_target" "${memory_args[@]}"
fi

if client_requested pi && command_exists pi && "$install_pi_adapter"; then
    if ! pi list 2>/dev/null | grep -Fq "pi-mcp-adapter"; then
        printf 'Installing Pi MCP adapter (third-party package %s)...\n' "$pi_adapter_spec"
        if ! pi install "$pi_adapter_spec"; then
            case "$pi_adapter_spec" in
                npm:pi-mcp-adapter*)
                    pi_home="${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}"
                    pi_npm_root="$pi_home/npm"
                    printf 'Global npm install failed; retrying under %s...\n' "$pi_npm_root"
                    npm install --prefix "$pi_npm_root" --no-audit --no-fund \
                        "${pi_adapter_spec#npm:}"
                    pi install "$pi_npm_root/node_modules/pi-mcp-adapter"
                    ;;
                *)
                    fail "Pi MCP adapter install failed: $pi_adapter_spec"
                    ;;
            esac
        fi
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
