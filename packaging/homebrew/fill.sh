#!/bin/sh
# Fills in the Homebrew formula template from a release's checksums:
#   packaging/homebrew/fill.sh 0.3.3 <dir with pitupitu-*.tar.gz.sha256> > pitupitu.rb
# Fails on a missing or malformed checksum, so a release never carries a
# half-filled formula.
set -eu

version="$1"
dir="$2"
template="$(dirname "$0")/pitupitu.rb"

script="s/__VERSION__/$version/g"
for target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
  sum="$(cut -d' ' -f1 "$dir/pitupitu-$target.tar.gz.sha256")"
  if ! echo "$sum" | grep -Eq '^[0-9a-f]{64}$'; then
    echo "no valid checksum for $target" >&2
    exit 1
  fi
  key="__SHA256_$(echo "$target" | tr 'a-z-' 'A-Z_')__"
  script="$script;s/$key/$sum/g"
done

out="$(sed "$script" "$template")"
if echo "$out" | grep -q '__[A-Z0-9_]*__'; then
  echo "unfilled placeholders left in the formula" >&2
  exit 1
fi
printf '%s\n' "$out"
