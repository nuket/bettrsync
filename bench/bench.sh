#!/usr/bin/env bash
# Times rsync --inplace against bettrsync on one large image with a few
# hundred changed spots, over ssh to a remote host, with the page caches
# cold and then hot on both sides.  Run from the sending machine.
#
# usage: bench/bench.sh BETTRSYNC USER@HOST SCRATCHDIR [SIZE_GIB] [CHANGES]
#
# The image is SIZE_GIB (4) of random data, made in SCRATCHDIR/bench the
# first time and copied to USER@HOST:bench/ (that first copy is timed too).
# Each round changes CHANGES (200) spots of 4 KiB to 1 MiB, then syncs.
# ssh uses BENCH_SSH ("ssh" by default) and bettrsync on the remote is
# BENCH_REMOTE_BIN ("./bettrsync").
#
# Cold means the image's pages are evicted on both sides (dd iflag=nocache,
# which needs no root), and the whole page cache dropped as well where
# sudo allows tee /proc/sys/vm/drop_caches without a password.  Hot means
# the image was just read through on both sides.

set -euo pipefail

BIN=$(realpath "$1")
REMOTE=$2
S=$3
SIZE=${4:-4}
CHANGES=${5:-200}
SSH=${BENCH_SSH:-ssh}
RBIN=${BENCH_REMOTE_BIN:-./bettrsync}

D="$S/bench"
IMG="$D/image.img"
mkdir -p "$D"
cd "$D"

say() { echo; echo "## $*"; }

remote() { $SSH "$REMOTE" "$@"; }

now() { date +%s.%N; }

# Evicts the image's pages on both sides, and everything else where allowed.
cold() {
    sync
    dd if="$IMG" iflag=nocache count=0 status=none
    if sudo -n true 2>/dev/null; then echo 3 | sudo -n tee /proc/sys/vm/drop_caches >/dev/null; else echo "(no sudo here: only the image evicted)"; fi
    remote "sync; dd if=bench/image.img iflag=nocache count=0 status=none; if sudo -n true 2>/dev/null; then echo 3 | sudo -n tee /proc/sys/vm/drop_caches >/dev/null; else echo '(no sudo there: only the image evicted)'; fi"
}

hot() {
    cat "$IMG" >/dev/null &
    remote "cat bench/image.img >/dev/null"
    wait
}

# Changes CHANGES spots of the image, the same spots for the same round.
mutate() {
    python3 - "$IMG" "$CHANGES" "$1" <<'EOF'
import os, random, sys
path, changes, seed = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
random.seed(seed)
size = os.path.getsize(path)
with open(path, 'r+b') as f:
    for _ in range(changes):
        n = random.choice([4096, 65536, 1 << 20])
        f.seek(random.randrange(0, size - n))
        f.write(os.urandom(n))
EOF
}

verify() {
    local a b
    a=$(md5sum "$IMG" | cut -d' ' -f1) &
    b=$(remote "md5sum bench/image.img | cut -d' ' -f1")
    wait
    a=$(md5sum "$IMG" | cut -d' ' -f1)
    if [ "$a" = "$b" ]; then echo "verified: the copies match"; else echo "MISMATCH: $a here, $b there"; exit 1; fi
}

RESULTS=()
timed() {
    local label=$1; shift
    local t0 t1
    t0=$(now)
    "$@"
    t1=$(now)
    local secs
    secs=$(python3 -c "print(f'{$t1 - $t0:.2f}')")
    RESULTS+=("$(printf '%-34s %8s s' "$label" "$secs")")
    echo "-- $label: $secs s"
}

RSYNC=(rsync -rlt --inplace -e "$SSH")
BS=("$BIN" -e "$SSH" --remote-bin "$RBIN")

if [ ! -f "$IMG" ]; then
    say "making a $SIZE GiB image of random data"
    head -c "$((SIZE << 30))" /dev/urandom > "$IMG"
fi
remote "mkdir -p bench"

if ! remote "test -f bench/image.img && test \$(stat -c %s bench/image.img) = $(stat -c %s "$IMG")"; then
    say "first copy, cold caches"
    remote "rm -f bench/image.img"
    cold
    timed "bettrsync new file, cold" "${BS[@]}" --commit -q "$IMG" "$REMOTE:bench/"
    verify
fi

say "rsync --inplace, cold"
mutate 1; cold
timed "rsync --inplace, cold" "${RSYNC[@]}" "$IMG" "$REMOTE:bench/"
verify
say "rsync --inplace, hot"
mutate 2; hot
timed "rsync --inplace, hot" "${RSYNC[@]}" "$IMG" "$REMOTE:bench/"
verify

say "bettrsync dry run, cold then hot"
mutate 3; cold
timed "bettrsync dry run, cold" "${BS[@]}" -q "$IMG" "$REMOTE:bench/"
timed "bettrsync dry run, hot" "${BS[@]}" -q "$IMG" "$REMOTE:bench/"
say "bettrsync --commit, cold"
cold
timed "bettrsync --commit, cold" "${BS[@]}" --commit -q "$IMG" "$REMOTE:bench/"
verify
say "bettrsync --commit, hot"
mutate 4; hot
timed "bettrsync --commit, hot" "${BS[@]}" --commit -q "$IMG" "$REMOTE:bench/"
verify

say "nothing changed, hot: rsync then bettrsync"
timed "rsync, unchanged, hot" "${RSYNC[@]}" "$IMG" "$REMOTE:bench/"
timed "bettrsync, unchanged, hot" "${BS[@]}" --commit -q "$IMG" "$REMOTE:bench/"
timed "bettrsync -c, unchanged, hot" "${BS[@]}" -c -q "$IMG" "$REMOTE:bench/"

say "results: $SIZE GiB image, $CHANGES changed spots, $(nproc) cores here"
printf '%s\n' "${RESULTS[@]}"
