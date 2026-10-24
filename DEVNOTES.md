# Development notes

What a new session needs to know that the code and the git history do
not say. README.md says what the tool does; this says how the work is
done and what was found.

## Working the tree

- Dry run by default, `--commit` to write: that is the house style for
  tools here, keep it.
- `LOG.md` (the conversation log) and `systems.yaml` (the fleet file
  for ../Shove/systems-*.py) are never committed to main. They are
  listed in `.git/info/exclude` and live only on the local `log-md`
  branch, which holds just those two files. `tools/snapshot-log.sh`
  updates that branch through a scratch index without checking it
  out. Never check `log-md` out: it would pull `LOG.md` from under
  the editor. Snapshot it whenever main gets a milestone commit.
- `build.sh` builds in the `rust:bullseye` Docker image when Docker is
  present (so the binary runs on glibc 2.31 up), else with the local
  cargo. `BETTRSYNC_GIT_REVISION` stamps `--version`.
- `tests/smoke.sh BIN SCRATCHDIR` is the regression test; run it after
  every change, on the release build and on `build.sh --debug`'s,
  which has the debug assertions on the protocol's invariants (data
  only for an open file, no FileDone after Finish, every slot given
  back by Finish, one decision an entry, nothing in flight at Finish).
  It needs a few hundred MB of scratch. `tests/fakessh.sh` stands in
  for ssh there, so the server mode, pipes and relayed stderr run in
  every test run without a network.
- `bench/bench.sh BIN USER@HOST SCRATCHDIR [GIB] [CHANGES]` is the
  benchmark against `rsync --inplace`; the cold runs need
  `sudo -n tee /proc/sys/vm/drop_caches` without a password on both
  machines, else only the image's pages are evicted. The rule, one
  line in `/etc/sudoers.d/drop-caches` (mode 440, checked with
  `visudo -cf`), for the account that runs or receives the bench:

      max ALL=(root) NOPASSWD: /usr/bin/tee /proc/sys/vm/drop_caches

  sudoers matches the arguments exactly, so it allows nothing but that
  one write. Both bench machines have it.
- `bench/tree.sh BIN USER@HOST SCRATCHDIR [FILES] [CHANGES]` is the
  same for a tree of many small files, against `rsync -rlt`. Both
  benches take `BENCH_SSH` and `BENCH_REMOTE_BIN`.
- `BETTRSYNC_TRACE=1` makes every thread print where its time went,
  on both sides; put it in `--remote-bin "BETTRSYNC_TRACE=1 bettrsync"`
  for the far side. Read the "sender pool reading" figure first when a
  run is slow: it tells disk from everything else.
- Windows: `python3 ../Shove/systems-build.py -f systems.yaml build
  dev-windows-10` syncs the tree (a tar over ssh) to
  `%USERPROFILE%\devsrc\bettrsync` on the Windows 10 dev VM
  (192.168.56.111, user max, id_testkey, cmd as the ssh shell, cargo
  1.98 MSVC on the PATH, Python 3.13) and runs `build.bat`, the target
  `bettrsync-windows`, fetching `bettrsync.exe` into `artifacts/`.
  `tests/smoke.py BIN SCRATCHDIR` is the smoke test that runs there
  (and on Linux), `tests/fakessh.py` its ssh stand-in through cmd.
  The ssh sessions on the Windows VMs are not elevated, so no symlink
  can be made in them: the smoke test skips its link checks and a
  link sent there is a warning. test-windows-11 (192.168.56.133, no
  Python, only the Store stub) and test-ubuntu-2604 (192.168.56.122)
  are the far ends of the cross-platform runs; both Windows VMs and
  dev-ubuntu-2604 have id_testkey in their `.ssh` too, so each can be
  the client against the Linux VMs. Run both smoke tests, both
  builds, on both platforms after a change that touches `fsutil.rs`.
  A run with Windows as the client (bettrsync.exe spawning ssh.exe)
  cannot be driven over ssh: Windows' ssh.exe, started inside an sshd
  session with no console, never forwards a piped stdin, so the
  handshake deadlocks (`echo x | ssh host cat` hangs there too). In
  the desktop session it works, so those runs go through
  `systems-deploy.py start dev-windows-10 --as cross <batch file>`,
  which runs it in the logged-in session with its output in
  `%USERPROFILE%\bettrsync\cross.log`.
