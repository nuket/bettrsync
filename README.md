# bettrsync

A small sync tool that does what `rsync -rltpv --inplace` does, and
does it with every core on both ends.

    bettrsync [OPTIONS] SOURCE... DESTINATION

The basic premise is to mirror everything from a subfolder, minus excluded
dirs and files, as fast as possible.

In comparison, I feel that `rsync` has too many options, most of which
I never use.

## Dry Run By Default

Nothing is written without `--commit`.

The default run does all the reading, hashing and comparing, lists
what it would do, and ends with a summary of how long each part took
and what it would send.

    update   images/dev.vdi  1,165 of 65,536 blocks, 72.8 MiB

    scanned     0.11 s   1 entries: 0 dirs, 1 files (4.00 GiB), 0 links
    compared    1.15 s   hashed 4.00 GiB here, 4.00 GiB there (6.96 GiB/s)
    to send     1.15 s   72.8 MiB in 1,165 blocks of 64.0 KiB (63.3 MiB/s)
    total       1.26 s   unchanged 0, updated 1, new 0 files, 0 dirs, 0 links, failed 0
    dry run: nothing was written; add --commit to write

The same run with `--commit` writes the data, and says `sent` instead.

## Options

`bettrsync` allows these options:

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
rsync. A single file to a path that is not a directory is copied to
that name.

`--exclude` reads its pattern as rsync does. A pattern without a slash
matches a name anywhere in the tree, `*.tmp`; with a slash it matches
the path under the destination, `build/cache`, anchored at the top if
it starts with a slash, `/top`, and otherwise anywhere on a directory
boundary. A trailing slash matches directories only, `cache/`. `*`
matches within a name, `**` across slashes, `?` one character, `[a-z]`
a class, `\` escapes. An excluded directory is not entered at all.
`--exclude-from` reads the same patterns from a file, one a line, with
blank lines and lines starting with `#` or `;` skipped, and `-` for
standard input; the file is read on the side the command runs on.

`--delete` removes, from each directory the source has, whatever the
source does not: files, links, and directories with everything in
them. Excluded things are left alone on both sides, so
`--exclude cache/ --delete` neither copies caches nor removes the ones
the destination has. A dry run lists what would go, as `delete`. If the
sending side had any warning, an unreadable directory for one, nothing
is deleted, since the list it sent may be short.

## On Windows

