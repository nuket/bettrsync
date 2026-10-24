#!/usr/bin/env python3
# bettrsync
# Copyright (c) 2026 Max Vilimpoc
# SPDX-License-Identifier: MIT
"""A smoke test of bettrsync between two local directories, in Python
so that it runs on Windows as well as Linux: a small tree with times
and a read-only file (and links, where they can be made), an image
changed in a few places, shrunk and grown, the remote code path through
a stand-in for ssh, a tree of thousands of files changed every way at
once, -c, --exclude, --exclude-from, --delete, the bits, and on
Windows the spellings of a path. Stops at the first check that fails;
prints ALL OK at the end.

usage: smoke.py BETTRSYNC SCRATCHDIR
"""

import filecmp
import os
import random
import shutil
import stat
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from treecmp import differences  # noqa: E402

BIN = os.path.abspath(sys.argv[1])
SCRATCH = sys.argv[2]
FAKESSH = f'{sys.executable} {os.path.join(os.path.dirname(os.path.abspath(__file__)), "fakessh.py")}'
WIN = os.name == 'nt'
REMOTE = [BIN, '-e', FAKESSH, '--remote-bin', BIN]


def ok(what):
    print(f'{what}: ok')


def fail(what):
    print(f'FAIL: {what}')
    sys.exit(1)


def run(*args, expect=0, stdin=None):
    """bettrsync with `args`; its output, stderr included, or a failure
    if the status is not `expect`."""
    p = subprocess.run([BIN, *args], input=stdin, capture_output=True, text=True)
    if p.returncode != expect:
        fail(f'bettrsync {" ".join(str(a) for a in args)}: status {p.returncode}, wanted {expect}\n{p.stdout}{p.stderr}')
    return p.stdout + p.stderr


def check(cond, what, more=''):
    if cond:
        ok(what)
    else:
        fail(f'{what}{": " + more if more else ""}')


def random_bytes(n):
    return random.randbytes(n)


def write(path, data):
    os.makedirs(os.path.dirname(path) or '.', exist_ok=True)
    with open(path, 'wb') as f:
        f.write(data)


def set_time(path, when, follow=True):
    """Modification time, seconds since the epoch, with nanoseconds."""
    ns = int(when * 1e9)
    if follow or not os.path.islink(path):
        os.utime(path, ns=(ns, ns))
    elif os.utime in os.supports_follow_symlinks:
        os.utime(path, ns=(ns, ns), follow_symlinks=False)


def read_only(path, yes=True):
    if WIN:
        os.chmod(path, stat.S_IREAD if yes else stat.S_IREAD | stat.S_IWRITE)
    else:
        os.chmod(path, 0o444 if yes else 0o644)


def same_tree(a, b, what):
    """Contents and metadata, the root directory's own time too."""
    found = differences(a, b)
    if found:
        fail(f'{what}: ' + '; '.join(found[:6]))
    ok(what)


def files_of(root):
    return sorted(os.path.relpath(os.path.join(d, f), root).replace(os.sep, '/') for d, _, fs in os.walk(root) for f in fs)


random.seed(1)
root = os.path.join(SCRATCH, 'smoke')
if os.path.exists(root):
    # Read-only files stop a plain rmtree on Windows.
    for dirpath, dirs, files in os.walk(root):
        for name in files:
            try:
                read_only(os.path.join(dirpath, name), False)
            except OSError:
                pass
    shutil.rmtree(root)
os.makedirs(root)
os.chdir(root)

# Whether links can be made here: not on Windows without a privilege.
try:
    os.symlink('x', 'linktest')
    os.remove('linktest')
    LINKS = True
except OSError:
    LINKS = False
    print('no symlinks here: the link checks are skipped')

print('== a small tree')
write('src/a/hello.txt', b'hello\n')
write('src/a/zero', b'')
write('src/a/b/odd.bin', random_bytes(1000003))
os.makedirs('src/empty')
write('src/image.img', random_bytes(64 << 20))
write('src/a/ro.txt', b'read only\n')
read_only('src/a/ro.txt')
if LINKS:
    os.symlink('hello.txt', 'src/a/link')
    os.symlink(os.path.join('..', 'a', 'b'), 'src/a/dirlink')
    set_time('src/a/link', 1609459200, follow=False)
set_time('src/a/hello.txt', 1577934245.678901234)
for d in ('src/a/b', 'src/empty', 'src'):
    set_time(d, 1557032705)

