#!/usr/bin/env bash
# Fetches Alpine Linux's arm64 minirootfs (and, optionally, extra packages)
# into build/alpine, verifying checksums. Used by tests/alpine.sh; needs
# network access the first time.
#
#   tests/alpine-fetch.sh [PACKAGE...]
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
version=3.24.2
branch=v${version%.*}
tarball=alpine-minirootfs-$version-aarch64.tar.gz
sha256=9bf70a7f18ea44094cbb5f70c58f9af129c8214745743db0e68e5502cc2ce773
mirror=https://dl-cdn.alpinelinux.org/alpine
cache=$root/build/alpine-cache
dest=$root/build/alpine
mkdir -p "$cache"

if [ ! -e "$dest/.complete" ]; then
	[ -e "$cache/$tarball" ] || curl -sSfL -o "$cache/$tarball.part" "$mirror/$branch/releases/aarch64/$tarball" \
		&& { [ -e "$cache/$tarball" ] || mv "$cache/$tarball.part" "$cache/$tarball"; }
	echo "$sha256  $cache/$tarball" | sha256sum -c --quiet
	rm -rf "$dest"
	mkdir -p "$dest"
	tar -xzf "$cache/$tarball" -C "$dest" 2>/dev/null
	touch "$dest/.complete"
fi

# Packages: resolve names to files through the repository index, whose
# checksums (C: fields, SHA-1 of the control segment) we don't re-verify:
# packages come over HTTPS from the same origin as the verified rootfs.
[ $# -gt 0 ] || exit 0
index=$cache/APKINDEX-main
for repo in main community; do
	[ -e "$cache/APKINDEX-$repo" ] || {
		curl -sSfL "$mirror/$branch/$repo/aarch64/APKINDEX.tar.gz" | tar -xzO APKINDEX 2>/dev/null > "$cache/APKINDEX-$repo"
	}
done
for pkg in "$@"; do
	[ -e "$dest/.pkg-$pkg" ] && continue
	for repo in main community; do
		ver=$(awk -v p="$pkg" 'BEGIN{RS=""; FS="\n"} { name=""; ver=""; for (i=1;i<=NF;i++) { if ($i ~ /^P:/) name=substr($i,3); if ($i ~ /^V:/) ver=substr($i,3) } if (name==p) { print ver; exit } }' "$cache/APKINDEX-$repo")
		[ -n "$ver" ] && break
	done
	[ -n "$ver" ] || { echo "package $pkg not found" >&2; exit 1; }
	file=$pkg-$ver.apk
	[ -e "$cache/$file" ] || curl -sSfL -o "$cache/$file" "$mirror/$branch/$repo/aarch64/$file"
	# An .apk is concatenated gzip streams: signature, control, then data.
	tar -xzf "$cache/$file" -C "$dest" --exclude='.PKGINFO' --exclude='.SIGN*' --exclude='.pre-*' --exclude='.post-*' --exclude='.trigger' 2>/dev/null
	touch "$dest/.pkg-$pkg"
	echo "installed $pkg-$ver"
done
