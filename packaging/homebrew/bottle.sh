#!/bin/sh
# Makes this machine's Homebrew bottle of a release, before the release is
# published:
#   packaging/homebrew/bottle.sh 0.3.3 <dir with pitupitu-*.tar.gz and .sha256> <out dir>
# Fills the formula from the tarballs (fill.sh), installs it from a tap of
# its own with --build-bottle, the tarball for this machine put where
# Homebrew would download it to, and bottles it. Leaves in <out dir>
# `pitupitu-<version>.<tag>.bottle.tar.gz`, named as Homebrew downloads it
# from the release, and `pitupitu-<version>.<tag>.bottle.line`, the
# bottle's line for fill.sh. Then pours the bottle in place of what it
# installed, to show Homebrew takes it.
#
# For a CI runner only: it installs and uninstalls pitupitu with this
# machine's Homebrew.
set -eu

version="$1"
dir="$(cd "$2" && pwd)"
out="$(mkdir -p "$3" && cd "$3" && pwd)"
here="$(cd "$(dirname "$0")" && pwd)"

if [ -z "${GITHUB_ACTIONS:-}" ]; then
  echo "bottle.sh installs and uninstalls pitupitu with this machine's Homebrew; it runs on CI only" >&2
  exit 1
fi

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64) target=aarch64-unknown-linux-musl ;;
  *) echo "no release tarball for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_FROM_API=1 HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_ENV_HINTS=1
tap=klarluft/tap
formula="$tap/pitupitu"

# The formula without bottles, from the checksums alone.
sums="$(mktemp -d)"
trap 'rm -rf "$sums"' EXIT
cp "$dir"/pitupitu-*.tar.gz.sha256 "$sums/"
brew tap-new --no-git "$tap" >/dev/null
brew trust "$tap"
"$here/fill.sh" "$version" "$sums" > "$(brew --repository "$tap")/Formula/pitupitu.rb"

# Not published yet: the tarball goes where Homebrew would download it to,
# and Homebrew checks it against the formula's checksum all the same.
cache="$(brew --cache --formula "$formula")"
mkdir -p "$(dirname "$cache")"
cp "$dir/pitupitu-$target.tar.gz" "$cache"

brew install --build-bottle "$formula"
(cd "$out" && brew bottle --json --no-rebuild \
  --root-url="https://github.com/klarluft/pitupitu/releases/download/v$version" "$formula") > "$sums/bottle.out"
cat "$sums/bottle.out"

# `sha256 cellar: :any_skip_relocation, arm64_sequoia: "…"`, as brew said it.
line="$(sed -n 's/^ *sha256 \(cellar: .*\)$/\1/p' "$sums/bottle.out")"
tag="$(echo "$line" | sed -n 's/^cellar: [^,]*, \([a-z0-9_]*\): .*/\1/p')"
[ -n "$tag" ] || { echo "no bottle line in brew's output" >&2; exit 1; }
mv "$out/pitupitu--$version.$tag.bottle.tar.gz" "$out/pitupitu-$version.$tag.bottle.tar.gz"
rm -f "$out/pitupitu--$version.$tag.bottle.json"
printf '%s' "$line" > "$out/pitupitu-$version.$tag.bottle.line"

# Pour it: the same formula with this bottle, from <out dir>.
brew uninstall "$formula"
cp "$out/pitupitu-$version.$tag.bottle.line" "$sums/"
"$here/fill.sh" "$version" "$sums" \
  | sed "s|^    root_url .*|    root_url \"file://$out\"|" > "$(brew --repository "$tap")/Formula/pitupitu.rb"
brew install "$formula"
poured="$(brew info --json=v2 "$formula" | grep -c '"poured_from_bottle": *true' || true)"
[ "$poured" -ge 1 ] || { echo "Homebrew did not pour the bottle" >&2; exit 1; }
"$(brew --prefix)/bin/pitupitu" --version
# The signature macOS keys the person's privacy answers on came through.
if [ "$(uname -s)" = Darwin ]; then
  codesign --verify --deep --strict "$(brew --prefix "$formula")/pitupitu.app"
fi
echo "bottled pitupitu $version for $tag"
