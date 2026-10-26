#!/bin/sh
# Signs a release's SHA256SUMS.txt for the in-app updater, which installs a
# release only when SHA256SUMS.txt.sig is the Ed25519 signature of it made
# with the release key (its public half is SIGNING_KEY in src/update.rs).
#
#   packaging/sign-release.sh --release v2.8.1   download the release's SHA256SUMS.txt
#                                                (gh), sign it, upload SHA256SUMS.txt.sig
#   packaging/sign-release.sh SHA256SUMS.txt     write SHA256SUMS.txt.sig next to it
#   packaging/sign-release.sh --new-key          make a release key (prints its public half)
#
# The key: $BANANASPLIT_SIGNING_KEY, or ~/.config/bananasplit-release/signing-key.pem.
# Keep it on your own computer and back it up. Never put it on GitHub (not
# as a secret either): the signature is what tells a real release from one
# made by someone who got into the GitHub account. Without the key, no new
# release can be installed by the updater (users install one by hand).
#
# Needs: openssl 1.1.1 or newer, od; gh for --release.
set -eu
umask 077
HERE=$(cd "$(dirname "$0")" && pwd)
KEY=${BANANASPLIT_SIGNING_KEY:-${XDG_CONFIG_HOME:-$HOME/.config}/bananasplit-release/signing-key.pem}
fail() { echo "sign-release.sh: $*" >&2; exit 1; }
command -v openssl >/dev/null || fail "needs openssl"

hex() { od -An -v -tx1 | tr -d ' \n'; }
public_key() { openssl pkey -in "$KEY" -pubout -outform DER | tail -c 32 | hex; }

if [ "${1:-}" = --new-key ]; then
  [ -e "$KEY" ] && fail "$KEY already exists (a new key would make every installed BananaSplit refuse new releases)"
  mkdir -p "$(dirname "$KEY")"
  openssl genpkey -algorithm ed25519 -out "$KEY"
  echo "made $KEY; back it up. Its public key, for SIGNING_KEY in src/update.rs:"
  public_key
  echo
  exit 0
fi

[ -f "$KEY" ] || fail "no release key at $KEY (set BANANASPLIT_SIGNING_KEY)"
PUB=$(public_key)
# the key the updater checks against, in this checkout
WANT=$(sed -n 's/^pub const SIGNING_KEY: &str = "\([0-9a-f]*\)";$/\1/p' "$HERE/../src/update.rs")
[ "$PUB" = "$WANT" ] || fail "$KEY is not the release key in src/update.rs ($PUB, the updater wants $WANT)"

sign() {
  sums=$1
  [ -s "$sums" ] || fail "$sums is empty or missing"
  openssl pkeyutl -sign -rawin -inkey "$KEY" -in "$sums" -out "$sums.sig.bin"
  openssl pkey -in "$KEY" -pubout -out "$sums.pub.pem"
  openssl pkeyutl -verify -rawin -pubin -inkey "$sums.pub.pem" -in "$sums" -sigfile "$sums.sig.bin" >/dev/null \
    || fail "the signature does not verify"
  { hex < "$sums.sig.bin"; echo; } > "$sums.sig"
  rm -f "$sums.sig.bin" "$sums.pub.pem"
  [ "$(tr -d '\n' < "$sums.sig" | wc -c)" -eq 128 ] || fail "unexpected signature length"
  echo "signed:"
  sed 's/^/  /' "$sums"
}

case "${1:-}" in
  --release)
    TAG=${2:-}
    [ -n "$TAG" ] || fail "--release needs a tag (v2.8.1)"
    command -v gh >/dev/null || fail "--release needs gh (GitHub's command line)"
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    gh release download "$TAG" --repo LudaLunizz/BananaSplit --pattern SHA256SUMS.txt --dir "$WORK"
    # the files the checksums are for are the ones the release has
    for f in $(awk '{ sub(/^\*/, "", $2); print $2 }' "$WORK/SHA256SUMS.txt"); do
      gh release view "$TAG" --repo LudaLunizz/BananaSplit --json assets --jq '.assets[].name' | grep -qx "$f" \
        || fail "SHA256SUMS.txt lists $f, which the release does not have"
    done
    sign "$WORK/SHA256SUMS.txt"
    gh release upload "$TAG" --repo LudaLunizz/BananaSplit --clobber "$WORK/SHA256SUMS.txt.sig"
    echo "uploaded SHA256SUMS.txt.sig to $TAG; untick pre-release when it is ready for everyone"
    ;;
  ""|-*)
    sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
  *)
    sign "$1"
    echo "wrote $1.sig: upload it to the release next to SHA256SUMS.txt"
    ;;
esac
