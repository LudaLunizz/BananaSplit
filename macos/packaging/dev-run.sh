#!/bin/sh
# Development builds as an app, so macOS keeps the permissions you gave it
# (microphone, system audio recording) from one build to the next. Runs on
# a Mac:
#
#   sh packaging/dev-run.sh [bananasplit options]   (e.g. --status)
#
# It builds for this Mac, copies the program into target/dev/BananaSplit.app
# and signs that again only when the program, Info.plist or the icon
# changed: an unchanged app keeps its signature and so its permissions.
# macOS ties a permission to the signature; an ad hoc one changes with
# every changed program, so for grants that last across changes set
# DEV_SIGNING_IDENTITY to a signing identity of your own (an Apple
# Development certificate, or a self-signed one made in Keychain Access).
#
# Not a cargo runner on purpose: that would also wrap `cargo test` (and
# the CI runner's tests).
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)
cargo build --locked
app=target/dev/BananaSplit.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
stamp=target/dev/signed.sha256

# what the signature covers, before copying
new_plist=$(mktemp)
trap 'rm -f "$new_plist"' EXIT
sed "s/@VERSION@/$version/g" packaging/Info.plist > "$new_plist"
want=$(cat target/debug/bananasplit "$new_plist" assets/icons/bananasplit.icns packaging/entitlements.plist |
    shasum -a 256 | cut -d' ' -f1)

if [ ! -f "$stamp" ] || [ "$(cat "$stamp")" != "$want" ] || [ ! -x "$app/Contents/MacOS/bananasplit" ]; then
    cp target/debug/bananasplit "$app/Contents/MacOS/bananasplit"
    cp "$new_plist" "$app/Contents/Info.plist"
    cp assets/icons/bananasplit.icns "$app/Contents/Resources/bananasplit.icns"
    codesign --force --sign "${DEV_SIGNING_IDENTITY:--}" --entitlements packaging/entitlements.plist "$app"
    echo "$want" > "$stamp"
    echo "dev-run: signed $app again (something changed)"
fi
exec "$app/Contents/MacOS/bananasplit" "$@"
