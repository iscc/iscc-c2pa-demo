#!/bin/bash
# Install the latest ISCC C2PA Demo release on macOS.
#
#   curl -fsSL https://c2pa-demo.iscc.codes/install-mac.sh | bash
#
# Downloads the universal disk image from the latest GitHub release, checks it against the
# release's SHA256SUMS, copies the app to /Applications (~/Applications when /Applications is not
# writable) and opens it. The builds are not notarized; macOS only checks files that carry the
# quarantine flag, which browsers set and curl does not, so the app starts without a warning.

set -euo pipefail

REPO="iscc/iscc-c2pa-demo"
BASE="https://github.com/$REPO/releases/latest/download"
APP="ISCC C2PA Demo.app"

# Print a message and exit with an error.
fail() {
  echo "error: $*" >&2
  exit 1
}

# Print the "sha256 name" line of the disk image in the release's SHA256SUMS.
dmg_entry() {
  curl -fsSL "$BASE/SHA256SUMS" | grep -E ' \*?[^ ]+-macos-universal\.dmg$' | head -n 1 ||
    fail "no macOS disk image in the latest release"
}

# Print the folder to install into: /Applications, or ~/Applications without write access there.
target_dir() {
  if [ -w /Applications ]; then
    echo /Applications
  else
    mkdir -p "$HOME/Applications"
    echo "$HOME/Applications"
  fi
}

# Detach the disk image if it is mounted and remove the temporary folder.
cleanup() {
  if [ -d "$TMP/mnt/$APP" ]; then hdiutil detach -quiet "$TMP/mnt" || true; fi
  rm -rf "$TMP"
}

main() {
  [ "$(uname -s)" = "Darwin" ] || fail "this installer is for macOS; see https://c2pa-demo.iscc.codes"

  local entry sum name dest
  entry=$(dmg_entry)
  sum=${entry%% *}
  name=${entry##* }
  name=${name#\*}

  TMP=$(mktemp -d)
  trap cleanup EXIT

  echo "Downloading $name"
  curl -fL --progress-bar "$BASE/$name" -o "$TMP/$name"
  [ "$(shasum -a 256 "$TMP/$name" | cut -d ' ' -f 1)" = "$sum" ] ||
    fail "checksum mismatch for $name"
  echo "Checksum OK"

  hdiutil attach -quiet -nobrowse -readonly -noautoopen -mountpoint "$TMP/mnt" "$TMP/$name"
  [ -d "$TMP/mnt/$APP" ] || fail "$APP not found in $name"

  dest=$(target_dir)
  rm -rf "${dest:?}/$APP"
  ditto "$TMP/mnt/$APP" "$dest/$APP"
  echo "Installed $dest/$APP"

  open "$dest/$APP"
}

main "$@"
