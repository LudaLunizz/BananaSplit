#!/bin/sh
# Builds BananaSplit.app, for Apple silicon and Intel Macs in one, and
# dist/bananasplit-<version>-macos.zip with it. Runs on a Mac with Rust
# (rustup) and Xcode's command line tools (xcode-select --install):
#
#   sh packaging/bundle.sh
#
# The app is signed ad hoc, unless SIGNING_IDENTITY names a Developer ID
# (Apple's paid developer program) and then also notarized when
# NOTARY_PROFILE is set: packaging/signing.sh, packaging/SIGNING.md.
# Ad hoc, README.md says how to open it the first time.
set -eu
cd "$(dirname "$0")/.."
. packaging/signing.sh
# (a profile without an identity stops here, before the long build)
mode=$(signing_mode)
echo "signing: $mode"

version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)
if [ -z "$version" ]; then
    echo "no version in Cargo.toml" >&2
    exit 1
fi
# Info.plist says macOS 11 or newer: build for that.
export MACOSX_DEPLOYMENT_TARGET=11.0

for t in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add "$t" >/dev/null 2>&1 || true
    cargo build --locked --release --target "$t"
done

app=dist/BananaSplit.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/bananasplit" \
    target/aarch64-apple-darwin/release/bananasplit \
    target/x86_64-apple-darwin/release/bananasplit
cp assets/icons/bananasplit.icns "$app/Contents/Resources/bananasplit.icns"
sed "s/@VERSION@/$version/g" packaging/Info.plist > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
sign_app "$mode" "$app"
codesign --verify --deep --strict "$app"
codesign -d --entitlements - "$app" 2>/dev/null | grep -q com.apple.security.device.audio-input || {
    echo "the app's signature lacks the audio-input entitlement" >&2
    exit 1
}
said=$(plutil -extract CFBundleShortVersionString raw -o - "$app/Contents/Info.plist")
if [ "$said" != "$version" ]; then
    echo "the app's Info.plist says version $said, Cargo.toml $version" >&2
    exit 1
fi
archs=$(lipo -archs "$app/Contents/MacOS/bananasplit")
for a in arm64 x86_64; do
    case " $archs " in
        *" $a "*) ;;
        *)
            echo "the app has no $a part (it has: $archs)" >&2
            exit 1
            ;;
    esac
done
echo "for $archs"

# The release file (ditto keeps what Finder needs of the bundle).
zip="dist/bananasplit-$version-macos.zip"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
if [ "$mode" = notarize ]; then
    notarize_app "$app" "$zip"
fi
echo "built $app and $zip ($mode)"
