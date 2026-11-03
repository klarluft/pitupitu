#!/bin/bash
# Packages the macOS binary for a release:
#   packaging/macos/package.sh <built binary> <out dir> <version>
# Puts `pitupitu` and `pitupitu.app` (the same binary as an app, which
# Homebrew installs) in <out dir>, both signed with the Developer ID when
# MACOS_SIGN_P12 (base64 PKCS#12) and MACOS_SIGN_P12_PASSWORD are set.
#
# macOS remembers a person's privacy answers (may it open Documents, …)
# per program: for a signed binary by its signature and path, for a signed
# app by its bundle identifier. Ad-hoc signed builds lose the answers on
# every update; Homebrew's folder changes with every version, which only
# the app's identifier survives. Without the secrets (a fork), the build is
# ad-hoc signed, as before; REQUIRE_SIGNING=1 (tag builds) refuses that.
set -euo pipefail

bin="$1"
out="$2"
version="$3"
id="chat.pitupitu.box"
identity="Developer ID Application: Klarluft B.V. (AJG932F2NZ)"

mkdir -p "$out"
cp "$bin" "$out/pitupitu"
app="$out/pitupitu.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$bin" "$app/Contents/MacOS/pitupitu"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>$id</string>
  <key>CFBundleName</key>
  <string>pitupitu</string>
  <key>CFBundleDisplayName</key>
  <string>pitupitu</string>
  <key>CFBundleExecutable</key>
  <string>pitupitu</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$version</string>
  <key>CFBundleVersion</key>
  <string>$version</string>
  <key>LSMinimumSystemVersion</key>
  <string>11.0</string>
  <key>LSUIElement</key>
  <true/>
</dict>
</plist>
PLIST

if [ -z "${MACOS_SIGN_P12:-}" ]; then
  if [ "${REQUIRE_SIGNING:-}" = 1 ]; then
    echo "MACOS_SIGN_P12 is not set: a release is never shipped unsigned" >&2
    exit 1
  fi
  echo "::warning::no signing key: ad-hoc signing only"
  codesign --force --sign - --identifier "$id" "$out/pitupitu"
  codesign --force --sign - "$app"
  exit 0
fi

# A keychain of its own, deleted at the end.
tmp="$(mktemp -d)"
keychain="$tmp/sign.keychain-db"
pass="$(openssl rand -hex 16)"
before="$(security list-keychains -d user | tr -d '"')"
cleanup() {
  # shellcheck disable=SC2086
  security list-keychains -d user -s $before >/dev/null 2>&1 || true
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
  rm -rf "$tmp"
}
trap cleanup EXIT
security create-keychain -p "$pass" "$keychain"
security set-keychain-settings -lut 3600 "$keychain"
security unlock-keychain -p "$pass" "$keychain"
printf %s "$MACOS_SIGN_P12" | base64 --decode > "$tmp/sign.p12"
security import "$tmp/sign.p12" -k "$keychain" -P "$MACOS_SIGN_P12_PASSWORD" -T /usr/bin/codesign >/dev/null
rm -f "$tmp/sign.p12"
curl -fsSL https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer -o "$tmp/g2.cer"
security import "$tmp/g2.cer" -k "$keychain" >/dev/null 2>&1 || true
security set-key-partition-list -S apple-tool:,apple: -s -k "$pass" "$keychain" >/dev/null
# shellcheck disable=SC2086
security list-keychains -d user -s "$keychain" $before

sign() {
  codesign --force --options runtime --timestamp --keychain "$keychain" --sign "$identity" "$@"
}
sign --identifier "$id" "$out/pitupitu"
sign "$app"

codesign --verify --strict --verbose=2 "$out/pitupitu"
codesign --verify --strict --deep --verbose=2 "$app"
for f in "$out/pitupitu" "$app"; do
  codesign -dv "$f" 2>&1 | grep -E '^(Identifier|TeamIdentifier)='
done
