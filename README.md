# bettrsync

A small sync tool that does what `rsync -rltv --inplace` does, and does
it with every core on both ends.

    bettrsync [OPTIONS] SOURCE... DESTINATION

Directories, regular files and symbolic links under the sources are
copied into the destination, keeping modification times to the
nanosecond.  Symlinks are copied as links.  Permissions, owners and
groups are not copied: a new file gets the source's permission bits
under the receiving umask, an existing one keeps its own.  Nothing is
deleted.

A file the destination already has, of the same size and time, is left
alone.  One that differs is updated in place: both sides hash it in
blocks, at the same time, on all their cores, and only the blocks that
differ cross the wire, while the hashing goes on.  That is what makes a
VM disk image with a few hundred changed spots a matter of seconds.

Either end may be remote, `[user@]host:path`, over ssh, with bettrsync
installed on the host too.  Two local paths work without ssh.

## Dry run by default

Nothing is written without `--commit`.  The default run does all the
reading, hashing and comparing, lists what it would do, and ends with a
summary of how long each part took and what it would send:

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

    -n, --dry-run        read, hash and compare, report, write nothing (default)
        --commit         write to the destination
    -c, --checksum       compare the blocks of every file, not only those whose
                         size or modification time differs
    -q, --quiet          no listing, only the summary
    -v, --verbose        the listing (default)
    -B, --block-size N   the block size, K, M or G suffix (default 64K)
    -j, --threads N      threads for hashing and scanning (default: the cores)
    -e, --rsh COMMAND    how to reach a remote host (default: ssh)
        --remote-bin P   bettrsync on the remote host (default: bettrsync)

A source with a trailing slash contributes its contents, `src/`; without
it the directory itself goes into the destination, `dst/src`, as with
rsync.  A single file to a path that is not a directory is copied to
that name.

Smaller blocks send less when the changes are small and scattered: on a
4 GiB image with 200 changed spots of 4 KiB to 1 MiB, 16K blocks sent
71 MiB, 64K 80 MiB, 128K 93 MiB, 1M 261 MiB, and the hashing took the
same time at every size, since it is bound by reading the file.

`BETTRSYNC_TRACE=1` in the environment makes each thread print, at its
end, where its time went, on both sides (the remote side's lines come
back over ssh's stderr).

## How it works

The sender scans the sources with several threads and sends the list.
The receiver walks it in order, making directories and links, and
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
ssh pipe or a socket pair in one process.

## Building and testing

`build.sh` builds in the `rust:bullseye` Docker image when Docker is
there, so no Rust toolchain is needed on the build machine and the
binary runs on any glibc from 2.31 up; otherwise it runs `cargo build`.
`tests/smoke.sh BIN SCRATCHDIR` checks a tree with links and times, an
image changed in place, shrunk and grown.  `bench/bench.sh BIN USER@HOST
SCRATCHDIR [GIB] [CHANGES]` times `rsync --inplace` against bettrsync
on a changed image over ssh, page caches cold and hot on both sides,
verifying the copies after every write.  `bench/tree.sh BIN USER@HOST
SCRATCHDIR [FILES] [CHANGES]` does the same with a tree of many small
files against `rsync -rlt`, verifying paths, sizes, times, links and
contents.

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

A tree of 108,000 files of 512 B to 16 KiB in 211 directories with
1,000 links, 867 MiB, on the same two systems; a round changes 1,000
files and adds 500.  The receiver runs a few VMs, so these move by a
few tenths from run to run:

    rsync new tree, cold                   6.75 s
    bettrsync new tree, cold               3.62 s   (at 252 MiB/s, the wire)
    rsync, unchanged, hot                  0.63 s
    bettrsync, unchanged, hot              0.39 s
    bettrsync -c, unchanged, hot           0.80 s
    rsync, 1500 changed, cold              1.21 s
    rsync, 1500 changed, hot               0.50 s
    bettrsync, 1500 changed, cold          0.66 s
    bettrsync, 1500 changed, hot           0.55 s

The unchanged pass is 0.11 s of ssh startup, 0.03 s of scanning, and
about 2 µs an entry on each side for the exchange; `-c` on an unchanged
tree hashes every file on both sides without touching any of them.
