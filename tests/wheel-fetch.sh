#!/usr/bin/env bash
# Installs pinned Python wheels from PyPI (manylinux, arm64, CPython 3.12)
# into the Ubuntu root's /usr/local/lib/python3.12/dist-packages, checking
# each against PyPI's SHA-256. Like `pip install --no-deps`, without running
# pip. Needs network the first time.
#
#   tests/wheel-fetch.sh numpy==2.5.3 ...
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cache=$root/build/wheel-cache
site=$root/build/ubuntu/usr/local/lib/python3.12/dist-packages
mkdir -p "$cache" "$site"
for spec in "$@"; do
	name=${spec%%==*} version=${spec#*==}
	[ -e "$site/.installed-$name-$version" ] && continue
	read -r file sha url < <(curl -sSfL "https://pypi.org/pypi/$name/$version/json" | jq -r '
		[.urls[] | select(.filename | test("cp312-cp312-manylinux.*aarch64|py3-none-any"))][0]
		| "\(.filename) \(.digests.sha256) \(.url)"')
	[ "$file" != null ] || { echo "no arm64 cp312 wheel for $spec" >&2; exit 1; }
	if [ ! -e "$cache/$file" ]; then
		curl -sSfL -o "$cache/$file.part" "$url"
		mv "$cache/$file.part" "$cache/$file"
	fi
	echo "$sha  $cache/$file" | sha256sum -c --quiet
	unzip -qo "$cache/$file" -d "$site"
	touch "$site/.installed-$name-$version"
	echo "installed $file"
done