run('src/', 'dst/')
check(not os.path.exists('dst'), 'dry run made nothing')
run('--commit', 'src/', 'dst/')
same_tree('src', 'dst', 'commit')
out = run('--commit', 'src/', 'dst/')
n = 11 if LINKS else 9
check(f'unchanged {n}, updated 0, new 0' in out, 'second run, nothing to do', out)

print('== the image changed, shrunk and grown')
inode = os.stat('dst/image.img').st_ino
with open('src/image.img', 'r+b') as f:
    for _ in range(20):
        f.seek(random.randrange(0, (64 << 20) - 65536))
        f.write(random_bytes(random.choice([1, 4096, 65536])))
run('src/', 'dst/')
check(not filecmp.cmp('src/image.img', 'dst/image.img', shallow=False), 'dry run left the image')
out = run('--commit', 'src/', 'dst/')
check('update   image.img' in out, 'image listed as an update', out)
check(filecmp.cmp('src/image.img', 'dst/image.img', shallow=False), 'image updated')
check(os.stat('dst/image.img').st_ino == inode, 'in place')
out = run('-c', 'src/', 'dst/')
check('updated 0' in out and 'hashed' in out, '-c compares everything', out)
with open('src/image.img', 'r+b') as f:
    f.truncate(20 << 20)
run('-q', '--commit', 'src/', 'dst/')
check(filecmp.cmp('src/image.img', 'dst/image.img', shallow=False), 'shrink')
with open('src/image.img', 'ab') as f:
    f.write(random_bytes(10 << 20))
run('-q', '--commit', 'src/', 'dst/')
check(filecmp.cmp('src/image.img', 'dst/image.img', shallow=False), 'grow')
same_tree('src', 'dst', 'tree after the image changes')

print('== a single file, a source without a trailing slash')
run('-q', '--commit', 'src/a/hello.txt', 'dst/copy.txt')
check(filecmp.cmp('src/a/hello.txt', 'dst/copy.txt', shallow=False), 'single file')
run('-q', '--commit', 'src/a', 'dst2')
same_tree('src/a', 'dst2/a', 'no trailing slash')

if WIN:
    print('== the spellings of a path on Windows')
    run('-q', '--commit', '.\\src\\', 'dstw1\\')
    same_tree('src', 'dstw1', '.\\src\\ to dstw1\\')
    os.makedirs('sub')
    os.chdir('sub')
    run('-q', '--commit', '..\\src\\', '..\\dstw2')
    os.chdir(root)
    same_tree('src', 'dstw2', '..\\src\\ from a subdirectory')
    run('-q', '--commit', os.path.join(root, 'src') + '\\', os.path.join(root, 'dstw3'))
    same_tree('src', 'dstw3', 'absolute with a trailing backslash')
    run('-q', '--commit', 'src\\a', 'dstw4')
    same_tree('src/a', 'dstw4/a', 'src\\a without a trailing backslash')
    run('-q', '--commit', 'src/a/', '.\\dstw5\\')
    same_tree('src/a', 'dstw5', 'forward slashes to a backslash destination')
    out = run('--commit', 'src\\', 'dstw1\\')
    check('updated 0, new 0' in out, 'the same tree again under another spelling', out)
    check(os.path.exists('dstw1/a/hello.txt'), 'C:-style absolute paths are local')

print('== over the fake rsh, push and pull')
here = os.path.abspath('.')
run('-e', FAKESSH, '--remote-bin', BIN, '-q', '--commit', 'src/', f'fake:{os.path.join(here, "rdst")}{os.sep}')
same_tree('src', 'rdst', 'push')
run('-e', FAKESSH, '--remote-bin', BIN, '-q', '--commit', f'fake:{os.path.join(here, "src")}{os.sep}', 'rpull/')
same_tree('src', 'rpull', 'pull')
out = run('-e', FAKESSH, '--remote-bin', BIN, '-q', '--commit', 'src/', f'fake:{os.path.join(here, "rdst")}/')
check(f'unchanged {n}, updated 0, new 0' in out, 'unchanged over rsh', out)
if WIN:
    run('-e', FAKESSH, '--remote-bin', BIN, '-q', '--commit', 'src\\', f'fake:{os.path.join(here, "rdst spaced")}\\')
    same_tree('src', 'rdst spaced', 'push to a remote path with a space and backslashes')

print('== a tree of 2,500 files')
random.seed(2)
for d in range(5):
    os.makedirs(f'tree/d{d}/sub')
    for i in range(500):
        write(f'tree/d{d}/f{i:04d}', random_bytes(random.randint(0, 20000)))
    write(f'tree/d{d}/sub/s', b'x' * 100000)
    if LINKS:
        os.symlink('f0000', f'tree/d{d}/link')
