#!/bin/sh
# Builds BananaSplit's release packages into dist/ and checks them:
#
#   bananasplit_<version>_amd64.deb              Debian, Ubuntu, Mint, Pop!_OS
#   bananasplit-<version>-linux-x86_64.tar.gz    any distribution (./install.sh)
#
#   packaging/build-packages.sh
#
# Needs: cargo, python3, binutils (strip, readelf), dpkg-deb, tar, gzip.
# The binary is made to start on glibc 2.34 and newer (packaging/glibc_compat.py)
# whatever glibc it is built on.
set -eu
umask 022
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
VER=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$VER" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
[ "$(uname -m)" = x86_64 ] || { echo "the packages are built for x86-64, on x86-64" >&2; exit 1; }
for t in cargo python3 strip readelf dpkg-deb tar gzip; do
  command -v $t >/dev/null || { echo "build-packages.sh needs $t" >&2; exit 1; }
done
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}"
STAMP=$(date -u -d "@$SOURCE_DATE_EPOCH" +%Y%m%d%H%M.%S)
DIST=$ROOT/dist
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
fail() { echo "build-packages.sh: FAIL: $*" >&2; exit 1; }
mkdir -p "$DIST"

# ---- the binary ---------------------------------------------------------------------
cargo build --release --locked
BIN=$WORK/bananasplit
cp target/release/bananasplit "$BIN"
strip --strip-all "$BIN"
python3 packaging/glibc_compat.py "$BIN" --max 2.34
NEWEST=$(readelf -V "$BIN" | grep -o 'GLIBC_2\.[0-9.]*' | sed 's/GLIBC_//' | sort -V | tail -n 1)
[ "$(printf '%s\n2.34\n' "$NEWEST" | sort -V | tail -n 1)" = 2.34 ] || fail "the binary needs glibc $NEWEST"
[ "$("$BIN" --version)" = "BananaSplit $VER" ] || fail "--version: $("$BIN" --version)"
chmod 0755 "$BIN"
echo "binary: $(stat -c %s "$BIN") bytes, needs glibc >= $NEWEST"

touch_all() { find "$1" -exec touch -h -t "$STAMP" {} +; }

# ---- .deb ---------------------------------------------------------------------------
D=$WORK/deb
DEB=$DIST/bananasplit_${VER}_amd64.deb
install -d -m 0755 "$D" "$D/DEBIAN"
install -D -m 0755 "$BIN" "$D/usr/bin/bananasplit"
install -D -m 0644 contrib/bananasplit.desktop "$D/usr/share/applications/bananasplit.desktop"
install -D -m 0644 assets/icons/bananasplit.svg "$D/usr/share/icons/hicolor/scalable/apps/bananasplit.svg"
for s in 16 24 32 48 64 128 256; do
  install -D -m 0644 "assets/icons/bananasplit-$s.png" "$D/usr/share/icons/hicolor/${s}x${s}/apps/bananasplit.png"
done
DOC=$D/usr/share/doc/bananasplit
install -D -m 0644 README.md "$DOC/README.md"
gzip -9n "$DOC/README.md"
sed 's|%h/.local/bin/bananasplit|/usr/bin/bananasplit|g; /^# Installed from the .deb/d; /^# (both lines)/d' \
  contrib/systemd/bananasplit.service > "$WORK/bananasplit.service"
grep -q '%h/.local' "$WORK/bananasplit.service" && fail "service example still points to ~/.local"
install -D -m 0644 "$WORK/bananasplit.service" "$DOC/examples/bananasplit.service"
{
  echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
  echo "Upstream-Name: BananaSplit"
  echo "Source: the bananasplit-$VER source archive"
  echo
  echo "Files: *"
  echo "Copyright: 2026 the BananaSplit authors"
  echo "License: MIT"
  echo
  echo "License: MIT"
  sed 's/^$/./; s/^/ /' LICENSE | sed '1,4d'
} > "$DOC/copyright"
chmod 0644 "$DOC/copyright"
{
  echo "bananasplit ($VER) stable; urgency=medium"
  echo
  echo "  * New studio interface: input gain, pan/balance, segmented meters with"
  echo "    peak hold, quick mix, session panel; dark and light themes, zoom."
  echo "  * Desktop app: its own window (GTK 3 + WebKitGTK), tray icon, closing"
  echo "    keeps it running, start with the system, app-menu entry and icon."
  echo "  * Player for recordings, into the buses of your choice; scenes."
  echo "  * Uses PipeWire's libjack by itself when the system's finds no JACK"
  echo "    server: no pw-jack needed."
  echo
  echo " -- BananaSplit developers <noreply@bananasplit.invalid>  $(date -u -R -d "@$SOURCE_DATE_EPOCH")"
} > "$WORK/changelog"
gzip -9n -c "$WORK/changelog" > "$DOC/changelog.gz"
chmod 0644 "$DOC/changelog.gz"
SIZE=$(du -k -s --apparent-size "$D/usr" | cut -f1)
cat > "$D/DEBIAN/control" <<EOF
Package: bananasplit
Version: $VER
Section: sound
Priority: optional
Architecture: amd64
Maintainer: BananaSplit developers <noreply@bananasplit.invalid>
Installed-Size: $SIZE
Depends: libc6 (>= 2.34), pipewire-jack | pipewire-audio-client-libraries | libjack-jackd2-0 | libjack0
Recommends: libwebkit2gtk-4.1-0 | libwebkit2gtk-4.0-37, pulseaudio-utils, xdg-utils
Description: virtual audio mixer for PipeWire and JACK, like Voicemeeter Banana
 BananaSplit mixes and routes audio between applications and devices: six
 input strips with input gain, pan, a 6-band EQ, mute, solo and mono; five
 buses that play on speakers and headphones, become virtual microphones that
 other applications record from, or both, and can feed each other; a virtual
 cable for every channel; a menu on every channel to pick devices and apps;
 a multitrack recorder and player; scenes; and VBAN network audio that works
 with Voicemeeter.
 .
 It runs as a desktop app with its own window and a tray icon, and keeps
 mixing when the window is closed. The audio engine is a real-time JACK
 client that runs on PipeWire's JACK layer or on JACK2.
