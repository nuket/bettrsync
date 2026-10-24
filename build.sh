#!/usr/bin/env bash
# bettrsync
# Copyright (c) 2026 Max Vilimpoc
# SPDX-License-Identifier: MIT
# Builds bettrsync.  On Linux the build runs in the rust:bullseye Docker
# image, as Shove's Cli/build.sh does, so the build systems need no Rust
# toolchain and the binary links against an old glibc that runs anywhere.

set -euo pipefail

cd "$(dirname "$0")"

MODE=release
for arg in "$@"; do
    case "$arg" in
        --debug)   MODE=debug ;;
        --release) MODE=release ;;
        *) echo "Unknown argument: $arg" >&2
           echo "Usage: $0 [--debug|--release]" >&2
           exit 1 ;;
    esac
done

case "$MODE" in
    debug)   CARGO_FLAGS= ;;
    release) CARGO_FLAGS=--release ;;
esac

# The revision stamped into the executable: BETTRSYNC_GIT_REVISION when the
# caller knows it (systems-build.py, building a tree synced without its git
# history), else this checkout's.
if [ -n "${BETTRSYNC_GIT_REVISION:-}" ]; then
    GIT_REV=$BETTRSYNC_GIT_REVISION
else
    GIT_REV=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")
    GIT_DIRTY=$(git status --porcelain 2>/dev/null || true)
    [ -n "$GIT_DIRTY" ] && GIT_REV="${GIT_REV}-modified"
fi

if [[ "$(uname)" != "Linux" ]] || ! command -v docker >/dev/null; then
    BETTRSYNC_GIT_REVISION="$GIT_REV" cargo build $CARGO_FLAGS
else
    # Build as the invoking user, not as root, so target/ and Cargo.lock come
    # out owned by whoever ran this.  passwd/group are mounted so the uid/gid
    # resolve to the same names inside the container.  The cargo cache lives
    # in a host directory for the same reason: a named volume is root-owned.
    CARGO_CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/bettrsync/cargo"
    mkdir -p "$CARGO_CACHE"
    # With https_proxy set (systems-build.py lends the control system's
    # internet through a tunnel on this machine's loopback), the container
    # shares this machine's network so cargo can reach the proxy there.
    NET=()
    if [ -n "${https_proxy:-}" ]; then
        NET=(--network host --env https_proxy --env "http_proxy=${http_proxy:-$https_proxy}")
    fi
    docker run --rm "${NET[@]}" \
        --user "$(id -u):$(id -g)" \
        --volume /etc/passwd:/etc/passwd:ro \
        --volume /etc/group:/etc/group:ro \
        --env HOME=/tmp \
        --env CARGO_HOME=/cargo \
        --env CARGO_FLAGS="$CARGO_FLAGS" \
        --env BETTRSYNC_GIT_REVISION="$GIT_REV" \
        --volume "$CARGO_CACHE":/cargo \
        --volume "$(pwd)":/repo \
        --workdir /repo \
        rust:bullseye \
        sh -c 'cargo build $CARGO_FLAGS'
fi

echo "Binary: $(pwd)/target/$MODE/bettrsync"