- macOS: the Mac mini (macmini in systems.yaml, 192.168.178.60, max,
  id_testkey; Apple Silicon, macOS 27, cargo 1.99 in ~/.cargo/bin,
  which non-interactive ssh sees) builds with
  `python3 ../Shove/systems-build.py -f systems.yaml build macmini`;
  build.sh runs cargo directly there. The name must have no space,
  as systems-build.py puts it in ssh's ControlPath. `tests/smoke.sh`
  does not run on BSD userland (head -c 256M, find -printf, stat -c):
  `tests/smoke.py` is the Mac's smoke test, links included. When the
  Mac is off the network, the Headless2Go KVM on the control system
  (systems-kvm.py: shot, wake, key, type, and a transfer pipe once
  its agent runs there) is the way in; colour bars in a shot mean the
  display is asleep, `wake` lights it. It needed no code changes.
- As few crates as possible, the cost of each weighed against what it
  brings: xxhash-rust for the hashes and libc on Unix is the whole
  list; the Windows side is std with the few Win32 constants it needs
  written out, rather than windows-sys.
- Commit to main as each milestone works.

## How it works

The sender scans the sources on several threads and sends the entries
as it finds them, a directory at a time, each directory before its
contents. The receiver walks them in order, making directories and
links, and hands each file to a small pool that looks at the
destination and answers: skip (size and time match), send whole
(nothing there), or update in place.

For an update, both sides hash the file in blocks on their own thread
pools, the receiver's hashes going back as each chunk is done; a
comparer thread on the sender matches them as they arrive and queues
the differing blocks, coalesced into runs, for the wire. The receiver
writes them at their offsets, and its file pool then truncates, stamps
the time and, for a new file, renames it into place, so the thread on
the wire never waits for the file system. Up to 1,024 files are in
flight at a time, fewer if the descriptor limit does not allow it.

Hashes are xxh3, 128 bits, over blocks of `-B` bytes, read in chunks
of 8 MiB so the reads are large and sequential. The protocol is a few
message types with a tag byte and little-endian fields, the same over
an ssh pipe or a socket pair in one process; entries and decisions
travel in batches of up to 256, so a tree of a hundred thousand files
is a few hundred messages each way rather than a hundred thousand.
The remote command line is quoted so that cmd (Windows' OpenSSH
shell) and sh both read it; `$` and backtick are escaped for sh, which
is why they do not survive on a Windows host. Exclude patterns match
the wire's paths, hence `/` on every side.

The only crates are xxhash-rust for the hashes and, on Unix, libc for
the few calls std does not have; the Windows side is std alone.

## Releases and packaging

GitHub Actions build and test every push on Linux, macOS and Windows
(`.github/workflows/ci.yml`; the Linux leg also runs the packaging
and signing scripts with a throwaway key). A tag `vX.Y.Z` that
matches `Cargo.toml` makes a release (`release.yml`): the Linux
binaries come out of `build.sh` in the `rust:bullseye` image on the
amd64 and arm64 runners and are packaged on the runner by
`packaging/package.sh BINARY amd64|arm64 OUTDIR`, which needs nothing
but `dpkg-deb` and `rpmbuild` and writes the control file and spec
from `Cargo.toml` (bullseye's own apt is past its day, so nothing is
installed inside the image); the Arch package is built from the
source tarball by makepkg in the `archlinux` image through
`packaging/arch.sh OUTDIR`, which runs makepkg directly on an Arch
system; macOS is two cargo targets joined by lipo; Windows a zip by
7z. Both packaging scripts work on a desk too. Run by hand from the
Actions tab, the workflow builds the same packages and keeps them as
artifacts without publishing anything. The workflows pass the short
commit hash as `BETTRSYNC_GIT_REVISION`, as `build.sh` does locally.

