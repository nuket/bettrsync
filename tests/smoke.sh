#!/usr/bin/env bash
# A smoke test of bettrsync between two local directories: a small tree
# with links and times, then an image changed in a few places, shrunk and
# grown.  Stops at the first check that fails; prints ALL OK at the end.
#
# usage: tests/smoke.sh BETTRSYNC SCRATCHDIR

set -euo pipefail

BIN=$(realpath "$1")
S=$2

ok() { echo "$1: ok"; }
fail() { echo "FAIL: $1"; exit 1; }

rm -rf "$S/smoke"
mkdir -p "$S/smoke"
cd "$S/smoke"

mkdir -p src/a/b src/empty
echo hello > src/a/hello.txt
: > src/a/zero
head -c 1000003 /dev/urandom > src/a/b/odd.bin
ln -s hello.txt src/a/link
ln -s ../a/b src/a/dirlink
head -c 256M /dev/urandom > src/image.img
chmod +x src/a/b/odd.bin
touch -d '2020-01-02 03:04:05.678' src/a/hello.txt
touch -h -d '2021-01-01 00:00:00' src/a/link
touch -d '2019-05-05 05:05:05' src/a/b src/empty src

echo "== dry run"
"$BIN" src/ dst/
test ! -e dst && ok "dst untouched" || fail "dry run made dst"

echo "== commit"
"$BIN" --commit src/ dst/
diff -r src dst >/dev/null && ok diff || fail diff
(cd src && find . -printf '%p %T@ %l %m\n' | sort) > src.meta
(cd dst && find . -printf '%p %T@ %l %m\n' | sort) > dst.meta
diff src.meta dst.meta && ok metadata || fail metadata

echo "== second run, nothing to do"
"$BIN" --commit src/ dst/ | tail -3

INODE=$(stat -c %i dst/image.img)
python3 - <<'EOF'
import os, random
random.seed(1)
with open('src/image.img', 'r+b') as f:
    for _ in range(20):
        off = random.randrange(0, 256 * 1024 * 1024 - 65536)
        f.seek(off)
        f.write(os.urandom(random.choice([1, 4096, 65536])))
EOF
echo "== changed image, dry run"
"$BIN" src/ dst/
cmp -s src/image.img dst/image.img && fail "the dry run wrote to the image" || ok "dry run left the image"
echo "== changed image, commit"
"$BIN" --commit src/ dst/
cmp src/image.img dst/image.img && ok image || fail image
test "$(stat -c %i dst/image.img)" = "$INODE" && ok "in place" || fail "not in place"

echo "== checksum run"
"$BIN" -c src/ dst/ | tail -4

truncate -s 100M src/image.img
"$BIN" -q --commit src/ dst/
cmp src/image.img dst/image.img && ok shrink || fail shrink
head -c 50M /dev/urandom >> src/image.img
"$BIN" -q --commit src/ dst/
cmp src/image.img dst/image.img && ok grow || fail grow

"$BIN" -q --commit src/a/hello.txt dst/copy.txt
cmp src/a/hello.txt dst/copy.txt && ok "single file" || fail "single file"
"$BIN" -q --commit src/a dst2
diff -r src/a dst2/a >/dev/null && ok "no trailing slash" || fail "no trailing slash"

echo "ALL OK"