out = run(*REMOTE[1:], '-q', '--commit', 'tree/', f'fake:{os.path.join(here, "tdst")}/')
links = 5 if LINKS else 0
check(f'new 2,505 files, 10 dirs, {links} links, failed 0' in out, 'tree copied', out)
same_tree('tree', 'tdst', 'tree')
out = run(*REMOTE[1:], '-q', '--commit', 'tree/', f'fake:{os.path.join(here, "tdst")}/')
check(f'unchanged {2516 + links:,}, updated 0, new 0' in out, 'tree unchanged', out)

print('== the tree changed: 1,200 rewritten, 1,100 new, type changes')
random.seed(3)
files = sorted(f'tree/d{d}/f{i:04d}' for d in range(5) for i in range(500))
for p in random.sample(files, 1200):
    write(p, random_bytes(random.randint(0, 20000)))
for i in range(1100):
    write(f'tree/d{i % 5}/n{i:04d}', random_bytes(random.randint(0, 20000)))
os.remove('tree/d0/f0001')
write('tree/d0/f0001/inside', b'in')
shutil.rmtree('tree/d1/sub')
write('tree/d1/sub', b'was a directory')
if LINKS:
    os.remove('tree/d2/link')
    write('tree/d2/link', b'was a link')
    os.remove('tree/d3/f0002')
    os.symlink('f0003', 'tree/d3/f0002')
out = run(*REMOTE[1:], '-q', '--commit', 'tree/', f'fake:{os.path.join(here, "tdst")}/')
check('updated 1,2' in out and 'failed 0' in out, 'rewritten files updated', out)
check(not any(f.endswith('.bettrsync.tmp') for f in files_of('tdst')), 'no temp files left')
same_tree('tree', 'tdst', 'changed tree')
check(os.path.isdir('tdst/d0/f0001') and os.path.isfile('tdst/d1/sub'), 'type changes')

print('== same size and time, different content')
p = 'tree/d0/f0100'
st = os.stat(p)
with open(p, 'r+b') as f:
    f.write(b'\xff' * min(64, st.st_size))
os.utime(p, ns=(st.st_atime_ns, st.st_mtime_ns))
out = run(*REMOTE[1:], '-q', '--commit', 'tree/', f'fake:{os.path.join(here, "tdst")}/')
check('updated 0' in out, 'not seen without -c', out)
out = run(*REMOTE[1:], '-q', '-c', '--commit', 'tree/', f'fake:{os.path.join(here, "tdst")}/')
check('updated 1,' in out, 'seen with -c', out)
check(filecmp.cmp(p, 'tdst/d0/f0100', shallow=False), 'repaired with -c')

print('== remote not found')
nowhere = 'C:\\nonexistent\\bettrsync.exe' if WIN else '/nonexistent/bettrsync'
out = run('-e', FAKESSH, '--remote-bin', nowhere, 'tree/', f'fake:{os.path.join(here, "xnone")}/', expect=2)
check('not found' in out and '--remote-bin' in out, 'push: not found by path', out)
out = run('-e', FAKESSH, '--remote-bin', nowhere, f'fake:{os.path.join(here, "tree")}/', 'xnone/', expect=2)
check('not found' in out and '--remote-bin' in out, 'pull: not found by path', out)
out = run('-e', FAKESSH, '--remote-bin', 'nonexistent-bettrsync', 'tree/', f'fake:{os.path.join(here, "xnone")}/', expect=2)
check('not found' in out and '--remote-bin' in out, 'push: not found by name', out)
check(not os.path.exists('xnone'), 'nothing created')

print('== --exclude, --exclude-from')
for p, data in [('xsrc/a/keep', b'1'), ('xsrc/a/x.tmp', b'2'), ('xsrc/a/cache/c1', b'3'), ('xsrc/b/cache/c2', b'4'), ('xsrc/b/top', b'5'),
                ('xsrc/top', b'6'), ('xsrc/c/deep/er/leaf.log', b'7'), ('xsrc/c/deep/leaf.log', b'8')]:
    write(p, data)
