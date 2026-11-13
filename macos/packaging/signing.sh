# How bundle.sh signs, as functions (sourced by bundle.sh; tested on any
# system by packaging/test-signing.sh with the macOS tools stubbed).
#
#   SIGNING_IDENTITY  a Developer ID Application identity in a keychain
#                     (unset, empty or "-": signed ad hoc, as always)
#   NOTARY_PROFILE    an `xcrun notarytool store-credentials` profile name:
#                     the zip is notarized and the app stapled (needs
#                     SIGNING_IDENTITY)
#   NOTARY_KEYCHAIN   the keychain that profile is in, if not the default
#
# SIGNING.md says how the release workflow sets them.

# adhoc | developer-id | notarize, or exit status 1 with the reason on stderr.
signing_mode() {
    sm_id=${SIGNING_IDENTITY:-}
    [ "$sm_id" = "-" ] && sm_id=
    sm_profile=${NOTARY_PROFILE:-}
    if [ -z "$sm_id" ] && [ -n "$sm_profile" ]; then
        echo "NOTARY_PROFILE is set but SIGNING_IDENTITY is missing: notarizing needs a Developer ID signature" >&2
        return 1
    fi
    if [ -z "$sm_id" ]; then
        echo adhoc
    elif [ -z "$sm_profile" ]; then
        echo developer-id
    else
        echo notarize
    fi
}

# sign_app MODE APP
sign_app() {
    if [ "$1" = adhoc ]; then
        # Apple silicon runs only signed code; the signature also seals
        # Info.plist, whose microphone and network texts macOS shows when it
        # asks.
        codesign --force --sign - --entitlements packaging/entitlements.plist "$2"
    else
        # The hardened runtime (notarization requires it) with a secure
        # timestamp; the entitlements keep the inputs working under it.
        codesign --force --sign "$SIGNING_IDENTITY" --options runtime --timestamp \
            --entitlements packaging/entitlements.plist "$2"
    fi
}

# notarize_app APP ZIP: submit the zip, staple the ticket to the app, and
# zip the stapled app again (the ticket must be inside the release file).
notarize_app() {
    if [ -n "${NOTARY_KEYCHAIN:-}" ]; then
        xcrun notarytool submit "$2" --keychain-profile "$NOTARY_PROFILE" --keychain "$NOTARY_KEYCHAIN" --wait
    else
        xcrun notarytool submit "$2" --keychain-profile "$NOTARY_PROFILE" --wait
    fi
    xcrun stapler staple "$1"
    rm -f "$2"
    ditto -c -k --keepParent "$1" "$2"
    spctl --assess --type execute -vv "$1"
}
