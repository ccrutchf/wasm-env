#!/usr/bin/env bash
# Fetches Ubuntu 24.04's arm64 base system and, optionally, packages (with
# their dependencies, resolved like apt: Depends and Pre-Depends, the first
# available alternative, virtual packages through Provides) into
# build/ubuntu. The base tarball and every .deb are checked against SHA-256
# sums. Maintainer scripts are not run. Needs network the first time.
#
#   tests/ubuntu-fetch.sh [PACKAGE...]
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
tarball=ubuntu-base-24.04.5-base-arm64.tar.gz
sha256=a91d5a93010193712d346d761372b7c9db6dfcf093893161c64ca107f05914f2
base_url=https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release
mirror=http://ports.ubuntu.com/ubuntu-ports
cache=$root/build/ubuntu-cache
dest=$root/build/ubuntu
mkdir -p "$cache"

if [ ! -e "$dest/.complete" ]; then
	if [ ! -e "$cache/$tarball" ]; then
		curl -sSfL -o "$cache/$tarball.part" "$base_url/$tarball"
		mv "$cache/$tarball.part" "$cache/$tarball"
	fi
	echo "$sha256  $cache/$tarball" | sha256sum -c --quiet
	rm -rf "$dest"
	mkdir -p "$dest"
	tar -xzf "$cache/$tarball" -C "$dest" --exclude='./dev/*' 2>/dev/null
	touch "$dest/.complete"
fi
[ $# -gt 0 ] || exit 0

# Package indexes: noble-updates first (newer versions win), then noble.
indexes=()
for suite in noble-updates noble; do
	for comp in main universe; do
		f=$cache/Packages-$suite-$comp
		if [ ! -e "$f" ]; then
			curl -sSfL "$mirror/dists/$suite/$comp/binary-arm64/Packages.xz" | xz -d >"$f.part"
			mv "$f.part" "$f"
		fi
		indexes+=("$f")
	done
done

# Resolve: prints "filename sha256 package" for each package to install,
# skipping what the base system already has (its dpkg status).
awk -v want="$*" '
BEGIN { RS = ""; FS = "\n" }
function field(name,   i, v) {
	v = ""
	for (i = 1; i <= NF; i++) if (index($i, name ": ") == 1) v = substr($i, length(name) + 3)
	return v
}
FILENAME ~ /status$/ { if (field("Status") ~ / installed$/) installed[field("Package")] = 1; next }
{
	p = field("Package")
	if (p in file) next # the first index (noble-updates) wins
	file[p] = field("Filename"); sum[p] = field("SHA256")
	deps[p] = field("Pre-Depends") ", " field("Depends")
	n = split(field("Provides"), pr, ",")
	for (i = 1; i <= n; i++) { v = pr[i]; gsub(/\(.*\)/, "", v); gsub(/^ +| +$/, "", v); if (v != "" && !(v in provider)) provider[v] = p }
}
END {
	n = split(want, queue, " "); head = 1
	while (head <= n) {
		alts = queue[head++]
		m = split(alts, choice, "|")
		pick = ""
		for (j = 1; j <= m && pick == ""; j++) {
			d = choice[j]; gsub(/\(.*\)|\[.*\]|:any/, "", d); gsub(/^ +| +$/, "", d)
			if (d == "") continue
			if (d in installed || d in done) { pick = "-"; break }
			if (d in file) pick = d
			else if (d in provider) pick = (provider[d] in installed || provider[d] in done) ? "-" : provider[d]
		}
		if (pick == "" || pick == "-") continue
		done[pick] = 1
		print file[pick], sum[pick], pick
		k = split(deps[pick], ds, ",")
		for (i = 1; i <= k; i++) queue[++n] = ds[i]
	}
}' "$dest/var/lib/dpkg/status" "${indexes[@]}" | while read -r filename sum pkg; do
	[ -e "$dest/.pkg-$pkg" ] && continue
	deb=$cache/$(basename "$filename")
	if [ ! -e "$deb" ]; then
		curl -sSfL -o "$deb.part" "$mirror/$filename"
		mv "$deb.part" "$deb"
	fi
	echo "$sum  $deb" | sha256sum -c --quiet
	member=$(ar t "$deb" | grep '^data\.tar')
	case $member in
	*.zst) ar p "$deb" "$member" | zstd -dq | tar -x --keep-directory-symlink -C "$dest" ;;
	*.xz) ar p "$deb" "$member" | xz -d | tar -x --keep-directory-symlink -C "$dest" ;;
	*.gz) ar p "$deb" "$member" | tar -xz --keep-directory-symlink -C "$dest" ;;
	*) ar p "$deb" "$member" | tar -x --keep-directory-symlink -C "$dest" ;;
	esac
	touch "$dest/.pkg-$pkg"
	echo "installed $pkg"
done
