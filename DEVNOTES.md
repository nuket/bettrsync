# Development notes

What a new session needs to know that the code and the git history do
not say.  README.md says what the tool does; this says how the work is
done and what was found.

## Working the tree

- Dry run by default, `--commit` to write: that is the house style for
  tools here, keep it.
- `LOG.md` (the conversation log) and `systems.yaml` (the fleet file
  for ../Shove/systems-*.py) are never committed to main.  They are
  listed in `.git/info/exclude` and live only on the local `log-md`
  branch, which holds just those two files.  `tools/snapshot-log.sh`
  updates that branch through a scratch index without checking it
  out.  Never check `log-md` out: it would pull `LOG.md` from under
  the editor.  Snapshot it whenever main gets a milestone commit.
- `build.sh` builds in the `rust:bullseye` Docker image when Docker is
  present (so the binary runs on glibc 2.31 up), else with the local
  cargo.  `BETTRSYNC_GIT_REVISION` stamps `--version`.
- `tests/smoke.sh BIN SCRATCHDIR` is the regression test; run it after
  every change.  It needs a few hundred MB of scratch.
- `bench/bench.sh BIN USER@HOST SCRATCHDIR [GIB] [CHANGES]` is the
  benchmark against `rsync --inplace`; the cold runs need
  `sudo -n tee /proc/sys/vm/drop_caches` without a password on both
  machines (a sudoers.d line), else only the image's pages are evicted.
  `bench/tree.sh BIN USER@HOST SCRATCHDIR [FILES] [CHANGES]` is the
  same for a tree of many small files, against `rsync -rlt`.  Both
  take `BENCH_SSH` and `BENCH_REMOTE_BIN`.
- `BETTRSYNC_TRACE=1` makes every thread print where its time went,
  on both sides; put it in `--remote-bin "BETTRSYNC_TRACE=1 bettrsync"`
  for the far side.  Read the "sender pool reading" figure first when a
  run is slow: it tells disk from everything else.
- Commit to main as each milestone works.

## Design decisions

- Fixed-offset blocks, not rsync's rolling checksum: the files that
  matter are VM disk images, which change in place.  Both sides hash
  the same offsets at once; nothing is found that has shifted.
- xxh3-128 per block, 64K blocks by default, read in 8 MiB chunks.  A
  sweep over a 4 GiB image with 200 changed spots (4 KiB to 1 MiB)
  found hashing read-bound at every block size, so smaller blocks only
  send less: 16K 71 MiB, 32K 74, 64K 80, 128K 93, 1M 261.
- Existing files are updated in place; new files go to a temp name
  beside them and are renamed at the end.
- Up to 1,024 files in flight on each side, a slot freed at FileDone,
  so neither side runs out of descriptors on big trees.  main raises
  the soft descriptor limit to make room for twice that (both sides
  may be one process) and lowers the window if the hard limit stops
  it.  It was 64, which held 512 KiB of 8 KiB files: less than the
  2 ms of pipeline between the sides at 275 MB/s, so a tree of small
  files ran at 29,000 files/s with the wire half idle.
- The receiver opens, creates and finishes files on a pool of up to 8
  threads, not on the thread reading the wire: at one thread the file
  system's create-write-stamp-rename cycle was the ceiling, 26,000
  files/s on Wonderslice; at four it does 75,000, and more threads do
  not help (ext4 serializes on the directory).  Decisions therefore
  reach the sender in roughly, not exactly, scan order, and so does
  the listing.  Directories and links are still made in order, by the
  decider, before the files in them are handed out.
- A file the receiver opened for a delta but received nothing for,
  whose size and time already matched, is closed as it was: no
  truncate, no stamp.  Re-stamping 104k inodes on a `-c` pass cost
  nothing in that pass but left the receiver writing back metadata
  for seconds, and any pass over the tree in that time took a second
  longer (0.8 s became 1.9).
- No permissions, owner, group, or `--delete`.  Nanosecond mtimes,
  compared to the nanosecond (second-only comparison skipped a file
  changed within the second it was made).

## Findings so far (2026-10-20, 6-core VM sending to a Ryzen 9 7950X)

