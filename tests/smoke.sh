#!/usr/bin/env bash
# A smoke test of bettrsync between two local directories: a small tree
# with links and times, then an image changed in a few places, shrunk and
# grown.  Stops at the first check that fails; prints ALL OK at the end.
#
# usage: tests/smoke.sh BETTRSYNC SCRATCHDIR

set -euo pipefail

BIN=$(realpath "$1")
S=$2
FAKESSH=$(dirname "$(realpath "$0")")/fakessh.sh

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

# The same over the fake rsh: the remote code path, both directions.
REMOTE=("$BIN" -e "$FAKESSH" --remote-bin "$BIN")
echo "== over the fake rsh, push and pull"
"${REMOTE[@]}" -q --commit src/ "fake:$PWD/rdst/"
diff -r src rdst >/dev/null && ok "push" || fail "push"
(cd src && find . -printf '%p %T@ %l %m\n' | sort) > src.meta
(cd rdst && find . -printf '%p %T@ %l %m\n' | sort) > rdst.meta
diff src.meta rdst.meta >/dev/null && ok "push metadata" || fail "push metadata"
"${REMOTE[@]}" -q --commit "fake:$PWD/src/" rpull/
diff -r src rpull >/dev/null && ok "pull" || fail "pull"
"${REMOTE[@]}" -q --commit src/ "fake:$PWD/rdst/" | grep -q 'unchanged 10, updated 0, new 0' && ok "unchanged over rsh" || fail "unchanged over rsh"

# A tree of more files than fit in flight at once, over the fake rsh:
# every slot is taken and given back many times over.
echo "== a tree of 2,500 files"
python3 - <<'PY'
import os, random
random.seed(2)
for d in range(5):
    os.makedirs(f'tree/d{d}/sub')
    for i in range(500):
        with open(f'tree/d{d}/f{i:04d}', 'wb') as f:
            f.write(os.urandom(random.randint(0, 20000)))
    with open(f'tree/d{d}/sub/s', 'wb') as f:
        f.write(b'x' * 100000)
    os.symlink('f0000', f'tree/d{d}/link')
PY
treemeta() { (cd "$1" && find . -printf '%y %p %s %T@ %l %m\n' | grep -v '^d' | sort); }
"${REMOTE[@]}" -q --commit tree/ "fake:$PWD/tdst/" | grep -q 'new 2,505 files, 10 dirs, 5 links, failed 0' && ok "tree copied" || fail "tree copied"
diff -r tree tdst >/dev/null && ok "tree diff" || fail "tree diff"
diff <(treemeta tree) <(treemeta tdst) >/dev/null && ok "tree metadata" || fail "tree metadata"
"${REMOTE[@]}" -q --commit tree/ "fake:$PWD/tdst/" | grep -q 'unchanged 2,521, updated 0, new 0' && ok "tree unchanged" || fail "tree unchanged"

# Changes of every kind at once: more rewritten and more new files
# than fit in flight, three unreadable new ones, and a type change each
# way between file, directory and link.
echo "== the tree changed: 1,200 rewritten, 1,100 new, 3 unreadable, 4 type changes"
python3 - <<'PY'
import os, random, shutil
random.seed(3)
files = sorted(f'tree/d{d}/f{i:04d}' for d in range(5) for i in range(500))
for p in random.sample(files, 1200):
    with open(p, 'wb') as f:
        f.write(os.urandom(random.randint(0, 20000)))
for i in range(1100):
    with open(f'tree/d{i % 5}/n{i:04d}', 'wb') as f:
        f.write(os.urandom(random.randint(0, 20000)))
for i in range(3):
    p = f'tree/d4/unreadable{i}'
    with open(p, 'wb') as f:
        f.write(b'secret')
    os.chmod(p, 0)
os.remove('tree/d0/f0001'); os.makedirs('tree/d0/f0001'); open('tree/d0/f0001/inside', 'w').write('in')
shutil.rmtree('tree/d1/sub'); open('tree/d1/sub', 'w').write('was a directory')
os.remove('tree/d2/link'); open('tree/d2/link', 'w').write('was a link')
os.remove('tree/d3/f0002'); os.symlink('f0003', 'tree/d3/f0002')
PY
rc=0
out=$("${REMOTE[@]}" -q --commit tree/ "fake:$PWD/tdst/" 2>&1) || rc=$?
test "$rc" = 1 && ok "exit status 1 with failures" || fail "exit status $rc with failures"
echo "$out" | grep -q 'failed 3' && ok "3 failed" || fail "failed count: $(echo "$out" | grep total)"
echo "$out" | grep -q 'updated 1,2' && ok "rewritten files updated" || fail "updated count: $(echo "$out" | grep total)"
test -z "$(find tdst -name '*.bettrsync.tmp')" && ok "no temp files left" || fail "temp files left behind"
test ! -e tdst/d4/unreadable0 && ok "unreadable not copied" || fail "unreadable copied"
chmod 644 tree/d4/unreadable*
"${REMOTE[@]}" -q --commit tree/ "fake:$PWD/tdst/" | grep -q 'new 3 files, 0 dirs, 0 links, failed 0' && ok "readable now, copied" || fail "readable now"
diff -r tree tdst >/dev/null && ok "changed tree diff" || fail "changed tree diff"
diff <(treemeta tree) <(treemeta tdst) >/dev/null && ok "changed tree metadata" || fail "changed tree metadata"
test -d tdst/d0/f0001 -a -f tdst/d1/sub -a -f tdst/d2/link -a -L tdst/d3/f0002 && ok "type changes" || fail "type changes"

