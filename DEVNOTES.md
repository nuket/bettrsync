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
- Up to 64 files in flight on each side, a slot freed at FileDone, so
  neither side runs out of descriptors on big trees.
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

## Not yet done

- Trees of many small files have not been benchmarked; the per-file
  protocol cost is untested at scale.
- Larger-than-RAM images on the receiving side, for a truly cold
  receiver.
- ssh is the only transport; at 2.5 Gbps and up one ssh stream's
  cipher may become the limit.  Measure before building anything.