EOF
chmod 0644 "$D/DEBIAN/control"
touch_all "$D"
rm -f "$DEB"
dpkg-deb --root-owner-group -Zxz --build "$D" "$DEB" >/dev/null

# ---- portable package -----------------------------------------------------------------
NAME=bananasplit-$VER-linux-x86_64
P=$WORK/$NAME
TGZ=$DIST/$NAME.tar.gz
install -d -m 0755 "$P" "$P/contrib/systemd"
install -m 0755 "$BIN" "$P/bananasplit"
install -m 0755 packaging/install.sh "$P/install.sh"
install -m 0644 README.md LICENSE "$P/"
install -m 0644 contrib/systemd/bananasplit.service "$P/contrib/systemd/"
touch_all "$P"
tar --sort=name --owner=0 --group=0 --numeric-owner --format=gnu -C "$WORK" -cf - "$NAME" | gzip -9n > "$TGZ"

# ---- checks ---------------------------------------------------------------------------
echo "checking $DEB"
INFO=$(dpkg-deb -I "$DEB")
for f in "Package: bananasplit" "Version: $VER" "Architecture: amd64" "Depends: libc6 (>= 2.34)"; do
  echo "$INFO" | grep -qF " $f" || fail ".deb control lacks '$f'"
done
LIST=$(dpkg-deb -c "$DEB")
for f in ./usr/bin/bananasplit ./usr/share/applications/bananasplit.desktop \
         ./usr/share/icons/hicolor/scalable/apps/bananasplit.svg ./usr/share/icons/hicolor/256x256/apps/bananasplit.png \
         ./usr/share/doc/bananasplit/copyright ./usr/share/doc/bananasplit/changelog.gz; do
  echo "$LIST" | grep -q " $f\$" || fail ".deb lacks $f"
done
echo "$LIST" | grep -v ' root/root ' && fail ".deb has files not owned by root"
echo "$LIST" | grep -q '^-rwxr-xr-x .* \./usr/bin/bananasplit$' || fail "/usr/bin/bananasplit is not 0755"
echo "$LIST" | awk '$1 ~ /^d/ && $1 != "drwxr-xr-x" { bad = 1 } END { exit bad }' || fail ".deb has directories that are not 0755"
X=$WORK/x-deb
dpkg-deb -x "$DEB" "$X"
cmp -s "$X/usr/bin/bananasplit" "$BIN" || fail ".deb binary differs"
"$X/usr/bin/bananasplit" --version >/dev/null || fail ".deb binary does not run"
python3 - "$X/usr/share/applications/bananasplit.desktop" <<'EOF' || fail "desktop entry"
import configparser, sys
c = configparser.RawConfigParser(strict=True, delimiters=("=",))
c.optionxform = str
c.read(sys.argv[1], encoding="utf-8")
e = c["Desktop Entry"]
assert e["Type"] == "Application" and e["Name"] == "BananaSplit" and e["Icon"] == "bananasplit"
assert e["Exec"] == "bananasplit --gui" and e["TryExec"] == "bananasplit", e["Exec"]
assert e["StartupWMClass"] == "bananasplit" and e["Terminal"] == "false"
assert e["Categories"].endswith(";") and "AudioVideo" in e["Categories"].split(";")
assert e["Actions"] == "quit;" and c["Desktop Action quit"]["Exec"] == "bananasplit --quit"
EOF
if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$X/usr/share/applications/bananasplit.desktop" || fail "desktop-file-validate"
fi

echo "checking $TGZ"
T=$WORK/x-tgz
mkdir -p "$T/home"
tar -xzf "$TGZ" -C "$T"
for f in bananasplit install.sh README.md LICENSE contrib/systemd/bananasplit.service; do
  [ -f "$T/$NAME/$f" ] || fail "portable package lacks $f"