EXCL = ['--exclude', '*.tmp', '--exclude', 'cache/', '--exclude', '/top', '--exclude', 'deep/**/leaf.log']
run('-q', '--commit', *EXCL, 'xsrc/', 'xdst/')
check(files_of('xdst') == ['a/keep', 'b/top'], 'excluded files', str(files_of('xdst')))
check(not os.path.exists('xdst/a/cache') and not os.path.exists('xdst/top') and os.path.isdir('xdst/c/deep/er'), 'excluded dirs, anchored, kept')
run(*REMOTE[1:], '-q', '--commit', *EXCL, 'xsrc/', f'fake:{os.path.join(here, "xdst2")}/')
same_tree('xdst', 'xdst2', 'excludes over rsh')
# The pattern file with Windows line ends, as an editor there writes it.
write('xfrom', b'# comment\r\n*.tmp\r\n\r\n; also a comment\r\ncache/\r\n/top\r\n')
run('-q', '--commit', '--exclude-from', 'xfrom', '--exclude', 'deep/**/leaf.log', 'xsrc/', 'xdst3/')
same_tree('xdst', 'xdst3', '--exclude-from with CRLF')
run(*REMOTE[1:], '-q', '--commit', '--exclude-from', '-', '--exclude', 'deep/**/leaf.log', 'xsrc/', f'fake:{os.path.join(here, "xdst4")}/', stdin=open('xfrom').read())
same_tree('xdst', 'xdst4', '--exclude-from - over rsh')
out = run('-q', '--exclude-from', 'xmissing', 'xsrc/', 'xdst3/', expect=2)
check('xmissing' in out, 'missing file refused', out)

print('== --delete')
for p in ('xdst/extra/dir/f', 'xdst/a/old', 'xdst/a/keep.tmp', 'xdst/a/cache/stale'):
    write(p, b'e')
out = run('--delete', *EXCL, 'xsrc/', 'xdst/')
check('delete   extra/' in out and 'delete   a/old' in out, 'dry run lists', out)
check('to delete 2' in out, 'dry run counts', out)
check(os.path.exists('xdst/extra/dir/f') and os.path.exists('xdst/a/old'), 'dry run deleted nothing')
out = run('-q', '--commit', '--delete', *EXCL, 'xsrc/', 'xdst/')
check('deleted 2' in out, 'deleted 2', out)
check(not os.path.exists('xdst/extra') and not os.path.exists('xdst/a/old'), 'extras gone')
check(os.path.exists('xdst/a/keep.tmp') and os.path.exists('xdst/a/cache/stale'), 'excluded extras kept')
out = run('-q', '--commit', '--delete', 'xsrc/', 'xdst/')
check('deleted 2' in out, 'no excludes: 2 more deleted', out)
same_tree('xsrc', 'xdst', 'delete makes a mirror')
write('xdst/a/extra2', b'e')
read_only('xdst/a/extra2')
write('xdst/extra3/ro', b'e')
read_only('xdst/extra3/ro')
out = run(*REMOTE[1:], '-q', '--commit', '--delete', 'xsrc/', f'fake:{os.path.join(here, "xdst")}/')
check('deleted 2' in out and not os.path.exists('xdst/a/extra2') and not os.path.exists('xdst/extra3'), 'read-only extras deleted over rsh', out)
same_tree('xsrc', 'xdst', 'mirror over rsh')
if WIN:
    # Foo and foo are one file here: a case that differs is no extra.
    os.rename('xdst/a/keep', 'xdst/a/KEEP')
    out = run('-q', '--commit', '--delete', 'xsrc/', 'xdst/')
    check('deleted 0' in out and os.path.exists('xdst/a/keep'), 'a case that differs is not deleted', out)
    # NTFS keeps the spelling it has, so put the name back for the checks below.
    os.rename('xdst/a/KEEP', 'xdst/a/keep')

print('== the bits')
read_only('xsrc/a/keep')
write('xsrc/m/n', b'n')
read_only('xsrc/m/n')
out = run('--commit', 'xsrc/', 'xdst/')
check(out.count('chmod    ') == 1, 'chmod listed for the existing file', out)
check('updated 0, new 1 files, 1 dirs' in out, 'a chmod alone is unchanged', out)
same_tree('xsrc', 'xdst', 'bits match')
out = run('--commit', 'xsrc/', 'xdst/')
check('chmod' not in out, 'nothing to chmod after', out)
# A read-only destination file that the source changed is written in
# place and left read-only; one the source made writable is freed.
read_only('xsrc/m/n', False)
write('xsrc/m/n', b'new content')
read_only('xsrc/m/n')
read_only('xsrc/a/keep', False)
out = run('--commit', 'xsrc/', 'xdst/')
check('update   m/n' in out and 'chmod    a/keep' in out, 'read-only file updated, bits freed', out)
same_tree('xsrc', 'xdst', 'bits after the changes')
run(*REMOTE[1:], '-q', '--commit', 'xsrc/', f'fake:{os.path.join(here, "xdst3")}/')
same_tree('xsrc', 'xdst3', 'bits over rsh')

print('ALL OK')