## Signing

The release key lives in ../bettrsync-keystore (a folder shared in
from the host, outside every repository), made once by make-key.sh
there: RSA 4096, sign only, three years. packaging/sign.sh signs a
directory of release files with it, from GPG_SIGNING_KEY in the
environment (the workflow) or the keyring GNUPGHOME names (by hand),
and picks the key by the fingerprint of packaging/bettrsync-signing-key.asc,
so the key that signs is always the one the repo publishes. RPMs get
the signature embedded with rpmsign (Ubuntu's rpm package has it; the
runner installs it), everything else a detached one; pacman wants a
binary .sig beside the package, and once there is one it insists on
knowing the key, hence the pacman-key lines in the README. The
workflow signs only when the secret is set (a job-level env from the
secrets context, since step `if` cannot read secrets); without it a
release is unsigned and has only `SHA256SUMS`. By hand:
`GNUPGHOME=../bettrsync-keystore/gnupg packaging/sign.sh DIR` over the
files downloaded from a release, then upload the signatures, the
`.rpm` files (signing changes them) and `SHA256SUMS`; the keystore's
README has the commands. sign.sh checks every signature with nothing
but the public key before it ends.

## Design decisions

- The scan streams: the lister thread sends entries a directory at a
  time as the walk finds them, a directory always before its contents,
  and the main thread feeds files from the first decision on. The
  list is no longer sorted as a whole (within a directory it is), so
  the listing comes out in walk order, and two sources with the same
  destination path are resolved first come, with a warning, instead of
  first in sort order. On NVMe with a hot cache the scan is 0.03 s and
  this is worth 0.04 s; it is for the cold spinning disk with a million
  files, where the receiver used to idle through the whole scan.
- Fixed-offset blocks, not rsync's rolling checksum: the files that
  matter are VM disk images, which change in place. Both sides hash
  the same offsets at once; nothing is found that has shifted.
- xxh3-128 per block, 64K blocks by default, read in 8 MiB chunks. A
  sweep over a 4 GiB image with 200 changed spots (4 KiB to 1 MiB)
  found hashing read-bound at every block size, so smaller blocks only
  send less: 16K 71 MiB, 32K 74, 64K 80, 128K 93, 1M 261.
- Existing files are updated in place; new files go to a temp name
  beside them and are renamed at the end.
- Up to 1,024 files in flight on each side, a slot freed at FileDone,
  so neither side runs out of descriptors on big trees. main raises
  the soft descriptor limit to make room for twice that (both sides
  may be one process) and lowers the window if the hard limit stops
  it. It was 64, which held 512 KiB of 8 KiB files: less than the
  2 ms of pipeline between the sides at 275 MB/s, so a tree of small
  files ran at 29,000 files/s with the wire half idle.
- The receiver opens, creates and finishes files on a pool of up to 8
  threads, not on the thread reading the wire: at one thread the file
  system's create-write-stamp-rename cycle was the ceiling, 26,000
  files/s on Wonderslice; at four it does 75,000, and more threads do
  not help (ext4 serializes on the directory). Decisions therefore
  reach the sender in roughly, not exactly, scan order, and so does
  the listing. Directories and links are still made in order, by the
  decider, before the files in them are handed out.
- A file the receiver opened for a delta but received nothing for,
  whose size and time already matched, is closed as it was: no
  truncate, no stamp. Re-stamping 104k inodes on a `-c` pass cost
  nothing in that pass but left the receiver writing back metadata
  for seconds, and any pass over the tree in that time took a second
  longer (0.8 s became 1.9).
- The sender keeps a file in flight until its FileDone is on the
  channel, since Finish goes out once nothing is in flight and the
  receiver stamps the directories at Finish: a FileDone after it meant
  one more rename into a directory already stamped. Seen once, on the
  last directory of a 106k-file tree, as a 43 s newer mtime.