4 GiB image, 200 changed spots, 1 GbE: rsync --inplace 5.9 s cold /
5.1 s hot; bettrsync --commit 3.8 / 1.9; dry run 2.6 / 1.6.  2 GiB
image (fits the VM's cache), USB 2.5 GbE: rsync 4.5 / 3.0; bettrsync
5.6 / 0.87; dry run 1.8 / 0.40.  Whole-file copies run at the ssh
ceiling.  Unchanged files cost 0.12 s.

The VM was a poor benchmark host, which is why development moved to
bare metal: its USB 2.5 GbE passthrough capped at ~175 MB/s out and
~280 MB/s in, and while any traffic crossed it the VM's disk reads fell
from 2.0 GB/s to ~230 MB/s (VirtualBox's USB and disk emulation
contend), so disk and network could not overlap there.  Its 5.4 GiB
of RAM never kept a 4 GiB image cached.

## Findings on bare metal (2026-10-20, two systems at 2.5 GbE)

The sender is a 32-thread box with 30 GiB; the receiver (Wonderslice,
claude@192.168.178.170, id_testkey) has 16 cores and 58 GiB.  Both
drop the whole page cache before a cold run (sudoers on both).  The
link: iperf3 2.35 Gbit/s, one ssh stream 275 MB/s either way, the same
with aes128-gcm, so at this speed ssh is wire-bound, not cipher-bound.

4 GiB image, 200 changed spots: rsync --inplace 6.0 s cold / 3.3 hot;
bettrsync --commit 1.5 / 1.1; dry run 1.1 / 0.68; -c on an unchanged
hot file 0.67; unchanged 0.12.  Whole-file copy 277 MiB/s, push and
pull alike, which is the ssh ceiling.  Hot compares hash both 4 GiB
copies at 14 GiB/s aggregate, cold at 6 to 9 GiB/s, so the delta runs
are bound by reading the file on each side; the wire is idle most of
the time.  Evicting only the image's pages (dd iflag=nocache) gave the
same cold numbers as dropping the whole cache, within 0.1 s.

A rough view of where the time goes now: rsync's hot 3.3 s is its
single-threaded rolling checksum over 4 GiB; bettrsync's hot 1.1 s is
the scan (0.11 s, mostly ssh startup) plus a read of both copies in
parallel, with the 70 to 90 MiB of changed blocks going over while
hashing continues.

### Trees of small files (bench/tree.sh)

102,000 files of 512 B to 16 KiB in 211 directories, 1,000 links,
820 MiB; a round rewrites 1,000 files and adds 500.  Before the file
pool and the larger window: new tree 4.10 s cold, 1,500 changed 1.22
cold / 0.45 hot.  The trace showed the receiver's wire-reading thread
spending 2.45 s of 3.8 finishing files (truncate, stamp, close,
rename) and the decider 2.9 s creating them, both single-threaded;
moving both onto a file pool alone gave 3.55 s, because the window of
64 files was then the limit (the decider waited 3.1 s for slots).  With
1,024 in flight: new tree 3.57 s cold at 252 MiB/s, the sender's writer
flushing 3.1 s of 3.3, so the wire; 1,500 changed 0.58 cold / 0.31 hot;
unchanged 0.37 hot; `-c` on an unchanged tree 0.88.  rsync: new tree
5.82, changed 1.09 / 0.53, unchanged 0.64.  The big-image numbers did
not move (old and new binaries alternated, cold `-c`, within 0.02 s).

A directory's own size (the `%s` of `find` on it) differs between
trees filled serially and in parallel; the tree fingerprint leaves it
out.

The per-file cost of the passes that send nothing, hot, 105k entries:
the unchanged pass is 0.37 to 0.46 s, of which ssh startup is 0.11 and
the scan 0.03, so the exchange of 105k entries and 105k decisions costs
about 2 µs an entry on each side (local to local on a socket pair:
0.19 s in all).  Nothing in it dominates: each entry is one message of
eight reads on a buffered reader and two allocations, a mutex and a
condvar wake into the decider, a boxed job for the file pool (the stat
is 5.6 µs there; on the decider alone it would be 0.58 s), and one
decision message back.  Batching entries and decisions into messages
of a few hundred would take out most of it, for 0.1 s at best; rsync
takes 0.64 s for the same pass.  The `-c` pass is 0.6 to 0.9 s: the
receiver's pool spends 8 µs a file opening and queueing the hash and
2 µs closing, the sender's comparer 2.3 µs a result, and the window of
1,024 is full the whole time (the decider waits for a slot 0.4 of 0.5
s), so it is bound by the round trip through open, hash, compare,
FileDone and close rather than by any one thread.

## Not yet done

- Trees of many small files: a new tree goes at the wire; the passes
  that send nothing are about 2 µs an entry a side in protocol work
  (see the findings), which batching entries and decisions could cut
  by perhaps 0.1 s on 100k files.  Two sources with the same relative
  path in both now race on the receiver's file pool where the later
  one used to win.
- Larger-than-RAM images on the receiving side, for a truly cold
  receiver.
- ssh is the only transport.  At 2.5 GbE one stream carries 275 MB/s
  of a 294 MB/s wire, cipher choice makes no difference, so it is not
  the limit there; at 10 GbE it will be.  Measure before building.
