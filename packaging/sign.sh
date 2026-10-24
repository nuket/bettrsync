#!/usr/bin/env bash
# bettrsync
# Copyright (c) 2026 Max Vilimpoc
# SPDX-License-Identifier: MIT
#
# Signs the release files in a directory with the bettrsync release
# key, the one whose public half is packaging/bettrsync-signing-key.asc.
# Every .rpm gets its signature embedded (when rpmsign is here); every
# file gets a detached signature beside it, .sig (binary) for the Arch
# package since that is the name pacman looks for, .asc for the rest;
# then SHA256SUMS is written over the files and clearsigned as
# SHA256SUMS.asc, and the public key is copied in as
# bettrsync-signing-key.asc. Everything is verified against that
# public key alone before this script ends.
#
# The private key comes from GPG_SIGNING_KEY (the armored key, as the
# GitHub secret holds it), imported into a keyring that is thrown away
# afterwards, or else from the keyring GnuPG would use anyway: point
# GNUPGHOME at the keystore. GPG_PASSPHRASE, when set, unlocks it
# without a prompt.
#
# usage: packaging/sign.sh DIR

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIR=$(realpath "$1")
PUB="$ROOT/packaging/bettrsync-signing-key.asc"
[ -f "$PUB" ] || { echo "no public key at $PUB: make the key first (README.md, Signed releases)" >&2; exit 1; }
FPR=$(gpg --batch --show-keys --with-colons "$PUB" | awk -F: '$1=="fpr"{print $10; exit}')
[ -n "$FPR" ] || { echo "$PUB holds no key" >&2; exit 1; }

work=$(mktemp -d)
homes=()
cleanup() {
    for h in "${homes[@]}"; do gpgconf --homedir "$h" --kill gpg-agent 2>/dev/null || true; done
    rm -rf "$work"
}
trap cleanup EXIT

if [ -n "${GPG_SIGNING_KEY:-}" ]; then
    export GNUPGHOME="$work/gnupg"
    mkdir -m 700 "$GNUPGHOME"
    homes+=("$GNUPGHOME")
    printf '%s\n' "$GPG_SIGNING_KEY" | gpg --batch --quiet --import
fi
gpg --batch --list-secret-keys "$FPR" >/dev/null 2>&1 \
    || { echo "the private half of $FPR is not in this keyring (GNUPGHOME=${GNUPGHOME:-unset})" >&2; exit 1; }

sign=(gpg --batch --yes --local-user "$FPR")
rpm_extra=""
if [ -n "${GPG_PASSPHRASE:-}" ]; then
    (umask 077; printf '%s\n' "$GPG_PASSPHRASE" > "$work/pass")
    sign+=(--pinentry-mode loopback --passphrase-file "$work/pass")
    rpm_extra="--batch --pinentry-mode loopback --passphrase-file $work/pass"
fi

cd "$DIR"
rm -f ./*.sig ./*.asc SHA256SUMS
assets=()
for f in *; do [ -f "$f" ] && assets+=("$f"); done
[ ${#assets[@]} -gt 0 ] || { echo "nothing in $DIR" >&2; exit 1; }

# The .rpm files carry their signature inside, as rpm expects.
rpms=()
for f in *.rpm; do [ -f "$f" ] && rpms+=("$f"); done
embedded=0
if [ ${#rpms[@]} -gt 0 ]; then
    if command -v rpmsign >/dev/null; then
        # Debian's rpm macros name gpg2, which need not exist: say which gpg.
        defs=(--define "__gpg $(command -v gpg)" --define "_gpg_name $FPR")
        [ -n "${GNUPGHOME:-}" ] && defs+=(--define "_gpg_path $GNUPGHOME")
        [ -n "$rpm_extra" ] && defs+=(--define "_gpg_sign_cmd_extra_args $rpm_extra")
        rpmsign --addsign "${defs[@]}" "${rpms[@]}" >/dev/null
        embedded=1
    else
        echo "rpmsign is not here (the rpm package has it): the .rpm files get a detached signature only" >&2
    fi
fi

sha256sum "${assets[@]}" > SHA256SUMS
for f in "${assets[@]}"; do
    case "$f" in
        *.pkg.tar.zst) "${sign[@]}" --detach-sign --output "$f.sig" "$f" ;;
        *) "${sign[@]}" --detach-sign --armor --output "$f.asc" "$f" ;;
    esac
done
"${sign[@]}" --clearsign --output SHA256SUMS.asc SHA256SUMS
cp "$PUB" bettrsync-signing-key.asc

# Checked the way a user would: with nothing but the public key file.
vhome="$work/verify"
mkdir -m 700 "$vhome"
homes+=("$vhome")
gpg --homedir "$vhome" --batch --quiet --import "$PUB"
for f in "${assets[@]}"; do
    case "$f" in *.pkg.tar.zst) s="$f.sig" ;; *) s="$f.asc" ;; esac
    gpg --homedir "$vhome" --batch --quiet --verify "$s" "$f" 2>/dev/null || { echo "$s does not verify" >&2; exit 1; }
done
gpg --homedir "$vhome" --batch --quiet --verify SHA256SUMS.asc 2>/dev/null || { echo "SHA256SUMS.asc does not verify" >&2; exit 1; }
if [ "$embedded" = 1 ]; then
    rpm --dbpath "$work/rpmdb" --import "$PUB"
    for f in "${rpms[@]}"; do
        rpm --dbpath "$work/rpmdb" -K "$f" | grep -q 'signatures OK' || { echo "$f: its embedded signature does not verify" >&2; exit 1; }
    done
fi
echo "signed with $FPR:"
for f in "${assets[@]}"; do case "$f" in *.pkg.tar.zst) echo "$f.sig" ;; *) echo "$f.asc" ;; esac; done
echo SHA256SUMS SHA256SUMS.asc bettrsync-signing-key.asc | tr ' ' '\n'
