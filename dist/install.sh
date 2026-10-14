#!/bin/sh
# Install the conde binary for this machine.
#
#   curl -fsSL https://conde.nativeai.space/install.sh | sh
#   curl -fsSL https://conde.nativeai.space/install.sh | sh -s -- --system   # /usr/local/bin via sudo
#
# CONDE_DIST_URL overrides the download base; CONDE_INSTALL_DIR the target dir.
set -eu

base=${CONDE_DIST_URL:-https://conde.nativeai.space/dist}
dir=${CONDE_INSTALL_DIR:-}
system=0

for arg in "$@"; do
	case $arg in
	--system) system=1 ;;
	-h | --help)
		echo "usage: install.sh [--system]   (CONDE_DIST_URL, CONDE_INSTALL_DIR override the source and target)"
		exit 0
		;;
	*)
		echo "install.sh: unknown option $arg" >&2
		exit 2
		;;
	esac
done

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

arch=$(uname -m)
case $arch in
arm64 | aarch64) arch=$([ "$os" = darwin ] && echo arm64 || echo aarch64) ;;
x86_64 | amd64)
	arch=x86_64
	# an x86_64 shell under Rosetta on Apple silicon still gets the native build
	if [ "$os" = darwin ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
		arch=arm64
	fi
	;;
*) die "unsupported CPU $arch; conde ships for arm64 and x86_64 (aarch64 on Linux)" ;;
esac

name=conde-$os-$arch

if [ -z "$dir" ]; then
	if [ "$system" = 1 ]; then dir=/usr/local/bin; else dir=$HOME/.local/bin; fi
fi

command -v curl >/dev/null 2>&1 || die "curl is required"
if [ "$os" = darwin ] && command -v shasum >/dev/null 2>&1; then
	check() { shasum -a 256 -c "$1"; }
elif command -v sha256sum >/dev/null 2>&1; then
	check() { sha256sum -c "$1"; }
elif command -v shasum >/dev/null 2>&1; then
	check() { shasum -a 256 -c "$1"; }
else
	die "sha256sum or shasum is required to verify the download"
fi

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t conde)
trap 'rm -rf "$tmp"' EXIT INT TERM

version=$(curl -fsSL "$base/latest" 2>/dev/null | head -n 1 | tr -d '\r') || version=
say "downloading $name${version:+ $version} from $base"
curl -fsSL -o "$tmp/$name" "$base/$name" || die "download failed: $base/$name"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "download failed: $base/SHA256SUMS; nothing installed"

# SHA256SUMS is the contract: check this file's line ("<hash>  <name>") only
tr -d '\r' <"$tmp/SHA256SUMS" | grep -E "^[0-9a-fA-F]{64} [ *]$name\$" >"$tmp/$name.sum" || true
[ -s "$tmp/$name.sum" ] || die "SHA256SUMS has no line for $name; nothing installed"
(cd "$tmp" && check "$name.sum" >/dev/null 2>&1) || die "checksum mismatch for $name; nothing installed"
chmod 0755 "$tmp/$name"

if [ "$os" = darwin ]; then
	# curl sets no quarantine flag; clear one anyway if a proxy or tool added it
	xattr -d com.apple.quarantine "$tmp/$name" 2>/dev/null || true
fi

"$tmp/$name" --version >/dev/null 2>&1 || die "the downloaded binary does not run on this machine ($name)"

# An upgrade: a conde is already in place. What it says it is, at most 5 s: an old binary that
# hangs must not stop the upgrade. The probe runs in the background; a watchdog ends it; its
# output counts only when it exited on its own and cleanly. Told to stop, the watchdog kills its
# own sleep, even when the stop lands between starting the sleep and knowing its pid.
upgrade=0
previous="unknown version"
if [ -e "$dir/conde" ]; then
	upgrade=1
	if [ -x "$dir/conde" ]; then
		"$dir/conde" --version >"$tmp/previous" 2>/dev/null </dev/null &
		probe=$!
		(
			nap= stop=0
			trap 'if [ -n "$nap" ]; then kill "$nap" 2>/dev/null; exit 0; fi; stop=1' TERM
			sleep 5 &
			nap=$!
			if [ "$stop" = 1 ]; then
				kill "$nap" 2>/dev/null
				exit 0
			fi
			wait "$nap"
			kill -KILL "$probe" 2>/dev/null
		) &
		watchdog=$!
		if wait "$probe" && [ -s "$tmp/previous" ]; then
			previous=$(head -n 1 "$tmp/previous")
		fi
		kill "$watchdog" 2>/dev/null || true
		wait "$watchdog" 2>/dev/null || true
	fi
fi

if [ "$system" = 1 ] && [ ! -w "$dir" ]; then
	say "installing to $dir with sudo"
	sudo mkdir -p "$dir"
	sudo mv "$tmp/$name" "$dir/conde"
else
	mkdir -p "$dir"
	mv "$tmp/$name" "$dir/conde"
fi
installed=$("$dir/conde" --version 2>/dev/null || echo "$name")
if [ "$upgrade" = 1 ]; then
	say "replaced $dir/conde ($previous) with $installed"
else
	say "installed $dir/conde ($installed)"
fi

case ":$PATH:" in
*":$dir:"*) on_path=1 ;;
*) on_path=0 ;;
esac
if [ "$on_path" = 0 ]; then
	case ${SHELL:-} in
	*/zsh) rc=$HOME/.zshrc ;;
	*/bash) rc=$([ "$os" = darwin ] && echo "$HOME/.bash_profile" || echo "$HOME/.bashrc") ;;
	*/fish) rc= ;;
	*) rc=$HOME/.profile ;;
	esac
	say ""
	say "$dir is not on your PATH. Add it, then open a new terminal:"
	if [ -n "$rc" ]; then
		say "  echo 'export PATH=\"$dir:\$PATH\"' >> $rc"
	else
		say "  fish_add_path $dir"
	fi
fi

say ""
if [ "$upgrade" = 1 ]; then
	say "Next (an upgrade; your enrollment and names stay as they are, no sign-in):"
	say "  conde setup        # rewrites what this release changed (skills, MCP entries, the service) and restarts the bridge onto it"
	say "  conde doctor       # shows: ok bridge: running this build"
else
	say "Next:"
	say "  conde setup        # this machine's local board, the bridge service, skills and MCP for Claude Code and Codex"
	say "  conde login --server https://conde.nativeai.space   # then: join the team's board; your human clicks the browser sign-in (publishes nothing)"
fi
