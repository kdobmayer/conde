#!/bin/sh
# Local release build: the static Linux binary plus SHA256SUMS in dist/out/v<version>/, laid out
# like the dist bucket. macOS builds come from .github/workflows/release.yml (GitHub macOS runners).
#
#   dist/release.sh
#
# Tool choice per target: cargo-zigbuild, else cross, else plain cargo when the
# musl target's standard library and a C compiler for it are installed.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/crates/conde/Cargo.toml" | head -n 1)
out=$root/dist/out/v$version
mkdir -p "$out"
built=0
missing=""

have() { command -v "$1" >/dev/null 2>&1; }

# the musl standard library is installed for this rustc
has_std() {
	libdir=$(rustc --print target-libdir --target "$1" 2>/dev/null) || return 1
	ls "$libdir"/libstd-* >/dev/null 2>&1
}

build() {
	target=$1 name=$2 cc=$3
	if have cargo-zigbuild; then
		(cd "$root" && cargo zigbuild --release --locked -p conde --target "$target")
	elif have cross; then
		(cd "$root" && cross build --release --locked -p conde --target "$target")
	elif has_std "$target" && have "$cc"; then
		# rusqlite compiles SQLite from C, so the target needs its own C compiler
		(cd "$root" && env "CC_$(echo "$target" | tr - _)=$cc" cargo build --release --locked -p conde --target "$target")
	else
		reason="$target: needs cargo-zigbuild (pip install cargo-zigbuild) or cross, or"
		if has_std "$target"; then
			reason="$reason the C compiler $cc"
		else
			reason="$reason the $target standard library (rustup target add $target) and the C compiler $cc"
		fi
		missing="$missing
  $reason"
		return 0
	fi
	cp "$root/target/$target/release/conde" "$out/$name"
	built=$((built + 1))
	echo "built dist/out/v$version/$name"
}

build x86_64-unknown-linux-musl conde-linux-x86_64 musl-gcc

cp "$root/dist/install.sh" "$root/dist/out/install.sh"
echo "v$version" >"$root/dist/out/latest"
sums=$(cd "$out" && find . -maxdepth 1 -type f -name 'conde-*' -exec basename {} \; | sort | xargs sha256sum)
printf '%s\n' "$sums" >"$out/SHA256SUMS"

if [ -n "$missing" ]; then
	echo "not built:$missing" >&2
fi
if [ "$built" = 0 ]; then
	echo "release.sh: nothing built" >&2
	exit 1
fi
echo "dist/out ready for v$version (Linux only; the release workflow publishes all three binaries)"