# Same size and time, different content: only -c sees it.
echo "== same size and time, different content"
python3 - <<'PY'
import os
p = 'tree/d0/f0100'
st = os.stat(p)
with open(p, 'r+b') as f:
    f.seek(0); f.write(b'\xff' * min(64, st.st_size))
os.utime(p, ns=(st.st_atime_ns, st.st_mtime_ns))
PY
"${REMOTE[@]}" -q --commit tree/ "fake:$PWD/tdst/" | grep -q 'unchanged 3,6' && ok "not seen without -c" || fail "seen without -c"
"${REMOTE[@]}" -q -c --commit tree/ "fake:$PWD/tdst/" | grep -q 'updated 1,' && ok "seen with -c" || fail "not seen with -c"
cmp tree/d0/f0100 tdst/d0/f0100 && ok "repaired with -c" || fail "not repaired with -c"
"${REMOTE[@]}" -q -c tree/ "fake:$PWD/tdst/" | grep -q 'unchanged 3,6.*updated 0, new 0' && ok "-c clean after" || fail "-c not clean after"

# A remote bettrsync that is not there, both ways: a clear message, exit 2.
echo "== remote not found"
! "$BIN" -e "$FAKESSH" --remote-bin /nonexistent/bettrsync tree/ "fake:$PWD/xnone/" 2>xerr && grep -q 'not found on fake.*--remote-bin' xerr && ok "push: not found" || fail "push: not found: $(cat xerr)"
! "$BIN" -e "$FAKESSH" --remote-bin /nonexistent/bettrsync "fake:$PWD/tree/" xnone/ 2>xerr && grep -q 'not found on fake.*--remote-bin' xerr && ok "pull: not found" || fail "pull: not found: $(cat xerr)"
test ! -e xnone && ok "nothing created" || fail "xnone created"

# --exclude: a name anywhere, a directory-only name, an anchored path,
# a path with ** across levels; the same over the fake rsh.
echo "== --exclude"
mkdir -p xsrc/a/cache xsrc/b/cache xsrc/c/deep/er
echo 1 > xsrc/a/keep; echo 2 > xsrc/a/x.tmp; echo 3 > xsrc/a/cache/c1; echo 4 > xsrc/b/cache/c2
echo 5 > xsrc/b/top; echo 6 > xsrc/top; echo 7 > xsrc/c/deep/er/leaf.log; echo 8 > xsrc/c/deep/leaf.log
EXCL=(--exclude '*.tmp' --exclude 'cache/' --exclude '/top' --exclude 'deep/**/leaf.log')
"$BIN" -q --commit "${EXCL[@]}" xsrc/ xdst/
test "$(cd xdst && find . -type f | sort | tr '\n' ' ')" = "./a/keep ./b/top " && ok "excluded files" || fail "excluded files: $(cd xdst && find . -type f | sort | tr '\n' ' ')"
test ! -e xdst/a/cache -a ! -e xdst/top -a -d xdst/c/deep/er && ok "excluded dirs, anchored, kept" || fail "excluded dirs"
"${REMOTE[@]}" -q --commit "${EXCL[@]}" xsrc/ "fake:$PWD/xdst2/"
diff -r xdst xdst2 >/dev/null && ok "excludes over rsh" || fail "excludes over rsh"
# --exclude-from: the same patterns from a file, comments and blanks
# skipped, mixed with --exclude, and from stdin over the fake rsh.
printf '# comment\n*.tmp\n\n; also a comment\ncache/\n/top\n' > xfrom
"$BIN" -q --commit --exclude-from xfrom --exclude 'deep/**/leaf.log' xsrc/ xdst3/
diff -r xdst xdst3 >/dev/null && ok "--exclude-from" || fail "--exclude-from: $(cd xdst3 && find . -type f | sort | tr '\n' ' ')"
"${REMOTE[@]}" -q --commit --exclude-from - --exclude 'deep/**/leaf.log' xsrc/ "fake:$PWD/xdst4/" < xfrom
diff -r xdst xdst4 >/dev/null && ok "--exclude-from - over rsh" || fail "--exclude-from - over rsh"
! "$BIN" -q --exclude-from xmissing xsrc/ xdst3/ 2>xerr && grep -q 'xmissing' xerr && ok "missing file refused" || fail "missing file not refused"

