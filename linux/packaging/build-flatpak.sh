#!/bin/sh
# Builds BananaSplit's Flatpak into dist/ and checks it:
#
#   bananasplit-<version>-x86_64.flatpak    install with
#                                           flatpak install --user bananasplit-<version>-x86_64.flatpak
#
#   packaging/build-flatpak.sh
#
# Needs: flatpak, flatpak-builder, and the runtime, SDK and Rust extension
# named in the manifest (installed from Flathub for this user when missing).
set -eu
umask 022
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
ID=io.github.LudaLunizz.BananaSplit
MANIFEST=packaging/flatpak/$ID.yml
VER=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$VER" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
for t in flatpak flatpak-builder; do
  command -v $t >/dev/null || { echo "build-flatpak.sh needs $t" >&2; exit 1; }
done
fail() { echo "build-flatpak.sh: FAIL: $*" >&2; exit 1; }
grep -q "<release version=\"$VER\"" packaging/flatpak/$ID.metainfo.xml || fail "the metainfo has no <release> for $VER"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
DIST=$ROOT/dist
mkdir -p "$DIST"
OUT=$DIST/bananasplit-$VER-x86_64.flatpak

flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install-deps-from=flathub --force-clean --disable-rofiles-fuse \
  --state-dir="$WORK/state" --repo="$WORK/repo" "$WORK/build" "$MANIFEST"
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
  "$WORK/repo" "$OUT" "$ID"

# ---- checks -------------------------------------------------------------------------
[ "$(flatpak build "$WORK/build" bananasplit --version)" = "BananaSplit $VER" ] \
  || fail "--version in the Flatpak"
F=$WORK/build/files
for f in bin/bananasplit share/applications/$ID.desktop share/metainfo/$ID.metainfo.xml \
         share/icons/hicolor/scalable/apps/$ID.svg share/icons/hicolor/256x256/apps/$ID.png; do
  [ -f "$F/$f" ] || fail "missing $f"
done
grep -qx "Icon=$ID" "$F/share/applications/$ID.desktop" || fail "the menu entry's icon is not $ID"
if command -v desktop-file-validate >/dev/null; then desktop-file-validate "$F/share/applications/$ID.desktop"; fi
if command -v appstreamcli >/dev/null; then appstreamcli validate --no-net --explain "$F/share/metainfo/$ID.metainfo.xml"; fi
echo "flatpak: $OUT ($(stat -c %s "$OUT") bytes)"
