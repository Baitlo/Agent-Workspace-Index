#!/usr/bin/env bash
set -euo pipefail
umask 077

repository="${AWI_RELEASE_REPOSITORY:-Baitlo/Agent-Workspace-Index}"
version="${AWI_VERSION:-latest}"
bin_dir="${AWI_INSTALL_BIN_DIR:-$HOME/.local/bin}"
release_base_url="${AWI_RELEASE_BASE_URL:-}"
workspace=""
workspace_args=()

usage() {
	cat <<'EOF'
Install a prebuilt AWI release for Linux x86_64/arm64 or macOS Apple Silicon/Intel.

Usage:
  install-release.sh [options]

Options:
  --workspace PATH       Also index this workspace and register Agent clients
  --version VERSION      Release tag such as v0.2.0 (default: latest)
  --index-dir PATH       Local index directory for --workspace
  --bin-dir PATH         Binary install directory (default: ~/.local/bin)
  --clients LIST         Comma-separated clients for --workspace (default: all)
  --skip-pi-adapter      Do not install the Pi MCP adapter
  --skip-agent-instructions
                         Do not add the managed AWI block to workspace/AGENTS.md
  --skip-agent-knowledge Do not index ancestor AGENTS.md or discovered Skills
  --skip-agent-memory    Do not discover project-scoped Agent memory
  --include-raw-memory   Include matching raw Agent history (opt-in)
  -h, --help             Show this help
EOF
}

fail() {
	printf 'error: %s\n' "$*" >&2
	exit 1
}

while (($# > 0)); do
	case "$1" in
	--workspace)
		(($# >= 2)) || fail "--workspace requires a value"
		workspace="$2"
		shift 2
		;;
	--version)
		(($# >= 2)) || fail "--version requires a value"
		version="$2"
		shift 2
		;;
	--bin-dir)
		(($# >= 2)) || fail "--bin-dir requires a path"
		bin_dir="$2"
		shift 2
		;;
	--index-dir | --clients)
		(($# >= 2)) || fail "$1 requires a value"
		workspace_args+=("$1" "$2")
		shift 2
		;;
	--skip-pi-adapter | --skip-agent-instructions | --skip-agent-knowledge | --skip-agent-memory | --include-raw-memory)
		workspace_args+=("$1")
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

if [[ -z "$workspace" && ${#workspace_args[@]} -gt 0 ]]; then
	fail "workspace setup options require --workspace"
fi

case "$(uname -s)" in
Linux)
	case "$(uname -m)" in
	x86_64 | amd64)
		target="x86_64-unknown-linux-gnu"
		;;
	aarch64 | arm64)
		target="aarch64-unknown-linux-gnu"
		;;
	*)
		fail "unsupported Linux architecture: $(uname -m)"
		;;
	esac
	;;
Darwin)
	case "$(uname -m)" in
	arm64)
		target="aarch64-apple-darwin"
		;;
	x86_64)
		target="x86_64-apple-darwin"
		;;
	*)
		fail "unsupported macOS architecture: $(uname -m)"
		;;
	esac
	;;
*)
	fail "prebuilt releases currently support Linux and macOS only"
	;;
esac

for command in curl tar install; do
	command -v "$command" >/dev/null 2>&1 || fail "$command is required"
done
if command -v sha256sum >/dev/null 2>&1; then
	sha256_check() { sha256sum --check -; }
elif command -v shasum >/dev/null 2>&1; then
	sha256_check() { shasum -a 256 --check -; }
else
	fail "sha256sum or shasum is required"
fi

if [[ -n "$release_base_url" ]]; then
	release_url="${release_base_url%/}"
elif [[ "$version" == "latest" ]]; then
	release_url="https://github.com/$repository/releases/latest/download"
else
	[[ "$version" == v* ]] || version="v$version"
	release_url="https://github.com/$repository/releases/download/$version"
fi

archive="awi-${target}.tar.gz"
temporary="$(mktemp -d "${TMPDIR:-/tmp}/awi-install.XXXXXXXX")"
temporary_target=""
cleanup() {
	rm -rf "$temporary"
	[[ -z "$temporary_target" ]] || rm -f "$temporary_target"
}
trap cleanup EXIT

curl --fail --location --silent --show-error \
	"$release_url/$archive" \
	--output "$temporary/$archive"
curl --fail --location --silent --show-error \
	"$release_url/SHA256SUMS" \
	--output "$temporary/SHA256SUMS"

checksum="$(
	awk -v archive="$archive" '$2 == archive || $2 == "*" archive { print; exit }' \
		"$temporary/SHA256SUMS"
)"
[[ -n "$checksum" ]] || fail "release checksum is missing for $archive"
(
	cd "$temporary"
	printf '%s\n' "$checksum" | sha256_check
)

tar --extract --gzip --file "$temporary/$archive" --directory "$temporary"
source_binary="$temporary/awi-${target}/awi"
[[ -x "$source_binary" ]] || fail "release archive does not contain an AWI executable"

if [[ -n "$workspace" ]]; then
	installer="$temporary/awi-${target}/install.sh"
	[[ -x "$installer" ]] || fail "release archive does not contain install.sh"
	"$installer" \
		--source-binary "$source_binary" \
		--workspace "$workspace" \
		--bin-dir "$bin_dir" \
		"${workspace_args[@]}"
	exit 0
fi

mkdir -p "$bin_dir"
temporary_target="$bin_dir/.awi.install.$$"
install -m 0755 "$source_binary" "$temporary_target"
mv -f "$temporary_target" "$bin_dir/awi"
temporary_target=""
"$bin_dir/awi" --version
printf 'Installed AWI to %s\n' "$bin_dir/awi"
