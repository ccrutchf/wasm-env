#!/usr/bin/env bash
# Fetches Alpine Linux's arm64 minirootfs and, optionally, packages (with
# their dependencies, resolved like apk does) into build/alpine. The rootfs
# is checksum-verified; packages come over HTTPS from the same origin. Used
# by tests/alpine.sh and tests/python.sh; needs network the first time.
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
	if [ ! -e "$cache/$tarball" ]; then
		curl -sSfL -o "$cache/$tarball.part" "$mirror/$branch/releases/aarch64/$tarball"
		mv "$cache/$tarball.part" "$cache/$tarball"
	fi
	echo "$sha256  $cache/$tarball" | sha256sum -c --quiet
	rm -rf "$dest"
	mkdir -p "$dest"
	tar -xzf "$cache/$tarball" -C "$dest" 2>/dev/null
	touch "$dest/.complete"
fi
[ $# -gt 0 ] || exit 0

for repo in main community; do
	if [ ! -e "$cache/APKINDEX-$repo" ]; then
		curl -sSfL "$mirror/$branch/$repo/aarch64/APKINDEX.tar.gz" | tar -xzO APKINDEX 2>/dev/null >"$cache/APKINDEX-$repo.part"
		mv "$cache/APKINDEX-$repo.part" "$cache/APKINDEX-$repo"
	fi
done

# Resolve the requested packages and their dependencies (D: fields; so:,
# cmd: and pc: names through the providers' p: fields), breadth-first.
# Prints "repo name version" for each package to install.
resolve() {
	awk -v want="$*" '
	BEGIN { RS = ""; FS = "\n" }
	{
		repo = (FILENAME ~ /community/) ? "community" : "main"
		name = ""; ver = ""; deps = ""; prov = ""
		for (i = 1; i <= NF; i++) {
			if ($i ~ /^P:/) name = substr($i, 3)
			else if ($i ~ /^V:/) ver = substr($i, 3)
			else if ($i ~ /^D:/) deps = substr($i, 3)
			else if ($i ~ /^p:/) prov = substr($i, 3)
		}
		if (name in version) next # main wins over community
		version[name] = ver; where[name] = repo; depends[name] = deps
		n = split(prov, ps, " ")
		for (j = 1; j <= n; j++) { p = ps[j]; sub(/[<>=~].*/, "", p); if (!(p in provider)) provider[p] = name }
	}
	END {
		n = split(want, queue, " "); head = 1
		while (head <= n) {
			d = queue[head++]
			if (d ~ /^!/) continue
			sub(/[<>=~].*/, "", d)
			pkg = (d in version) ? d : provider[d]
			if (pkg == "" || (pkg in done)) continue
			done[pkg] = 1
			print where[pkg], pkg, version[pkg]
			m = split(depends[pkg], ds, " ")
			for (k = 1; k <= m; k++) queue[++n] = ds[k]
		}
	}' "$cache/APKINDEX-main" "$cache/APKINDEX-community"
}

resolve "$@" | while read -r repo pkg ver; do
	[ -e "$dest/.pkg-$pkg" ] && continue
	file=$pkg-$ver.apk
	if [ ! -e "$cache/$file" ]; then
		curl -sSfL -o "$cache/$file.part" "$mirror/$branch/$repo/aarch64/$file"
		mv "$cache/$file.part" "$cache/$file"
	fi
	# An .apk is concatenated gzip streams: signature, control, then data.
	tar -xzf "$cache/$file" -C "$dest" --exclude='.PKGINFO' --exclude='.SIGN*' --exclude='.pre-*' \
		--exclude='.post-*' --exclude='.trigger' 2>/dev/null
	touch "$dest/.pkg-$pkg"
	echo "installed $pkg-$ver"
done
