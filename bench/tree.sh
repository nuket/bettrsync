#!/usr/bin/env bash
# Times rsync -rlt against bettrsync on a tree of many small files, over
# ssh to a remote host, with the page caches cold and hot on both sides.
# Run from the sending machine.  bench.sh is the same for one big image.
#
# usage: bench/tree.sh BETTRSYNC USER@HOST SCRATCHDIR [FILES] [CHANGES]
#
# The tree is FILES (100000) files of 512 B to 16 KiB in FILES/500
# directories, plus FILES/100 symlinks, made in SCRATCHDIR/bench/tree the
# first time.  Each changed round rewrites two thirds of CHANGES (1500)
# files and adds the other third.  Every write is verified by comparing a
# fingerprint of the trees: paths, sizes, times to the nanosecond, link
# targets and contents.  ssh uses BENCH_SSH ("ssh" by default) and
# bettrsync on the remote is BENCH_REMOTE_BIN ("./bettrsync").
#
# Cold means the whole page cache dropped on both sides, which needs
# sudo -n tee /proc/sys/vm/drop_caches without a password on both.

set -euo pipefail

BIN=$(realpath "$1")
REMOTE=$2
S=$3
FILES=${4:-100000}
CHANGES=${5:-1500}
SSH=${BENCH_SSH:-ssh}
RBIN=${BENCH_REMOTE_BIN:-./bettrsync}

PER=500
DIRS=$(( (FILES + PER - 1) / PER ))
LINKS=$(( FILES / 100 ))
T="$S/bench/tree"
RT=bench/tree
mkdir -p "$S/bench"

say() { echo; echo "## $*"; }
remote() { $SSH "$REMOTE" "$@"; }
now() { date +%s.%N; }

cold() {
    sync
    echo 3 | sudo -n tee /proc/sys/vm/drop_caches >/dev/null 2>&1 || echo "(no sudo here: cache not dropped)"
    remote "sync; echo 3 | sudo -n tee /proc/sys/vm/drop_caches >/dev/null 2>&1 || echo '(no sudo there: cache not dropped)'"
}

hot() {
    (cd "$T" && find . -type f -exec cat {} + >/dev/null) &
    remote "cd $RT 2>/dev/null && find . -type f -exec cat {} + >/dev/null; true"
    wait
}

# Paths, kinds, sizes, times to the nanosecond, link targets, contents.
# A directory's own size is left out: it depends on how it was filled.
fingerprint() {
    cd "$1" && {
        find . -type d -printf '%y %p %T@\n' | LC_ALL=C sort
        find . ! -type d -printf '%y %p %s %T@ %l\n' | LC_ALL=C sort
        find . -type f -print0 | LC_ALL=C sort -z | xargs -0 cat
    } | md5sum | cut -d' ' -f1
}

verify() {
    local a b
    a=$(fingerprint "$T")
    b=$(remote "$(declare -f fingerprint); fingerprint $RT")
    if [ "$a" = "$b" ]; then echo "verified: the trees match"; else echo "MISMATCH: $a here, $b there"; exit 1; fi
}

# Rewrites CHANGES*2/3 files and adds CHANGES/3, the same for the same seed.
mutate() {
    python3 - "$T" "$1" "$((CHANGES * 2 / 3))" "$((CHANGES - CHANGES * 2 / 3))" <<'PY'
import os, random, sys
root, seed, nchange, nnew = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4])
random.seed(seed)
files = sorted(os.path.join(dp, f) for dp, _, fs in os.walk(root) for f in fs)
for p in random.sample(files, nchange):
    with open(p, 'wb') as f:
        f.write(os.urandom(random.randint(512, 16384)))
dirs = sorted({os.path.dirname(p) for p in files})
for i in range(nnew):
    with open(os.path.join(random.choice(dirs), f'new-{seed}-{i}.dat'), 'wb') as f:
        f.write(os.urandom(random.randint(512, 16384)))
PY
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

RSYNC=(rsync -rlt -e "$SSH")
BS=("$BIN" -e "$SSH" --remote-bin "$RBIN")

if [ ! -d "$T" ]; then
    say "making $FILES files in $DIRS directories, $LINKS links"
    python3 - "$T" "$DIRS" "$PER" "$LINKS" "$FILES" <<'PY'
import os, random, sys
root, dirs, per, links, total = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
random.seed(1)
made = 0
for d in range(dirs):
    dp = os.path.join(root, f'd{d // 20:02d}', f'd{d:03d}')
    os.makedirs(dp)
    for i in range(per):
        if made == total:
            break
        with open(os.path.join(dp, f'f{i:04d}.dat'), 'wb') as f:
            f.write(os.urandom(random.randint(512, 16384)))
        made += 1
for i in range(links):
    d = random.randrange(dirs)
    dp = os.path.join(root, f'd{d // 20:02d}', f'd{d:03d}')
    os.symlink(f'f{random.randrange(per):04d}.dat', os.path.join(dp, f'l{i:04d}'))
PY
fi
echo "tree: $(find "$T" -type f | wc -l) files, $(find "$T" -type d | wc -l) dirs, $(find "$T" -type l | wc -l) links, $(du -sh "$T" | cut -f1)"
remote "mkdir -p bench"

say "new tree, cold: rsync"
remote "rm -rf $RT"; cold
timed "rsync new tree, cold" "${RSYNC[@]}" "$T/" "$REMOTE:$RT/"
verify
say "new tree, cold: bettrsync"
remote "rm -rf $RT"; cold
timed "bettrsync new tree, cold" "${BS[@]}" --commit -q "$T/" "$REMOTE:$RT/"
verify

say "unchanged, hot"
hot
timed "rsync, unchanged, hot" "${RSYNC[@]}" "$T/" "$REMOTE:$RT/"
timed "bettrsync, unchanged, hot" "${BS[@]}" --commit -q "$T/" "$REMOTE:$RT/"
timed "bettrsync -c, unchanged, hot" "${BS[@]}" -c -q "$T/" "$REMOTE:$RT/"

say "$CHANGES files changed, cold then hot: rsync"
mutate 1; cold
timed "rsync, $CHANGES changed, cold" "${RSYNC[@]}" "$T/" "$REMOTE:$RT/"
verify
mutate 2; hot
timed "rsync, $CHANGES changed, hot" "${RSYNC[@]}" "$T/" "$REMOTE:$RT/"
verify
say "$CHANGES files changed, cold then hot: bettrsync"
mutate 3; cold
timed "bettrsync, $CHANGES changed, cold" "${BS[@]}" --commit -q "$T/" "$REMOTE:$RT/"
verify
mutate 4; hot
timed "bettrsync, $CHANGES changed, hot" "${BS[@]}" --commit -q "$T/" "$REMOTE:$RT/"
verify

say "results: $(find "$T" -type f | wc -l) files in $(find "$T" -type d | wc -l) dirs, $LINKS links, $CHANGES changed per round, $(nproc) cores here"
printf '%s\n' "${RESULTS[@]}"
