#!/usr/bin/env python3
# bettrsync
# Copyright (c) 2026 Max Vilimpoc
# SPDX-License-Identifier: MIT
"""Compares two trees as bettrsync should have made the second from the
first: every path, kind, size, time and bits, and the contents of every
file. Times are compared to the nanosecond on Linux and to 100 ns on
Windows, where that is what NTFS keeps; bits are the mode's 0o7777 on
Linux and the read-only attribute on Windows. --via-windows compares
a Linux tree that went through a Windows machine and back the way
Windows could have kept it; --no-links leaves symlinks out. Prints
the first differences and exits 1 if there are any.

usage: treecmp.py [--via-windows] [--no-links] A B
"""

import filecmp
import os
import stat
import sys

WIN = os.name == 'nt'


def meta(root, via_windows=False, links=True):
    fold = WIN or via_windows
    out = []
    for dirpath, dirs, files in os.walk(root):
        for name in dirs + files:
            full = os.path.join(dirpath, name)
            st = os.lstat(full)
            rel = os.path.relpath(full, root).replace(os.sep, '/')
            t = st.st_mtime_ns // 100 if fold else st.st_mtime_ns
            if stat.S_ISLNK(st.st_mode):
                if links:
                    out.append((rel, 'l', os.readlink(full).replace('\\', '/'), t))
            elif stat.S_ISDIR(st.st_mode):
                out.append((rel, 'd', t, 0 if fold else st.st_mode & 0o7777))
            else:
                bits = (0 if st.st_mode & stat.S_IWRITE else 1) if fold else st.st_mode & 0o7777
                out.append((rel, 'f', st.st_size, t, bits))
    return sorted(out)


def differences(a, b, via_windows=False, links=True):
    """What differs: metadata entries on one side only, and files whose
    contents differ; empty when the trees match."""
    ma, mb = meta(a, via_windows, links), meta(b, via_windows, links)
    sa, sb = set(ma), set(mb)
    found = [f'only in {a}: {x}' for x in ma if x not in sb] + [f'only in {b}: {x}' for x in mb if x not in sa]
    for dirpath, dirs, files in os.walk(a):
        for name in files:
            pa = os.path.join(dirpath, name)
            if os.path.islink(pa):
                continue
            pb = os.path.join(b, os.path.relpath(pa, a))
            if os.path.isfile(pb) and not filecmp.cmp(pa, pb, shallow=False):
                found.append(f'contents differ: {os.path.relpath(pa, a)}')
    return found


if __name__ == '__main__':
    args = sys.argv[1:]
    via = '--via-windows' in args
    links = '--no-links' not in args
    a, b = [x for x in args if not x.startswith('--')]
    found = differences(a, b, via, links)
    for line in found[:10]:
        print(line)
    if found:
        print(f'{len(found)} differences between {a} and {b}')
        sys.exit(1)
    print(f'{a} and {b}: identical')
