# bettrsync

A small sync tool that does what `rsync -rltpv --inplace` does, and
does it with every core on both ends.

    bettrsync [OPTIONS] SOURCE... DESTINATION

Directories, regular files and symbolic links under the sources are
copied into the destination, keeping modification times to the
nanosecond and permission bits exactly, on new and existing files and
directories alike; a file or directory that differs in its bits alone
is listed as `chmod` and counts as unchanged.  Symlinks are copied as
links.  Owners and groups are not copied.  Nothing is deleted unless
`--delete` says so.

A file the destination already has, of the same size and time, is left
alone.  One that differs is updated in place: both sides hash it in
blocks, at the same time, on all their cores, and only the blocks that
differ cross the wire, while the hashing goes on.  A destination file
its owner may not write (0444, or read-only on Windows) is given owner
write for the update and the source's bits after it.  That is what makes a
VM disk image with a few hundred changed spots a matter of seconds.

Either end may be remote, `[user@]host:path`, over ssh, with bettrsync
installed on the host too, on the PATH a non-interactive login shell
sees, or named with `--remote-bin`.  Two local paths work without ssh.
It runs on Linux, macOS and Windows, and any of them may be at either
end.

## Dry run by default

Nothing is written without `--commit`.  The default run does all the
reading, hashing and comparing, lists what it would do, and ends with a
summary of how long each part took and what it would send.  There is no
`-n` or `-v`, since that is already what a plain run does; rsync's
habits get an unknown-option error rather than a surprise:

    update   images/dev.vdi  1,165 of 65,536 blocks, 72.8 MiB

    scanned     0.11 s   1 entries: 0 dirs, 1 files (4.00 GiB), 0 links
    compared    1.15 s   hashed 4.00 GiB here, 4.00 GiB there (6.96 GiB/s)
    to send     1.15 s   72.8 MiB in 1,165 blocks of 64.0 KiB (63.3 MiB/s)
    total       1.26 s   unchanged 0, updated 1, new 0 files, 0 dirs, 0 links, failed 0
    dry run: nothing was written; add --commit to write

The same run with `--commit` writes, and says `sent` instead.  The
summary is the benchmark: compare `compared` and `total` between
versions, block sizes, and cache states.

## Options

        --commit         write to the destination (without it: read, hash,
                         compare and report, writing nothing)
    -c, --checksum       compare the blocks of every file, not only those whose
                         size or modification time differs
    -q, --quiet          no listing, only the summary
    -B, --block-size N   the block size, K, M or G suffix (default 64K)
    -j, --threads N      threads for hashing and scanning (default: the cores)
    -e, --rsh COMMAND    how to reach a remote host (default: ssh)
        --remote-bin P   bettrsync on the remote host (default: bettrsync)
        --exclude PAT    leave out what matches PAT; may be given more than once
        --exclude-from F read patterns from F, one a line; - for stdin
        --delete         remove from the destination what the source does not have

A source with a trailing slash contributes its contents, `src/`; without
it the directory itself goes into the destination, `dst/src`, as with
rsync.  A single file to a path that is not a directory is copied to
that name.

`--exclude` reads its pattern as rsync does.  A pattern without a slash
matches a name anywhere in the tree, `*.tmp`; with a slash it matches
the path under the destination, `build/cache`, anchored at the top if
it starts with a slash, `/top`, and otherwise anywhere on a directory
boundary.  A trailing slash matches directories only, `cache/`.  `*`
matches within a name, `**` across slashes, `?` one character, `[a-z]`
a class, `\` escapes.  An excluded directory is not entered at all.
`--exclude-from` reads the same patterns from a file, one a line, with
blank lines and lines starting with `#` or `;` skipped, and `-` for
standard input; the file is read on the side the command runs on.

`--delete` removes, from each directory the source has, whatever the
source does not: files, links, and directories with everything in
them.  It runs after the files are written and before the directories
are stamped.  Excluded things are left alone on both sides, so
`--exclude cache/ --delete` neither copies caches nor removes the ones
the destination has.  A dry run lists what would go, as `delete`.  If the
sending side had any warning, an unreadable directory for one, nothing
is deleted, since the list it sent may be short.

## On Windows

