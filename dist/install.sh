#!/usr/bin/env bash
# Install the conde binary for this user, in ~/.local/bin (no sudo).
#
#   curl -fsSL https://conde.cloud/install.sh | bash
#
# CONDE_VERSION picks a release (default: the one in `latest`); CONDE_DIST_BASE_URL overrides the
# download base, CONDE_INSTALL_DIR the target directory.
if [ -z "${BASH_VERSION:-}" ]; then
	echo "install.sh: run it with bash: curl -fsSL https://conde.cloud/install.sh | bash" >&2
	exit 1
fi
set -euo pipefail

base=${CONDE_DIST_BASE_URL:-https://storage.googleapis.com/conde-cloud-dist}
dir=${CONDE_INSTALL_DIR:-$HOME/.local/bin}

say() { printf '%s\n' "$*"; }
die() {
	printf 'install.sh: %s\n' "$*" >&2
	exit 1
}

case $(uname -s) in
Darwin) os=darwin ;;
Linux) os=linux ;;
*) die "unsupported OS $(uname -s); conde ships for macOS and Linux" ;;
esac
case $(uname -m) in
arm64 | aarch64)
	[ "$os" = darwin ] || die "unsupported CPU: Linux ships for x86_64 only"
	arch=arm64
	;;
x86_64 | amd64)
	arch=x86_64
	# an x86_64 shell under Rosetta on Apple silicon still gets the native build
	[ "$os" = darwin ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ] && arch=arm64
	;;
*) die "unsupported CPU $(uname -m); conde ships for x86_64 and Apple silicon" ;;
esac
name=conde-$os-$arch

command -v curl >/dev/null || die "curl is required"
if command -v sha256sum >/dev/null; then
	check() { sha256sum -c "$1"; }
elif command -v shasum >/dev/null; then
	check() { shasum -a 256 -c "$1"; }
else
	die "sha256sum or shasum is required to verify the download"
fi

version=${CONDE_VERSION:-$(curl -fsSL "$base/latest" | head -n 1 | tr -d '\r')} || die "cannot read $base/latest"
version=${version#v}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([.+-][0-9A-Za-z.+-]+)?$ ]] || die "not a release version: '$version'"
url=$base/v$version

# staged next to the target, so the final rename replaces a running conde atomically
mkdir -p "$dir"
tmp=$(mktemp -d "$dir/.conde-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
say "downloading $name v$version from $base"
curl -fsSL -o "$tmp/$name" "$url/$name" || die "download failed: $url/$name"
curl -fsSL -o "$tmp/SHA256SUMS" "$url/SHA256SUMS" || die "download failed: $url/SHA256SUMS; nothing installed"

# SHA256SUMS is the contract: check this file's line ("<hash>  <name>") only
tr -d '\r' <"$tmp/SHA256SUMS" | grep -E "^[0-9a-fA-F]{64} [ *]$name\$" >"$tmp/$name.sum" || die "SHA256SUMS has no line for $name; nothing installed"
(cd "$tmp" && check "$name.sum" >/dev/null 2>&1) || die "checksum mismatch for $name; nothing installed"
chmod 0755 "$tmp/$name"
# curl sets no quarantine flag; clear one anyway if a proxy or tool added it
[ "$os" = darwin ] && { xattr -d com.apple.quarantine "$tmp/$name" 2>/dev/null || true; }
"$tmp/$name" --version >/dev/null 2>&1 || die "the downloaded binary does not run on this machine ($name)"

# An upgrade: what the old conde says it is, given at most 5 s (an old binary that hangs must not stop it).
upgrade=0 previous="unknown version"
if [ -e "$dir/conde" ]; then
	upgrade=1
	if [ -x "$dir/conde" ]; then
		"$dir/conde" --version >"$tmp/previous" 2>/dev/null </dev/null &
		probe=$!
		for _ in $(seq 50); do kill -0 "$probe" 2>/dev/null || break; sleep 0.1; done
		kill -KILL "$probe" 2>/dev/null || true
		if wait "$probe" 2>/dev/null && [ -s "$tmp/previous" ]; then previous=$(head -n 1 "$tmp/previous"); fi
	fi
fi

mv -f "$tmp/$name" "$dir/conde"
installed=$("$dir/conde" --version 2>/dev/null || echo "$name v$version")
if [ "$upgrade" = 1 ]; then say "replaced $dir/conde ($previous) with $installed"; else say "installed $dir/conde ($installed)"; fi

case ":$PATH:" in
*":$dir:"*) ;;
*) printf '\n%s\n  %s\n' "$dir is not on your PATH. Add this line to your shell's startup file (~/.zshrc, ~/.bashrc), then open a new terminal:" \
	"export PATH=\"$dir:\$PATH\"" ;;
esac

say ""
if [ "$upgrade" = 1 ]; then
	say "Next (an upgrade; your enrollment and names stay as they are, no sign-in):"
	say "  conde setup        # rewrites what this release changed (skills, MCP entries, the service) and restarts the bridge onto it"
	say "  conde doctor       # shows: ok bridge: running this build"
else
	say "Next:"
	say "  conde login --server https://<workspace>.conde.cloud   # the address in your invite; your human clicks the browser sign-in, then it sets up this machine"
	say "(No workspace: conde setup sets up this machine with its local board only.)"
fi