The same binary, `bettrsync.exe`, with the same options. A path may be
spelled either way: `src/`, `.\src\`, `..\images`, `C:\VMs\` or
`C:/VMs/`; a trailing slash or backslash means the contents, as above.
A drive letter is not a host, so `C:\dst` is local, and a remote path
on a Windows host is written after the host's own colon,
`winbox:C:/Users/Max/dst` or `winbox:C:\Users\Max\dst`, or relative to
the ssh user's home, `winbox:dst`; a `$` or backtick in a path on a
Windows host does not survive the remote command line. Exclude
patterns use `/` whatever the side.

Windows keeps modification times to 100 ns, so a Linux file's
nanoseconds are rounded there and the comparison allows for it both
ways. The only permission bit it has is the read-only attribute: a
source file without owner write becomes read-only there, a read-only
source file is 0444 on Linux, and directories carry no bits. Symbolic links
need a privilege (Developer Mode, or an elevated session) that an ssh
session does not have by default; without it a link is a warning and
counts as failed, and everything else goes through. NTFS does not
tell `Foo` from `foo`, so `--delete` does not either there. Names
that Windows cannot hold (`:`, `*`, `?` and so on) fail to be created
and are reported.

`BETTRSYNC_TRACE=1` in the environment makes each thread print, at its
end, where its time went, on both sides (the remote side's lines come
back over ssh's stderr).

## How It Works

Directories, regular files and symbolic links under the sources are
copied into the destination, links as links, keeping modification
times to the nanosecond and permission bits exactly, on new and
existing entries alike; one that differs in its bits alone is listed
as `chmod` and counts as unchanged. Owners and groups are not copied,
and nothing is deleted unless `--delete` says so.

A file the destination already has with the same size and time is
left alone. One that differs is updated in place: both ends hash it in
blocks of `-B` bytes, on all their cores at once, and only the blocks
that differ cross the wire. A destination file its owner may not write
(0444, or read-only on Windows) is made writable for the update and
given the source's bits after it. That is what makes a VM disk image
with a few hundred changed spots a matter of seconds.

Either end may be remote, `[user@]host:path`, over ssh, with bettrsync
installed on the host too, on the PATH a non-interactive login shell
sees, or named with `--remote-bin`. Two local paths work without ssh.
It runs on Linux, macOS and Windows, and any of them may be at either
end.

## Releases

Each release on GitHub has, for Linux x86_64 and aarch64, a tar.gz, a
`.deb` and an `.rpm` (glibc 2.31 or newer); an Arch package for
x86_64; a tar.gz with one universal binary for macOS; a zip for
Windows; and `SHA256SUMS`.

    sudo dpkg -i bettrsync_0.2.0_amd64.deb                  # Debian, Ubuntu
    sudo rpm -i bettrsync-0.2.0-1.x86_64.rpm                # Fedora, RHEL, SUSE
    sudo pacman -U bettrsync-0.2.0-1-x86_64.pkg.tar.zst     # Arch, CachyOS, Manjaro

pacman takes the release page's URL in place of the file. The tar.gz
and zip hold the binary itself; put it on the PATH.

### Signatures and Checksums

Releases are signed with the key in `packaging/bettrsync-signing-key.asc`,
also on the release page as `bettrsync-signing-key.asc`. Every file has
a signature beside it (`.asc`, or `.sig` for the Arch package), and
`SHA256SUMS.asc` is the signed list of checksums. To check a download:

    gpg --import bettrsync-signing-key.asc
    gpg --verify SHA256SUMS.asc && sha256sum -c --ignore-missing SHA256SUMS
    gpg --verify bettrsync-0.2.0-linux-x86_64.tar.gz.asc        # or any one file

The `.rpm` carries its signature inside as well:

    sudo rpm --import bettrsync-signing-key.asc
    rpm -K bettrsync-0.2.0-1.x86_64.rpm

pacman checks the `.sig` beside a package and refuses a key it does
not know, so on Arch tell it about the key once; the fingerprint is
what `gpg --show-keys bettrsync-signing-key.asc` prints:

    sudo pacman-key --add bettrsync-signing-key.asc
    sudo pacman-key --lsign-key <fingerprint>

## Building and Testing

- Linux: `./build.sh`, in the `rust:bullseye` Docker image when Docker
  is there, so the binary runs on any glibc from 2.31 up; otherwise
  with the local cargo.
- macOS: `./build.sh`, with the local cargo.
- Windows: `build.bat`, with rustup's MSVC toolchain.

The binary is `target/release/bettrsync` (`.exe` on Windows) and needs
nothing beside it. The tests want a scratch directory with a few
hundred MB free:

    cargo test --release
    python3 tests/smoke.py target/release/bettrsync SCRATCHDIR     # every platform
    tests/smoke.sh target/release/bettrsync SCRATCHDIR             # Linux

`bench/bench.sh BIN USER@HOST SCRATCHDIR [GIB] [CHANGES]` times
`rsync --inplace` against bettrsync on a changed image over ssh, and
`bench/tree.sh BIN USER@HOST SCRATCHDIR [FILES] [CHANGES]` does the
same with a tree of many small files.

## Benchmarks

On a 4 GiB image with 200 changed spots, over a 1 Gbps link from a
6-core VM to a 16-core desktop:

                                  cold      hot
    rsync --inplace             5.85 s   5.06 s
    bettrsync dry run           2.55 s   1.58 s
    bettrsync --commit          3.84 s   1.94 s

The same image and changes between two systems at 2.5 GbE, with the
whole page cache dropped on both sides before each cold run. iperf3
measures the link at 2.35 Gbit/s and one ssh stream carries 275 MB/s
either way, so the whole-file copy is at the ssh ceiling:

                                  cold      hot
    bettrsync new file         14.90 s        -   (4 GiB at 277 MiB/s)
    rsync --inplace             6.02 s   3.28 s
    bettrsync dry run           1.07 s   0.68 s
    bettrsync --commit          1.47 s   1.12 s
    rsync, unchanged                 -   0.16 s
    bettrsync, unchanged             -   0.12 s
    bettrsync -c, unchanged          -   0.67 s

Pulling works at the same rate: the 4 GiB file came back at 277 MiB/s,
and `-c` on an unchanged hot copy compared both sides in 0.64 s.

A tree of 110,000 files of 512 B to 16 KiB in 211 directories with
1,000 links, 882 MiB, on the same two systems; a round changes 1,000
files and adds 500. The receiver runs a few VMs, so these move by a
few tenths from run to run:

                                  cold      hot
    rsync new tree              7.11 s        -
    bettrsync new tree          3.49 s        -   (252 MiB/s, the wire)
    rsync, 1500 changed         1.27 s   0.51 s
    bettrsync, 1500 changed     0.45 s   0.32 s
    rsync, unchanged                 -   0.70 s
    bettrsync, unchanged             -   0.44 s
    bettrsync -c, unchanged          -   0.97 s

Run on their own, three times each, the unchanged pass takes 0.30 to
0.32 s, of which ssh startup is 0.11 s and the scan 0.03 s, and `-c`
takes 0.52 to 0.58 s, hashing every file on both sides without
touching any of them.

A million files, 7.87 GiB in 2,101 directories with 10,000 links; a
round changes 6,666 files and adds 3,334:

                                  cold      hot
    rsync new tree             47.07 s        -
    bettrsync new tree         30.03 s        -   (279 MiB/s, the wire)
    rsync, 10000 changed        9.37 s   3.92 s
    bettrsync, 10000 changed    2.76 s   2.04 s
    rsync, unchanged                 -   4.66 s
    bettrsync, unchanged             -   1.52 s
    bettrsync -c, unchanged          -   3.50 s

Memory: the sender holds about 300 bytes an entry until it is decided,
310 MiB for the million files; the receiver peaked at 89 MiB.

## License

MIT; see LICENSE.