The same binary, `bettrsync.exe`, with the same options.  A path may be
spelled either way: `src/`, `.\src\`, `..\images`, `C:\VMs\` or
`C:/VMs/`; a trailing slash or backslash means the contents, as above.
A drive letter is not a host, so `C:\dst` is local, and a remote path
on a Windows host is written after the host's own colon,
`winbox:C:/Users/Max/dst` or `winbox:C:\Users\Max\dst`, or relative to
the ssh user's home, `winbox:dst`.  The remote command line is quoted
so that cmd (Windows' OpenSSH shell) and sh both read it; a `$` or
backtick in a path on a Windows host is the one thing that does not
survive it.  Exclude patterns use `/` whatever the side, since the
paths they match are the wire's.

Windows keeps modification times to 100 ns, so a Linux file's
nanoseconds are rounded there and the comparison allows for it both
ways.  The only permission bit it has is the read-only attribute: a
source file without owner write becomes read-only there, a read-only
source file is 0444 on Linux, and directories carry no bits.  Symbolic links
need a privilege (Developer Mode, or an elevated session) that an ssh
session does not have by default; without it a link is a warning and
counts as failed, and everything else goes through.  NTFS does not
tell `Foo` from `foo`, so `--delete` does not either there.  Names
that Windows cannot hold (`:`, `*`, `?` and so on) fail to be created
and are reported.

Smaller blocks send less when the changes are small and scattered: on a
4 GiB image with 200 changed spots of 4 KiB to 1 MiB, 16K blocks sent
71 MiB, 64K 80 MiB, 128K 93 MiB, 1M 261 MiB, and the hashing took the
same time at every size, since it is bound by reading the file.

`BETTRSYNC_TRACE=1` in the environment makes each thread print, at its
end, where its time went, on both sides (the remote side's lines come
back over ssh's stderr).

## How it works

The sender scans the sources with several threads and sends the
entries as it finds them, a directory at a time, each directory before
its contents.  The receiver walks them in order, making directories and
links, and
hands each file to a small pool that looks at the destination and
answers: skip, send whole, or update in place.  For an update it opens
the file and queues its blocks for hashing on a thread pool; each
chunk's hashes go back as soon as they are done.  The sender reads and
hashes its copy on its own pool, and a comparer thread matches each
chunk against the receiver's hashes as they arrive, queueing the
differing blocks, coalesced into runs, for the wire.  The receiver
writes them at their offsets, and the file pool then truncates, stamps
the time, and for a new file renames it into place, so the thread on
the wire never waits for the file system.  Up to 1,024 files are in
flight at a time, fewer if the descriptor limit does not allow it.

Hashes are xxh3, 128 bits, over blocks of `-B` bytes, read in chunks of
8 MiB so the reads are large and sequential.  The protocol is a few
message types with a tag byte and little-endian fields, the same over an
ssh pipe or a socket pair in one process; entries and decisions travel
in batches of up to 256, so a tree of a hundred thousand files is a few
hundred messages each way rather than a hundred thousand.

## Releases

GitHub Actions build and test every push on Linux, macOS and Windows
(`.github/workflows/ci.yml`), and a tag `vX.Y.Z` that matches the
version in `Cargo.toml` makes a release (`release.yml`) with, for Linux
x86_64 and aarch64, a tar.gz, a `.deb` and an `.rpm`, built in the
same Debian 11 image as `build.sh` so the binary runs on any glibc
from 2.31 up; for macOS one universal binary in a tar.gz; for Windows
a zip; and a `SHA256SUMS`.  The packages are made by
`packaging/package.sh BINARY amd64|arm64 OUTDIR` with nothing but
`dpkg-deb` and `rpmbuild`, which also works on a desk.  Run by hand
from the Actions tab, the release workflow builds the same packages
and keeps them as artifacts without publishing anything.

    sudo dpkg -i bettrsync_0.1.0_amd64.deb      # Debian, Ubuntu
    sudo rpm -i bettrsync-0.1.0-1.x86_64.rpm    # Fedora, RHEL, SUSE

## Building and testing

`build.sh` builds in the `rust:bullseye` Docker image when Docker is
there, so no Rust toolchain is needed on the build machine and the
binary runs on any glibc from 2.31 up; otherwise it runs `cargo build`.
On macOS `build.sh` runs cargo directly, and the binary is like the
Linux one in every way that matters here.  `build.bat` does the same
on Windows with rustup's MSVC toolchain, the C runtime linked in so the
binary needs nothing beside it.  The only
crates are xxhash-rust for the hashes and, on Unix, libc for the few
calls std does not have; the Windows side is std alone.
`tests/smoke.sh BIN SCRATCHDIR` checks a tree with links and times, an
image changed in place, shrunk and grown, the remote code path through
a stand-in for ssh, a tree of thousands of files changed every way at
once with unreadable files and type changes, and `-c`; the debug build
asserts the protocol's invariants, so run it on that too.  `bench/bench.sh BIN USER@HOST
SCRATCHDIR [GIB] [CHANGES]` times `rsync --inplace` against bettrsync
on a changed image over ssh, page caches cold and hot on both sides,
verifying the copies after every write.  `bench/tree.sh BIN USER@HOST
SCRATCHDIR [FILES] [CHANGES]` does the same with a tree of many small
files against `rsync -rlt`, verifying paths, sizes, times, links and
contents.  `tests/smoke.py BIN SCRATCHDIR` is the smoke test in Python,
which runs on macOS and Windows too (the shell one wants GNU find,
stat and head), with `tests/fakessh.py` standing in for ssh through the
system's own shell; it also covers the spellings of a path and the
read-only attribute on Windows.

On a 4 GiB image with 200 changed spots, over a 1 Gbps link from a
6-core VM to a 16-core desktop:

    rsync --inplace, cold                  5.85 s
    rsync --inplace, hot                   5.06 s
    bettrsync dry run, cold                2.55 s
    bettrsync dry run, hot                 1.58 s
    bettrsync --commit, cold               3.84 s
    bettrsync --commit, hot                1.94 s

The same image and changes between two systems at 2.5 GbE, with the
whole page cache dropped on both sides before each cold run.  iperf3
measures the link at 2.35 Gbit/s and one ssh stream carries 275 MB/s
either way, so the whole-file copy is at the ssh ceiling:

    bettrsync new file, cold              14.90 s   (4 GiB at 277 MiB/s)
    rsync --inplace, cold                  6.02 s
    rsync --inplace, hot                   3.28 s
    bettrsync dry run, cold                1.07 s
    bettrsync dry run, hot                 0.68 s
    bettrsync --commit, cold               1.47 s
    bettrsync --commit, hot                1.12 s
    rsync, unchanged, hot                  0.16 s
    bettrsync, unchanged, hot              0.12 s
    bettrsync -c, unchanged, hot           0.67 s

Pulling works at the same rate: the 4 GiB file came back at 277 MiB/s,
and `-c` on an unchanged hot copy compared both sides in 0.64 s.

A tree of 110,000 files of 512 B to 16 KiB in 211 directories with
1,000 links, 882 MiB, on the same two systems; a round changes 1,000
files and adds 500.  The receiver runs a few VMs, so these move by a
few tenths from run to run:

    rsync new tree, cold                   7.11 s
    bettrsync new tree, cold               3.49 s   (at 252 MiB/s, the wire)
    rsync, unchanged, hot                  0.70 s
    bettrsync, unchanged, hot              0.44 s
    bettrsync -c, unchanged, hot           0.97 s
    rsync, 1500 changed, cold              1.27 s
    rsync, 1500 changed, hot               0.51 s
    bettrsync, 1500 changed, cold          0.45 s
    bettrsync, 1500 changed, hot           0.32 s

Run on their own, three times each, the unchanged pass takes 0.30 to
0.32 s, of which ssh startup is 0.11 s and the scan 0.03 s, and `-c`
takes 0.52 to 0.58 s, hashing every file on both sides without
touching any of them.

A million files, 7.87 GiB in 2,101 directories with 10,000 links; a
round changes 6,666 files and adds 3,334:

    rsync new tree, cold                  47.07 s
    bettrsync new tree, cold              30.03 s   (279 MiB/s, the wire)
    rsync, unchanged, hot                  4.66 s
    bettrsync, unchanged, hot              1.52 s
    bettrsync -c, unchanged, hot           3.50 s
    rsync, 10000 changed, cold             9.37 s
    rsync, 10000 changed, hot              3.92 s
    bettrsync, 10000 changed, cold         2.76 s
    bettrsync, 10000 changed, hot          2.04 s

The sender keeps the list of entries until each is decided, about
300 bytes each: 310 MiB for the million; the receiver peaked at 89
MiB.

## License

MIT; see LICENSE.
