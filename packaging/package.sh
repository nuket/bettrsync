#!/usr/bin/env bash
# bettrsync
# Copyright (c) 2026 Max Vilimpoc
# SPDX-License-Identifier: MIT
# Packages a Linux build of bettrsync: a tar.gz, a .deb, and an .rpm when
# rpmbuild is there. Nothing but dpkg-deb and rpmbuild is needed: the
# control file and the spec are written here, from Cargo.toml's version,
# description and license.
#
# usage: packaging/package.sh BINARY ARCH OUTDIR
#   ARCH is Debian's name for it, amd64 or arm64.

set -euo pipefail
umask 022

BIN=$(realpath "$1")
ARCH=$2
OUT=$(realpath -m "$3")
ROOT=$(cd "$(dirname "$0")/.." && pwd)

field() { sed -n "s/^$1 *= *\"\(.*\)\"/\1/p" "$ROOT/Cargo.toml" | head -1; }
VERSION=$(field version)
DESCRIPTION=$(field description)
LICENSE=$(field license)
[ -n "$VERSION" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
case "$ARCH" in
    amd64) RPM_ARCH=x86_64; TAR_ARCH=x86_64 ;;
    arm64) RPM_ARCH=aarch64; TAR_ARCH=aarch64 ;;
    *) echo "ARCH should be amd64 or arm64, not $ARCH" >&2; exit 1 ;;
esac

mkdir -p "$OUT"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The tarball: the binary and the README in a directory named like it.
name="bettrsync-$VERSION-linux-$TAR_ARCH"
mkdir -p "$work/$name"
install -m 755 "$BIN" "$work/$name/bettrsync"
install -m 644 "$ROOT/README.md" "$work/$name/README.md"
[ -f "$ROOT/LICENSE" ] && install -m 644 "$ROOT/LICENSE" "$work/$name/LICENSE"
tar -C "$work" -czf "$OUT/$name.tar.gz" "$name"
echo "$OUT/$name.tar.gz"

# The .deb: /usr/bin/bettrsync and its doc directory.
deb="$work/deb"
mkdir -p "$deb/DEBIAN" "$deb/usr/bin" "$deb/usr/share/doc/bettrsync"
install -m 755 "$BIN" "$deb/usr/bin/bettrsync"
install -m 644 "$ROOT/README.md" "$deb/usr/share/doc/bettrsync/README.md"
[ -f "$ROOT/LICENSE" ] && install -m 644 "$ROOT/LICENSE" "$deb/usr/share/doc/bettrsync/copyright"
cat > "$deb/DEBIAN/control" <<CONTROL
Package: bettrsync
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Maintainer: Max Vilimpoc <max@vilimpoc.org>
Depends: libc6 (>= 2.31)
Recommends: openssh-client
Homepage: https://github.com/nuket/bettrsync
Description: rsync -rltp with in-place, parallel block updates
 $DESCRIPTION. Files the destination already has are
 updated in place, both sides hashing them in blocks on every core at
 once, so only the blocks that differ cross the wire. Either end may be
 remote over ssh, with bettrsync installed there too.
CONTROL
dpkg-deb --build --root-owner-group "$deb" "$OUT/bettrsync_${VERSION}_$ARCH.deb" >/dev/null
echo "$OUT/bettrsync_${VERSION}_$ARCH.deb"

# The .rpm, if rpmbuild is here.
if command -v rpmbuild >/dev/null; then
    rpm="$work/rpm"
    mkdir -p "$rpm"
    cat > "$rpm/bettrsync.spec" <<SPEC
Name:           bettrsync
Version:        $VERSION
Release:        1
Summary:        rsync -rltp with in-place, parallel block updates
License:        ${LICENSE:-Unspecified}
URL:            https://github.com/nuket/bettrsync
Recommends:     openssh-clients
%global __os_install_post %{nil}
%global debug_package %{nil}
%global _build_id_links none

%description
$DESCRIPTION. Files the destination already has are
updated in place, both sides hashing them in blocks on every core at
once, so only the blocks that differ cross the wire. Either end may be
remote over ssh, with bettrsync installed there too.

%install
install -D -m 755 "$BIN" %{buildroot}/usr/bin/bettrsync
install -D -m 644 "$ROOT/README.md" %{buildroot}/usr/share/doc/bettrsync/README.md

%files
/usr/bin/bettrsync
%doc /usr/share/doc/bettrsync/README.md
SPEC
    rpmbuild -bb --quiet --target "$RPM_ARCH" --define "_topdir $rpm" --define "_rpmdir $OUT" --define "_rpmfilename %%{NAME}-%%{VERSION}-%%{RELEASE}.%%{ARCH}.rpm" "$rpm/bettrsync.spec" >/dev/null
    echo "$OUT/bettrsync-$VERSION-1.$RPM_ARCH.rpm"
else
    echo "rpmbuild not found: no .rpm" >&2
fi