done
[ -x "$T/$NAME/bananasplit" ] && [ -x "$T/$NAME/install.sh" ] || fail "portable package lost its exec bits"
# install, update, uninstall in a throw-away home; foreign files stay untouched
H=$T/home
inst() { env -u XDG_DATA_HOME -u XDG_CONFIG_HOME -u XDG_DATA_DIRS HOME="$H" PATH=/usr/bin:/bin "$@"; }
APPS=$H/.local/share/applications
mkdir -p "$APPS" "$H/.config/bananasplit" "$H/.config/autostart"
echo "[Desktop Entry]" > "$APPS/other.desktop"
echo "keep" > "$H/.config/bananasplit/bananasplit.ini"
printf '[Desktop Entry]\nName=Mine\nExec=/bin/true\n' > "$H/.config/autostart/other.desktop"
# 2.5's contrib entry, copied by hand: replaced
printf '[Desktop Entry]\nType=Application\nName=BananaSplit\nComment=6-strip virtual audio mixer and recorder\nExec=pw-jack bananasplit --gui\nIcon=audio-card\n' > "$APPS/bananasplit.desktop"
inst "$T/$NAME/install.sh" > "$T/install.txt" 2>&1 || { cat "$T/install.txt"; fail "install.sh"; }
grep -q "is in the app menu" "$T/install.txt" || fail "install.sh said: $(cat "$T/install.txt")"
cmp -s "$H/.local/bin/bananasplit" "$BIN" || fail "install.sh did not copy the program"
[ -x "$H/.local/bin/bananasplit" ] || fail "installed program is not executable"
grep -qx "Exec=$H/.local/bin/bananasplit --gui" "$APPS/bananasplit.desktop" || fail "menu entry Exec"
grep -qx "TryExec=$H/.local/bin/bananasplit" "$APPS/bananasplit.desktop" || fail "menu entry TryExec"
grep -qx "Icon=bananasplit" "$APPS/bananasplit.desktop" || fail "menu entry Icon"
grep -qx "Exec=$H/.local/bin/bananasplit --quit" "$APPS/bananasplit.desktop" || fail "menu entry quit action"
[ "$(find "$H/.local/share/icons/hicolor" -name 'bananasplit.*' | wc -l)" -eq 8 ] || fail "icons not installed"
inst "$H/.local/bin/bananasplit" --install > /dev/null || fail "installing again (update)"
# someone else's entry under our name: refused and left alone
mv "$APPS/bananasplit.desktop" "$T/ours.desktop"
printf '[Desktop Entry]\nName=Someone else\n' > "$APPS/bananasplit.desktop"
inst "$H/.local/bin/bananasplit" --install > "$T/refused.txt" 2>&1 && fail "--install replaced a foreign menu entry"
grep -q "was not made by BananaSplit" "$T/refused.txt" || fail "refusal message: $(cat "$T/refused.txt")"
grep -q "Someone else" "$APPS/bananasplit.desktop" || fail "foreign entry changed"
inst "$H/.local/bin/bananasplit" --uninstall > /dev/null || fail "--uninstall with a foreign entry"
grep -q "Someone else" "$APPS/bananasplit.desktop" || fail "--uninstall removed a foreign entry"
[ -x "$H/.local/bin/bananasplit" ] || fail "--uninstall removed the program without its entry"
mv "$T/ours.desktop" "$APPS/bananasplit.desktop"
inst "$T/$NAME/install.sh" --uninstall > "$T/uninstall.txt" 2>&1 || { cat "$T/uninstall.txt"; fail "install.sh --uninstall"; }
[ ! -e "$APPS/bananasplit.desktop" ] || fail "menu entry not removed"
[ ! -e "$H/.local/bin/bananasplit" ] || fail "program not removed"
[ -z "$(find "$H/.local/share/icons" -name 'bananasplit.*')" ] || fail "icons not removed"
[ -f "$APPS/other.desktop" ] && [ -f "$H/.config/autostart/other.desktop" ] || fail "foreign files touched"
[ "$(cat "$H/.config/bananasplit/bananasplit.ini")" = keep ] || fail "settings touched"
inst "$T/$NAME/bananasplit" --uninstall | grep -q "nothing to remove" || fail "second --uninstall"
# an entry of ours that starts a program elsewhere: the user's own ~/.local/bin copy stays
inst "$T/$NAME/install.sh" > /dev/null 2>&1 || fail "install.sh (again)"
sed -i "s|^Exec=$H/.local/bin/bananasplit --gui\$|Exec=/opt/elsewhere/bananasplit --gui|" "$APPS/bananasplit.desktop"
inst "$T/$NAME/bananasplit" --uninstall > /dev/null || fail "--uninstall (elsewhere)"
[ -x "$H/.local/bin/bananasplit" ] || fail "--uninstall removed a ~/.local/bin program its entry does not start"

echo
for f in "$DEB" "$TGZ"; do
  printf '%s  %s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$(basename "$f")"
done
echo "PACKAGES PASS: dist/$(basename "$DEB"), dist/$(basename "$TGZ")"