# --delete: extras go, excluded extras stay, a dry run only lists, and
# a warning on the sending side deletes nothing.
echo "== --delete"
mkdir -p xdst/extra/dir xdst/a/cache
echo e > xdst/extra/dir/f; echo e > xdst/a/old; echo e > xdst/a/keep.tmp; echo e > xdst/a/cache/stale
out=$("$BIN" --delete "${EXCL[@]}" xsrc/ xdst/)
echo "$out" | grep -q '^delete   extra/$' && echo "$out" | grep -q '^delete   a/old$' && ok "dry run lists" || fail "dry run listing: $out"
echo "$out" | grep -q 'to delete 2' && ok "dry run counts" || fail "dry run count"
test -e xdst/extra/dir/f -a -e xdst/a/old && ok "dry run deleted nothing" || fail "dry run deleted"
"$BIN" -q --commit --delete "${EXCL[@]}" xsrc/ xdst/ | grep -q 'deleted 2' && ok "deleted 2" || fail "deleted count"
test ! -e xdst/extra -a ! -e xdst/a/old && ok "extras gone" || fail "extras remain"
test -e xdst/a/keep.tmp -a -e xdst/a/cache/stale && ok "excluded extras kept" || fail "excluded extras deleted"
"$BIN" -q --commit --delete xsrc/ xdst/ | grep -q 'deleted 2' && ok "no excludes: 2 more deleted" || fail "no excludes: deleted count"
diff -r xsrc xdst >/dev/null && ok "delete makes a mirror" || fail "not a mirror"
mkdir xsrc/locked; echo l > xsrc/locked/f; chmod 000 xsrc/locked; echo e > xdst/a/extra2
rc=0; out=$("$BIN" -q --commit --delete xsrc/ xdst/ 2>&1) || rc=$?
test "$rc" = 1 && echo "$out" | grep -q 'nothing deleted' && test -e xdst/a/extra2 && ok "warning: nothing deleted" || fail "deleted despite a warning (rc $rc): $out"
chmod 755 xsrc/locked
"$BIN" -q --commit --delete xsrc/ xdst/ | grep -q 'deleted 1' && test ! -e xdst/a/extra2 && ok "deleted after the fix" || fail "not deleted after the fix"
echo e > xdst/a/extra3
"${REMOTE[@]}" -q --commit --delete xsrc/ "fake:$PWD/xdst/" | grep -q 'deleted 1' && test ! -e xdst/a/extra3 && ok "delete over rsh" || fail "delete over rsh"
diff -r xsrc xdst >/dev/null && ok "mirror over rsh" || fail "not a mirror over rsh"

# Permission bits: set on a file and a directory already there, and on
# a new file past what the umask would give.
echo "== modes"
chmod 666 xsrc/a/keep; chmod 710 xsrc/c
mkdir xsrc/m; chmod 700 xsrc/m; echo n > xsrc/m/n; chmod 666 xsrc/m/n
out=$("$BIN" --commit xsrc/ xdst/)
test "$(echo "$out" | grep -c '^chmod')" = 2 && ok "chmod listed for the two existing" || fail "chmod lines: $(echo "$out" | grep '^chmod')"
echo "$out" | grep -q 'unchanged 1[0-9], updated 0, new 1 files, 1 dirs' && ok "a chmod alone is unchanged" || fail "counts: $(echo "$out" | grep total)"
diff <(cd xsrc && find . -printf '%p %m\n' | sort) <(cd xdst && find . -printf '%p %m\n' | sort) >/dev/null && ok "modes match" || fail "modes differ: $(diff <(cd xsrc && find . -printf '%p %m\n' | sort) <(cd xdst && find . -printf '%p %m\n' | sort))"
"$BIN" --commit xsrc/ xdst/ | grep -q '^chmod' && fail "chmod again" || ok "nothing to chmod after"
"${REMOTE[@]}" -q --commit xsrc/ "fake:$PWD/xdst3/"
diff <(cd xsrc && find . -printf '%p %m\n' | sort) <(cd xdst3 && find . -printf '%p %m\n' | sort) >/dev/null && ok "modes over rsh" || fail "modes over rsh"

echo "ALL OK"
