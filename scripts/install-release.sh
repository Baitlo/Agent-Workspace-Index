#!/usr/bin/env bash
set -euo pipefail
umask 077

repository="${AWI_RELEASE_REPOSITORY:-Baitlo/Agent-Workspace-Index}"
version="${AWI_VERSION:-latest}"
bin_dir="${AWI_INSTALL_BIN_DIR:-$HOME/.local/bin}"
release_base_url="${AWI_RELEASE_BASE_URL:-}"

usage() {
	cat <<'EOF'
Install a prebuilt AWI binary for Linux x86_64 or arm64.

Usage:
  install-release.sh [options]

Options:
  --version VERSION  Release tag such as v0.1.0 (default: latest)
  --bin-dir PATH     Binary install directory (default: ~/.local/bin)
  -h, --help         Show this help
EOF
}

fail() {
	printf 'error: %s\n' "$*" >&2
	exit 1
}

while (($# > 0)); do
	case "$1" in
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
	-h | --help)
		usage
		exit 0
		;;
	*)
		fail "unknown option: $1"
		;;
	esac
done

[[ "$(uname -s)" == "Linux" ]] || fail "prebuilt releases currently support Linux only"
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

for command in curl tar sha256sum install; do
	command -v "$command" >/dev/null 2>&1 || fail "$command is required"
done

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
	printf '%s\n' "$checksum" | sha256sum --check -
)

tar --extract --gzip --file "$temporary/$archive" --directory "$temporary"
source_binary="$temporary/awi-${target}/awi"
[[ -x "$source_binary" ]] || fail "release archive does not contain an AWI executable"

mkdir -p "$bin_dir"
temporary_target="$bin_dir/.awi.install.$$"
install -m 0755 "$source_binary" "$temporary_target"
mv -f "$temporary_target" "$bin_dir/awi"
temporary_target=""
"$bin_dir/awi" --version
printf 'Installed AWI to %s\n' "$bin_dir/awi"