- Entries go in messages of 256; the receiver's writer folds whatever
  decisions it finds queued together into one message (the decider
  and the file pool still answer one at a time). One message an
  entry each way cost 2 µs an entry a side over ssh: 0.1 to 0.2 s off
  the unchanged pass on 110k files and 0.35 s off `-c`, where the
  decisions were a larger share of the traffic. Local to local, on a
  socket pair, it made no difference.
- `--exclude` prunes the sender's scan (an excluded directory is never
  read) and protects on the receiver's `--delete`; the same patterns
  go to both sides, so pulling excludes too. `--exclude-from` is read
  where the command runs and its patterns join that list, so the remote
  end never needs the file. The matcher is rsync's reading of a
  pattern, in `filter.rs`, with unit tests. `--delete`
  runs on the receiver at Finish, from the set of every path the sender
  listed (a HashSet, 50 to 100 MB for a million), over each directory
  the sender listed, after the last rename and before the directory
  stamps. Finish carries the sender's warning count: with any, nothing
  is deleted, as rsync does on an I/O error. Listing lines for deletes
  travel as Report messages to the sender, which prints them.
- Permission bits (the 0o7777 of the source's mode) are copied
  exactly: a file's at its finish (a new one is created under the
  umask first), a directory's at Finish with its time, so a read-only
  source directory does not stop its own contents from being written;
  an existing destination directory without owner rwx is opened up
  (u+rwx) when the receiver reaches it and given its proper bits at
  Finish, as rsync does. (Found by the test with an unreadable source
  directory: the first run copied its 000, the next could not write
  into it.)  A file or directory already right but for its bits gets a chmod and
  a `chmod` listing line, and counts as unchanged. No owner or group;
  nothing is asked of the umask. Nanosecond mtimes,
  compared to the nanosecond (second-only comparison skipped a file
  changed within the second it was made).

- Windows (2026-10-21). Everything the platforms do differently is
  in `fsutil.rs`, behind one set of functions with a `cfg(unix)` and a
  `cfg(windows)` body each; the rest of the code calls those and knows
  nothing of either. Names travel on the wire as bytes, the file
  system's own on Unix and WTF-8 (UTF-8 for any real name) on Windows,
  with `/` between the parts; a Linux name that is not UTF-8 gets
  replacement characters on Windows. Link targets go over with `/`
  and come back with `\` on Windows, where a link also has to say
  whether it leads to a directory, so the target is looked at first.
  Times: NTFS keeps 100 ns ticks, so a comparison on Windows is to the
  tick (both sides' nanoseconds divided by 100); a Linux file's time
  is rounded down there and matches on the next run. Bits: Windows
  has the read-only attribute and nothing else, so `mode_of` reads a
  file as 0o444 or 0o644 and a directory as 0o755, and `wanted_mode`
  folds the source's mode the same way before the comparison, else a
  Linux 0o755 would be a chmod every run. A destination file its
  owner may not write (0444 on Linux, read-only on Windows) cannot be
  opened for the in-place update, so `clear_readonly` gives it owner
  write first and the finish sets the source's bits, on both
  platforms; on Windows the same clearing goes before a delete. (The
  Python smoke test found the Linux case: the shell one never wrote
  to a 0444 file.)  `--delete` compares names
  folded to lower case on Windows, since NTFS does not tell `Foo` from
  `foo` and would otherwise delete what was just written. Offset
  reads and writes are `seek_read`/`seek_write`, which take the offset
  each call, so the threads sharing a `File` do not disturb each
  other. The server end takes the standard handles as raw `File`s on
  both platforms (binary, no line writer in the way); two local paths
  run over a socket pair on Unix and a pipe each way on Windows.
  Descriptor limits are not a thing on Windows, so `allow_open_files`
  grants what is asked.
- Paths on the command line. A trailing `/`, and on Windows a
  trailing `\`, means the contents, as does a name with no base name
  (`.`, `..`, `C:`, `C:\`). `[user@]host:path` is remote when the
  colon comes before any separator, except on Windows a single letter
  before it, `C:\x` or `c:/x`, which is a drive. The remote command
  line is quoted for sh and cmd at once: double quotes, `\"` for a
  quote inside, a backslash run before a quote or at the end doubled
  (the MSVC argument rule, which sh's double quotes happen to read the
  same way), `$` and backtick escaped for sh; on a Windows host those
  two arrive with their backslash, the one thing the scheme cannot
  do. cmd says 9009 for a program it cannot find, as sh says 127;
  both are read as "not found". Patterns stay `/`-separated: they
  match the wire's paths.
