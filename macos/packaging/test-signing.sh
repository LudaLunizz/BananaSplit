#!/bin/sh
# Tests packaging/signing.sh on any system: the macOS tools (codesign,
# xcrun, ditto, spctl) are stubs that print what they were asked, and each
# case checks the mode and the commands it leads to.
#
#   sh packaging/test-signing.sh
set -u
cd "$(dirname "$0")/.."
stubs=$(mktemp -d)
trap 'rm -rf "$stubs"' EXIT
for tool in codesign xcrun ditto spctl; do
    printf '#!/bin/sh\necho "%s $*"\n' "$tool" > "$stubs/$tool"
    chmod +x "$stubs/$tool"
done
PATH="$stubs:$PATH"
. packaging/signing.sh

failed=0
check() { # name, expected, got
    if [ "$2" = "$3" ]; then
        echo "ok   $1"
    else
        echo "FAIL $1"
        echo "  expected: $2"
        echo "  got:      $3"
        failed=1
    fi
}

# run_case SIGNING_IDENTITY NOTARY_PROFILE: the mode (or "exit 1: reason"),
# then the commands signing and (if notarized) notarizing would run.
run_case() {
    (
        unset SIGNING_IDENTITY NOTARY_PROFILE NOTARY_KEYCHAIN
        [ -n "$1" ] && export SIGNING_IDENTITY="$1"
        [ -n "$2" ] && export NOTARY_PROFILE="$2"
        if ! mode=$(signing_mode 2>"$stubs/err"); then
            echo "exit 1: $(cat "$stubs/err")"
            exit 0
        fi
        echo "$mode"
        sign_app "$mode" dist/BananaSplit.app
        [ "$mode" = notarize ] && notarize_app dist/BananaSplit.app dist/b.zip
        true
    )
}

nl='
'
check "no variables: ad hoc, the same command as before" \
    "adhoc${nl}codesign --force --sign - --entitlements packaging/entitlements.plist dist/BananaSplit.app" \
    "$(run_case "" "")"
check "identity \"-\": still ad hoc" \
    "adhoc${nl}codesign --force --sign - --entitlements packaging/entitlements.plist dist/BananaSplit.app" \
    "$(run_case "-" "")"
check "identity only: Developer ID, hardened runtime, not notarized" \
    "developer-id${nl}codesign --force --sign Developer ID Application: X (TEAM1) --options runtime --timestamp --entitlements packaging/entitlements.plist dist/BananaSplit.app" \
    "$(run_case "Developer ID Application: X (TEAM1)" "")"
check "profile only: exit 1, naming SIGNING_IDENTITY" \
    "exit 1: NOTARY_PROFILE is set but SIGNING_IDENTITY is missing: notarizing needs a Developer ID signature" \
    "$(run_case "" "bs-notary")"
check "both: sign, notarize, staple, re-zip, assess" \
    "notarize${nl}codesign --force --sign Developer ID Application: X (TEAM1) --options runtime --timestamp --entitlements packaging/entitlements.plist dist/BananaSplit.app${nl}xcrun notarytool submit dist/b.zip --keychain-profile bs-notary --wait${nl}xcrun stapler staple dist/BananaSplit.app${nl}ditto -c -k --keepParent dist/BananaSplit.app dist/b.zip${nl}spctl --assess --type execute -vv dist/BananaSplit.app" \
    "$(run_case "Developer ID Application: X (TEAM1)" "bs-notary")"

exit $failed
