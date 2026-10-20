#!/bin/sh
# Installs the pitupitu box agent: curl -fsSL https://pitupitu.chat/install | sh
# Downloads the latest release for this machine into ~/.local/bin (or
# $PITUPITU_INSTALL_DIR), checks its SHA-256, installs herdr when it is
# missing, then runs `pitupitu setup` when there is a terminal to answer it.
set -eu

repo="klarluft/pitupitu"
dir="${PITUPITU_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
  Linux) os="unknown-linux-musl" ;;
  Darwin) os="apple-darwin" ;;
  *) echo "pitupitu: unsupported system $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch="x86_64" ;;
  arm64 | aarch64) arch="aarch64" ;;
  *) echo "pitupitu: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
name="pitupitu-$arch-$os"
url="https://github.com/$repo/releases/latest/download/$name.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "downloading $url"
curl -fsSL "$url" -o "$tmp/$name.tar.gz"
curl -fsSL "$url.sha256" -o "$tmp/$name.tar.gz.sha256"
(
  cd "$tmp"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$name.tar.gz.sha256" >/dev/null
  else
    shasum -a 256 -c "$name.tar.gz.sha256" >/dev/null
  fi
)
tar xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$dir"
install -m 755 "$tmp/$name/pitupitu" "$dir/pitupitu"
echo "installed $dir/pitupitu"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "add $dir to your PATH to run pitupitu from a shell" ;;
esac
# pitupitu runs the agents in herdr: installed with herdr's own installer
# when it is missing. A failure is said, not fatal; setup tries again.
herdr_dir="${HERDR_INSTALL_DIR:-$HOME/.local/bin}"
if ! command -v herdr >/dev/null 2>&1 && [ ! -x "$herdr_dir/herdr" ]; then
  echo "pitupitu runs your agents in herdr; installing it"
  if curl -fsSL https://herdr.dev/install.sh -o "$tmp/herdr-install.sh" &&
    sh "$tmp/herdr-install.sh"; then
    :
  else
    echo "could not install herdr; pitupitu setup will try again (or see https://herdr.dev)" >&2
  fi
fi

# `-r /dev/tty` is true even without a controlling terminal; opening it
# is the real test.
if (exec </dev/tty) 2>/dev/null; then
  "$dir/pitupitu" setup </dev/tty
else
  echo "next: pitupitu setup"
fi
