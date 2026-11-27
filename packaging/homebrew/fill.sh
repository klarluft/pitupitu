#!/bin/sh
# Fills in the Homebrew formula template from a release's checksums:
#   packaging/homebrew/fill.sh 0.3.3 <dir with pitupitu-*.tar.gz.sha256> > pitupitu.rb
# Fails on a missing or malformed checksum, so a release never carries a
# half-filled formula. The bottles `bottle.sh` made, one
# `pitupitu-<version>.<tag>.bottle.line` each in the same dir, go in the
# bottle block; with none (`bottle.sh` itself fills the formula it bottles)
# there is no block.
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

# Homebrew's order: Apple silicon, Intel Macs, then Linux.
rank() {
  case "$1" in
    arm64_linux) echo 3 ;;
    x86_64_linux) echo 4 ;;
    arm64_*) echo 1 ;;
    *) echo 2 ;;
  esac
}
lines=""
for f in "$dir"/pitupitu-"$version".*.bottle.line; do
  [ -e "$f" ] || continue
  tag="${f##*/pitupitu-"$version".}"
  tag="${tag%.bottle.line}"
  line="$(cat "$f")"
  if ! echo "$line" | grep -Eq "^cellar: [^,]+, $tag: \"[0-9a-f]{64}\"\$"; then
    echo "no valid bottle line for $tag" >&2
    exit 1
  fi
  lines="$lines$(rank "$tag") $line
"
done
block="$(mktemp)"
trap 'rm -f "$block"' EXIT
if [ -n "$lines" ]; then
  {
    echo "  bottle do"
    echo "    root_url \"https://github.com/klarluft/pitupitu/releases/download/v$version\""
    # The digests in one column, as `brew style` wants them.
    printf '%s' "$lines" | sort -n | cut -d' ' -f2- | awk '
      { i = index($0, " \""); head[NR] = substr($0, 1, i - 1); sum[NR] = substr($0, i + 1)
        if (i - 1 > w) w = i - 1 }
      END { for (n = 1; n <= NR; n++) printf "    sha256 %-*s %s\n", w, head[n], sum[n] }'
    echo "  end"
    echo
  } > "$block"
fi

# The bottle block, or nothing, for the `__BOTTLE__` line and the blank
# line after it.
out="$(sed "$script" "$template" | awk -v block="$block" '
  skip && /^$/ { skip = 0; next }
  /^  __BOTTLE__$/ { while ((getline l < block) > 0) print l; skip = 1; next }
  { skip = 0; print }
')"
if echo "$out" | grep -q '__[A-Z0-9_]*__'; then
  echo "unfilled placeholders left in the formula" >&2
  exit 1
fi
printf '%s\n' "$out"
