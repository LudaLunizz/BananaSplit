# Signing and notarizing the macOS app

**Without the secrets below, nothing changes: the release app is signed ad hoc, as it always was, and people open it the first time with Open Anyway (README.md).**

With them, the release workflow (`.github/workflows/release.yml`, job "macOS app") signs the app with your Developer ID and the hardened runtime, sends it to Apple's notary service, staples the ticket to the app and zips it again. macOS then opens it without warnings, and the microphone permission survives updates.

It takes a paid Apple Developer Program membership (99 USD a year).

## The 5 secrets

Add them in GitHub: the repository › Settings › Secrets and variables › Actions › New repository secret.

| Secret | What it is | Where the value comes from |
|---|---|---|
| `MACOS_CERTIFICATE` | your **Developer ID Application** certificate with its private key, as a base64 text | developer.apple.com › Account › Certificates, IDs & Profiles › Certificates › + › "Developer ID Application". Install it (double-click), then in Keychain Access › My Certificates right-click it › Export… › `.p12` with a password. Then: `base64 -i DeveloperID.p12 \| pbcopy` and paste. |
| `MACOS_CERTIFICATE_PASSWORD` | the password you gave the `.p12` on export | you chose it |
| `APPLE_ID` | the Apple ID (e-mail address) of your developer account | your account |
| `APPLE_TEAM_ID` | your team's 10-character ID | developer.apple.com › Account › Membership details › Team ID |
| `APPLE_APP_PASSWORD` | an app-specific password for the notary service (not your Apple ID password) | account.apple.com › Sign-In and Security › App-Specific Passwords › + |

Delete the `.p12` file and clear the clipboard once the secret is saved.

## What the workflow does with them

When `MACOS_CERTIFICATE` is set, the macOS job:

1. Imports the certificate into a temporary keychain with a random password.
2. Finds the "Developer ID Application" identity and passes it to `bundle.sh` as `SIGNING_IDENTITY`.
3. Stores the notary credentials in that keychain as a profile (`NOTARY_PROFILE`, `NOTARY_KEYCHAIN`).
4. Runs `bundle.sh`, which signs, notarizes, staples, zips the stapled app and checks it with `spctl`.
5. Deletes the temporary keychain, even when a step failed.

Without the secret, those steps are skipped and `bundle.sh` signs ad hoc.

## On your own Mac

The same `bundle.sh`, with your identity and a profile you stored once:

```sh
xcrun notarytool store-credentials bs-notary --apple-id you@example.com --team-id ABCDE12345
SIGNING_IDENTITY="Developer ID Application: Your Name (ABCDE12345)" NOTARY_PROFILE=bs-notary sh packaging/bundle.sh
```

- With only `SIGNING_IDENTITY`, the app is signed with your Developer ID but not notarized.
- With only `NOTARY_PROFILE`, `bundle.sh` stops at once: notarizing needs the Developer ID signature.
- `sh packaging/test-signing.sh` checks which mode each combination picks, on any system.

For development builds that keep their permissions, see `packaging/dev-run.sh`.