- The Windows binary is built with `crt-static` (`.cargo/config.toml`)
  so it needs no Visual C++ redistributable on the machine it runs on.

## Findings so far (2026-10-20, 6-core VM sending to a Ryzen 9 7950X)

4 GiB image, 200 changed spots, 1 GbE: rsync --inplace 5.9 s cold /
5.1 s hot; bettrsync --commit 3.8 / 1.9; dry run 2.6 / 1.6. 2 GiB
image (fits the VM's cache), USB 2.5 GbE: rsync 4.5 / 3.0; bettrsync
5.6 / 0.87; dry run 1.8 / 0.40. Whole-file copies run at the ssh
ceiling. Unchanged files cost 0.12 s.

The VM was a poor benchmark host, which is why development moved to
bare metal: its USB 2.5 GbE passthrough capped at ~175 MB/s out and
~280 MB/s in, and while any traffic crossed it the VM's disk reads fell
from 2.0 GB/s to ~230 MB/s (VirtualBox's USB and disk emulation
contend), so disk and network could not overlap there. Its 5.4 GiB
of RAM never kept a 4 GiB image cached.

## Findings on bare metal (2026-10-20, two systems at 2.5 GbE)

The sender is a 32-thread box with 30 GiB; the receiver (Wonderslice,
claude@192.168.178.170, id_testkey) has 16 cores and 58 GiB, and runs
VirtualBox VMs that take about two cores between them, which is why
its small-file figures wander by a few tenths of a second from run to
run. Both
drop the whole page cache before a cold run (sudoers on both). The
link: iperf3 2.35 Gbit/s, one ssh stream 275 MB/s either way, the same
with aes128-gcm, so at this speed ssh is wire-bound, not cipher-bound.

4 GiB image, 200 changed spots: rsync --inplace 6.0 s cold / 3.3 hot;
bettrsync --commit 1.5 / 1.1; dry run 1.1 / 0.68; -c on an unchanged
hot file 0.67; unchanged 0.12. Whole-file copy 277 MiB/s, push and
pull alike, which is the ssh ceiling. Hot compares hash both 4 GiB
copies at 14 GiB/s aggregate, cold at 6 to 9 GiB/s, so the delta runs
are bound by reading the file on each side; the wire is idle most of
the time. Evicting only the image's pages (dd iflag=nocache) gave the
same cold numbers as dropping the whole cache, within 0.1 s.

A rough view of where the time goes now: rsync's hot 3.3 s is its
single-threaded rolling checksum over 4 GiB; bettrsync's hot 1.1 s is
the scan (0.11 s, mostly ssh startup) plus a read of both copies in
parallel, with the 70 to 90 MiB of changed blocks going over while
hashing continues.

### Trees of small files (bench/tree.sh)

102,000 files of 512 B to 16 KiB in 211 directories, 1,000 links,
820 MiB; a round rewrites 1,000 files and adds 500. Before the file
pool and the larger window: new tree 4.10 s cold, 1,500 changed 1.22
cold / 0.45 hot. The trace showed the receiver's wire-reading thread
spending 2.45 s of 3.8 finishing files (truncate, stamp, close,
rename) and the decider 2.9 s creating them, both single-threaded;
moving both onto a file pool alone gave 3.55 s, because the window of
64 files was then the limit (the decider waited 3.1 s for slots). With
1,024 in flight: new tree 3.57 s cold at 252 MiB/s, the sender's writer
flushing 3.1 s of 3.3, so the wire; 1,500 changed 0.58 cold / 0.31 hot;
unchanged 0.37 hot; `-c` on an unchanged tree 0.88. rsync: new tree
5.82, changed 1.09 / 0.53, unchanged 0.64. The big-image numbers did
not move (old and new binaries alternated, cold `-c`, within 0.02 s).

A directory's own size (the `%s` of `find` on it) differs between
trees filled serially and in parallel; the tree fingerprint leaves it
out.

The per-file cost of the passes that send nothing, hot, 105k entries,
before batching: the unchanged pass was 0.37 to 0.46 s, of which ssh
startup is 0.11 and the scan 0.03, so the exchange of 105k entries and
105k decisions cost about 2 µs an entry on each side (local to local
on a socket pair: 0.19 s in all). Nothing in it dominated: each entry
was one message of eight reads on a buffered reader and two
allocations, a mutex and a condvar wake into the decider, a boxed job
for the file pool (the stat is 5.6 µs there; on the decider alone it
would be 0.58 s), and one decision message back. The `-c` pass was
0.6 to 0.9 s: the receiver's pool spends 8 µs a file opening and
queueing the hash and 2 µs closing, the sender's comparer 2.3 µs a
result, and the window of 1,024 is full the whole time, so it is bound
by the round trip through open, hash, compare, FileDone and close
rather than by any one thread.

Batched (entries in 256s, decisions as the writer finds them), 110k
entries, alternated with the unbatched binary three times: unchanged
0.39 to 0.56 s became 0.30 to 0.32; `-c` 0.89 to 0.91 became 0.52 to
0.58. The receiver now reads 437 messages instead of 111k, the sender
19k instead of 209k. What is left of the unchanged pass after ssh and
the scan is 0.18 s, in which the file pool's stats take 0.58 thread-
seconds of the 1.6 available and both readers wait on each other: the
pipeline, not a thread.

Streaming the scan, same tree, alternated three times: unchanged cold
0.44 s became 0.40, hot 0.31 to 0.33 became 0.26 to 0.31. The window
for `-c` at 1,024, 2,048 and 4,096, three rounds each: 0.55 to 0.58,
0.53 to 0.58, 0.49 to 0.59, so nothing to choose, and it stays 1,024.
The ssh connection is made once a run, whatever the tree: the 0.11 s
is one key exchange and login, and ControlMaster in the user's ssh
config would make it about 10 ms.

### A million files (2026-10-21)

`bench/tree.sh ... 1000000 10000`: 1,000,000 files of 512 B to 16 KiB
in 2,101 directories, 10,000 links, 7.87 GiB; a round rewrites 6,666
files and adds 3,334. Every write verified.

    rsync new tree, cold                  47.07 s
    bettrsync new tree, cold              30.03 s   (279 MiB/s, the wire)
    rsync, unchanged, hot                  4.66 s
    bettrsync, unchanged, hot              1.52 s
    bettrsync -c, unchanged, hot           3.50 s   (7.87 GiB hashed a side)
    rsync, 10000 changed, cold             9.37 s
    rsync, 10000 changed, hot              3.92 s
    bettrsync, 10000 changed, cold         2.76 s
    bettrsync, 10000 changed, hot          2.04 s

The unchanged pass is 0.74 s of scan (a million lstats on this side,
overlapped with the receiver's) and 0.68 s after, 1.4 µs an entry a
side all in. Peak memory for 1.02M entries: the sender 310 MiB (the
entry list, about 300 B each, until each is decided), the receiver
89 MiB, 122 with `-c`. The 9.8 GB tree is kept at
`/home/max/.cache/bettrsync-1m/bench/tree` and in `~/bench/tree` on
Wonderslice (it replaced the 110k tree there; the 110k one is still
at `/home/max/.cache/bettrsync/bench/tree` here and goes back over on
the next `bench/tree.sh` run with the default scratch).

## Not yet done

- Larger-than-RAM trees on the sender: the list of entries is kept in
  memory until each is decided (a few hundred bytes each).
- Larger-than-RAM images on the receiving side, for a truly cold
  receiver.
- ssh is the only transport. At 2.5 GbE one stream carries 275 MB/s
  of a 294 MB/s wire, cipher choice makes no difference, so it is not
  the limit there; at 10 GbE it will be. Measure before building.
