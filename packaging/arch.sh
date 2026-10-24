#!/usr/bin/env bash
# bettrsync
# Copyright (c) 2026 Max Vilimpoc
# SPDX-License-Identifier: MIT
#
# Makes the Arch package, bettrsync-VERSION-1-ARCH.pkg.tar.zst, for
# pacman -U, from this tree with packaging/arch/PKGBUILD.  On an Arch
# system (CachyOS, Manjaro...) makepkg runs here; anywhere else with
# Docker, in the archlinux image, this script running itself there.
# The result is installed and run once, to be sure it is good.
#
# usage: packaging/arch.sh OUTDIR

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=$(realpath -m "$1")
mkdir -p "$OUT"

if ! command -v makepkg >/dev/null; then
    command -v docker >/dev/null || { echo "neither makepkg nor docker is here" >&2; exit 1; }
    echo "no makepkg here: in the archlinux image"
    exec docker run --rm --volume "$ROOT":/repo:ro --volume "$OUT":/out \
        --env BETTRSYNC_GIT_REVISION="${BETTRSYNC_GIT_REVISION:-}" \
        archlinux:latest bash -c '
            set -e
            pacman -Syu --noconfirm --needed base-devel rust python >/dev/null
            useradd -m builder
            sudo -u builder --preserve-env=BETTRSYNC_GIT_REVISION /repo/packaging/arch.sh /tmp/dist
            pacman -U --noconfirm /tmp/dist/*.pkg.tar.zst >/dev/null
            bettrsync --version
            cp /tmp/dist/*.pkg.tar.zst /out/
            chown "$(stat -c %u:%g /out)" /out/*.pkg.tar.zst'
fi

version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
[ -n "${BETTRSYNC_GIT_REVISION:-}" ] || BETTRSYNC_GIT_REVISION=$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)
export BETTRSYNC_GIT_REVISION
export PACKAGER="Max Vilimpoc <max@vilimpoc.org>"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# The source tarball: the tree, without what is built or kept beside it.
tar -C "$ROOT" --exclude=./target --exclude=./artifacts --exclude=./dist --exclude=./.git \
    --exclude=./LOG.md --exclude=./systems.yaml --exclude='./id_testkey*' \
    --transform "s,^\./,bettrsync-$version/," -czf "$work/bettrsync-$version.tar.gz" .
sed "s/^pkgver=.*/pkgver=$version/" "$ROOT/packaging/arch/PKGBUILD" > "$work/PKGBUILD"
(cd "$work" && makepkg --noconfirm --force --nodeps >/dev/null)
cp "$work"/*.pkg.tar.zst "$OUT/"
ls "$OUT"/*.pkg.tar.zst
