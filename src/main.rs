// bettrsync
// Copyright (c) 2026 Max Vilimpoc
// SPDX-License-Identifier: MIT

//! bettrsync - rsync -rltv, with the files updated in place and the
//! comparing done by every core on both sides at once.
//!
//! bettrsync [OPTIONS] SOURCE... DESTINATION
//!
//! A source or the destination may be remote, [user@]host:path, reached
//! over ssh, which runs bettrsync there (--server). Nothing is written
//! without --commit: the default run does all the reading, hashing and
//! comparing, reports what it would send, and how long each part took.

mod filter;
mod fsutil;
mod pool;
mod proto;
mod receiver;
mod scan;
mod sender;
mod trace;

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;
use std::process::{self, Command, Stdio};
use std::thread;

use scan::Source;

/// The most files either side keeps in flight at a time.
const MAX_OPEN: usize = 1024;

const USAGE: &str = "\
usage: bettrsync [OPTIONS] SOURCE... DESTINATION

Copies directories, files and symlinks from the sources into the
destination, as rsync -rltp does, keeping modification times and
permission bits; files the destination has already are updated in
place, only the blocks that differ being sent. A source with a
trailing slash contributes its contents, without it the directory
itself. Either end may be remote, [user@]host:path, over ssh, with
bettrsync installed there too.

      --commit         write to the destination (without it: read, hash,
                       compare and report, writing nothing)
  -c, --checksum       compare the blocks of every file, not only those whose
                       size or modification time differs
  -q, --quiet          no listing, only the summary
  -B, --block-size N   the block size, K, M or G suffix (default 64K)
  -j, --threads N      threads for hashing and scanning (default: the cores)
  -e, --rsh COMMAND    how to reach a remote host (default: ssh)
      --remote-bin P   bettrsync on the remote host (default: bettrsync)
      --exclude PAT    leave out what matches PAT, as rsync reads it: a name
                       anywhere, or a path under the destination if it has a
                       slash (anchored there with a leading slash); a trailing
                       slash means directories only; * ** ? [..] \\ as usual;
                       may be given more than once
      --exclude-from F read patterns from F, one a line, blank lines and
                       lines starting with # or ; skipped; - for stdin
      --delete         remove from the destination what the source does not
                       have (excluded things are left alone; with any warning
                       on the sending side nothing is deleted)
      --version
  -h, --help
";

struct Opts {
    commit: bool,
    checksum: bool,
    quiet: bool,
    excludes: Vec<String>,
    delete: bool,
    block: usize,
    threads: usize,
    rsh: String,
    remote_bin: String,
    server: Option<bool>, // Some(true): send, Some(false): receive
    paths: Vec<OsString>,
}

fn usage_error(what: String) -> ! {
    eprintln!("bettrsync: {what}");
    eprintln!("try bettrsync --help");
    process::exit(2);
}

/// The patterns in an exclude file, one a line: blank lines and lines
/// starting with # or ; are skipped, as rsync does; - reads stdin.
fn read_patterns(file: &str) -> io::Result<Vec<String>> {
    let text = if file == "-" { io::read_to_string(io::stdin())? } else { std::fs::read_to_string(file)? };
    Ok(text.lines().map(|l| l.trim_end_matches('\r')).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with(';')).map(str::to_string).collect())
}

fn parse_size(s: &str) -> Option<usize> {
    let s = s.trim();
    let (digits, mult) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1usize << 10),
        'm' | 'M' => (&s[..s.len() - 1], 1 << 20),
        'g' | 'G' => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    digits.parse::<usize>().ok().map(|n| n * mult).filter(|n| *n > 0)
}

fn parse_args() -> Opts {
    let mut opts = Opts {
        commit: false,
        checksum: false,
        quiet: false,
        excludes: Vec::new(),
        delete: false,
        block: 64 << 10,
        threads: thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
        rsh: "ssh".to_string(),
        remote_bin: "bettrsync".to_string(),
        server: None,
        paths: Vec::new(),
    };
    let mut args = std::env::args_os().skip(1);
    let mut only_paths = false;
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy().into_owned();
        if only_paths || !text.starts_with('-') || text == "-" {
            opts.paths.push(arg);
            continue;
        }
        // --name=value as --name value.
        let (name, inline) = match text.split_once('=') {
            Some((n, v)) if text.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (text.clone(), None),
        };
        let mut value = |what: &str| -> String {
            inline.clone().or_else(|| args.next().map(|a| a.to_string_lossy().into_owned())).unwrap_or_else(|| usage_error(format!("{name} needs {what}")))
        };
        match name.as_str() {
            "--" => only_paths = true,
            "--commit" => opts.commit = true,
            "-c" | "--checksum" => opts.checksum = true,
            "-q" | "--quiet" => opts.quiet = true,
            "-B" | "--block-size" => {
                let v = value("a size");
                opts.block = parse_size(&v).unwrap_or_else(|| usage_error(format!("{v} is not a block size")));
            }
            "-j" | "--threads" => {
                let v = value("a count");
                opts.threads = v.parse().ok().filter(|n| *n > 0).unwrap_or_else(|| usage_error(format!("{v} is not a thread count")));
            }
            "-e" | "--rsh" => opts.rsh = value("a command"),
            "--exclude" => opts.excludes.push(value("a pattern")),
            "--exclude-from" => {
                let f = value("a file");
                opts.excludes.extend(read_patterns(&f).unwrap_or_else(|e| usage_error(format!("{f}: {e}"))));
            }
            "--delete" => opts.delete = true,
            "--remote-bin" => opts.remote_bin = value("a path"),
            "--server" => {}
            "--send" => opts.server = Some(true),
            "--receive" => opts.server = Some(false),
            "--version" => {
                println!("bettrsync {} ({})", env!("CARGO_PKG_VERSION"), option_env!("BETTRSYNC_GIT_REVISION").unwrap_or("unknown"));
                process::exit(0);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                process::exit(0);
            }
            _ if text.starts_with("--") => usage_error(format!("unknown option {text}")),
            _ => {
                // A cluster of short flags, -cq.
                for c in text[1..].chars() {
                    match c {
                        'c' => opts.checksum = true,
                        'q' => opts.quiet = true,
                        'h' => {
                            print!("{USAGE}");
                            process::exit(0);
                        }
                        _ => usage_error(format!("unknown option -{c}")),
                    }
                }
            }
        }
    }
    opts
}

/// A source as given: a trailing separator (/ anywhere, \ too on
/// Windows), or a name like . or .. or C: that has no base name, asks
/// for the directory's contents.
fn source_of(path: &OsStr) -> Source {
    let bytes = fsutil::name_bytes(path);
    let contents = bytes.ends_with(b"/") || (cfg!(windows) && bytes.ends_with(b"\\")) || PathBuf::from(path).file_name().is_none();
    Source { path: PathBuf::from(path), contents }
}

/// `[user@]host:path`, when the path has a colon before any separator.
/// On Windows a drive letter, C:\dir or C:/dir, is not a host.
fn remote_of(path: &OsStr) -> Option<(String, Vec<u8>)> {
    let bytes = fsutil::name_bytes(path);
    let colon = bytes.iter().position(|b| *b == b':')?;
    let host = &bytes[..colon];
    if colon == 0 || host.contains(&b'/') {
        return None;
    }
    if cfg!(windows) && (host.contains(&b'\\') || (colon == 1 && bytes[0].is_ascii_alphabetic())) {
        return None;
    }
    Some((String::from_utf8_lossy(host).into_owned(), bytes[colon + 1..].to_vec()))
}

/// An argument on the remote shell's command line, whichever shell that
/// is: sh on Unix, cmd on Windows. Both take double quotes. A " inside
/// is written \", and a run of backslashes before one, or at the end, is
/// doubled, since that is how a program on Windows reads its command
/// line and how sh reads a double-quoted string too. $ and ` are
/// escaped for sh; a path with those on a Windows host is beyond this.
fn remote_quote(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in String::from_utf8_lossy(bytes).chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat('\\').take(2 * backslashes + 1));
                out.push(c);
                backslashes = 0;
            }
            '$' | '`' => {
                out.extend(std::iter::repeat('\\').take(backslashes + 1));
                out.push(c);
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat('\\').take(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat('\\').take(2 * backslashes));
    out.push('"');
    out
}

fn remote_command(opts: &Opts, host: &str, send: bool, paths: &[Vec<u8>]) -> Command {
    let argv: Vec<String> = opts.rsh.split_whitespace().map(String::from).collect();
    if argv.is_empty() {
        usage_error("the remote shell command is empty".to_string());
    }
    let mut remote = vec![opts.remote_bin.clone(), "--server".to_string(), if send { "--send" } else { "--receive" }.to_string()];
    if opts.commit {
        remote.push("--commit".to_string());
    }
    if opts.checksum {
        remote.push("--checksum".to_string());
    }
    if opts.quiet {
        remote.push("--quiet".to_string());
    }
    if opts.delete {
        remote.push("--delete".to_string());
    }
    for pat in &opts.excludes {
        remote.push(remote_quote(format!("--exclude={pat}").as_bytes()));
    }
    remote.push(format!("--block-size={}", opts.block));
    remote.push(format!("--threads={}", opts.threads));
    remote.push("--".to_string());
    remote.extend(paths.iter().map(|p| remote_quote(p)));
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).arg(host).arg(remote.join(" "));
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
    cmd
}

/// What to say when the remote end's process ended badly: a bettrsync
/// that was not there, an rsh that failed, or a remote that died. The
/// remote's own stderr is already on the screen above.
fn remote_failed(e: io::Error, status: process::ExitStatus, host: &str, remote_bin: &str) -> io::Error {
    if status.success() {
        return e;
    }
    // 127 is sh's word for a command not found, 9009 cmd's; cmd given a
    // path that leads nowhere says so and exits 1, which only the
    // missing handshake tells from a bettrsync that failed at once.
    let before_hello = e.kind() == io::ErrorKind::UnexpectedEof;
    let what = match status.code() {
        Some(127) | Some(9009) => format!("{remote_bin} was not found on {host}: install it there, or give its path with --remote-bin"),
        Some(126) => format!("{remote_bin} on {host} could not be run"),
        Some(255) => format!("could not reach {host}: the connection failed"),
        Some(n) if before_hello => format!("{remote_bin} on {host} ended with status {n} before the handshake: it was not found or could not start; install it there, or give its path with --remote-bin"),
        Some(n) => format!("{remote_bin} on {host} ended with status {n} before the transfer finished: {e}"),
        None => format!("{remote_bin} on {host} was killed before the transfer finished: {e}"),
    };
    io::Error::new(e.kind(), what)
}

fn fail(e: io::Error) -> ! {
    eprintln!("bettrsync: {e}");
    process::exit(2);
}

fn main() {
    let opts = parse_args();
    // Files in flight on each side: as many as the descriptor limit
    // leaves room for, with both sides in this process at worst.
    let max_open = ((fsutil::allow_open_files(2 * MAX_OPEN as u64 + 256).saturating_sub(256)) / 2).clamp(16, MAX_OPEN as u64) as usize;
    let excludes = std::sync::Arc::new(filter::Excludes::new(&opts.excludes));

    // The far end of an ssh connection: the protocol on stdin and stdout.
    if let Some(send) = opts.server {
        let (stdin, stdout) = fsutil::stdio();
        if opts.paths.is_empty() {
            usage_error("the server needs a path".to_string());
        }
        let clean = if send {
            let sources = opts.paths.iter().map(|p| source_of(p)).collect();
            sender::run(stdin, stdout, sender::SendOpts { sources, commit: opts.commit, block: opts.block, threads: opts.threads, max_open, quiet: opts.quiet, excludes: excludes.clone(), delete: opts.delete, local: false })
        } else {
            let dest = PathBuf::from(&opts.paths[0]);
            receiver::run(stdin, stdout, receiver::RecvOpts { dest, commit: opts.commit, checksum: opts.checksum, block: opts.block, threads: opts.threads, max_open, quiet: opts.quiet, excludes: excludes.clone(), delete: opts.delete, local: false })
                .map(|stats| stats.warnings == 0)
        };
        process::exit(match clean {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(e) => fail(e),
        });
    }

    if opts.paths.len() < 2 {
        usage_error("a source and a destination are needed".to_string());
    }
    let (sources, dest) = opts.paths.split_at(opts.paths.len() - 1);
    let dest = &dest[0];
    let remote_sources: Vec<_> = sources.iter().map(|s| remote_of(s)).collect();
    let remote_dest = remote_of(dest);
    let any_remote_source = remote_sources.iter().any(|r| r.is_some());
    if any_remote_source && remote_dest.is_some() {
        usage_error("the source and the destination cannot both be remote".to_string());
    }
    if any_remote_source && !remote_sources.iter().all(|r| r.is_some()) {
        usage_error("the sources must all be here or all on the same host".to_string());
    }
    let hosts: Vec<&str> = remote_sources.iter().flatten().map(|(h, _)| h.as_str()).collect();
    if hosts.windows(2).any(|w| w[0] != w[1]) {
        usage_error("the sources must all be on the same host".to_string());
    }

    let clean = if let Some((host, path)) = remote_dest {
        // Sending to a remote receiver.
        let mut child = remote_command(&opts, &host, false, &[path]).spawn().unwrap_or_else(|e| fail(e));
        let r: fsutil::Reader = Box::new(child.stdout.take().unwrap());
        let w: fsutil::Writer = Box::new(child.stdin.take().unwrap());
        let local_sources = sources.iter().map(|s| source_of(s)).collect();
        let result = sender::run(r, w, sender::SendOpts { sources: local_sources, commit: opts.commit, block: opts.block, threads: opts.threads, max_open, quiet: opts.quiet, excludes: excludes.clone(), delete: opts.delete, local: true });
        let status = child.wait().unwrap_or_else(|e| fail(e));
        result.map_err(|e| remote_failed(e, status, &host, &opts.remote_bin)).map(|clean| clean && status.success())
    } else if any_remote_source {
        // Receiving from a remote sender.
        let paths: Vec<Vec<u8>> = remote_sources.iter().flatten().map(|(_, p)| p.clone()).collect();
        let mut child = remote_command(&opts, hosts[0], true, &paths).spawn().unwrap_or_else(|e| fail(e));
        let r: fsutil::Reader = Box::new(child.stdout.take().unwrap());
        let w: fsutil::Writer = Box::new(child.stdin.take().unwrap());
        let result = receiver::run(r, w, receiver::RecvOpts { dest: PathBuf::from(dest), commit: opts.commit, checksum: opts.checksum, block: opts.block, threads: opts.threads, max_open, quiet: opts.quiet, excludes: excludes.clone(), delete: opts.delete, local: true });
        let status = child.wait().unwrap_or_else(|e| fail(e));
        result.map_err(|e| remote_failed(e, status, hosts[0], &opts.remote_bin)).map(|stats| stats.warnings == 0 && status.success())
    } else {
        // Both here: the receiver in a thread, over a socket pair.
        let ((r, w), (r2, w2)) = fsutil::local_pair().unwrap_or_else(|e| fail(e));
        let dest = PathBuf::from(dest);
        let ropts = receiver::RecvOpts { dest, commit: opts.commit, checksum: opts.checksum, block: opts.block, threads: opts.threads, max_open, quiet: opts.quiet, excludes: excludes.clone(), delete: opts.delete, local: false };
        let receiver = thread::spawn(move || receiver::run(r2, w2, ropts));
        let local_sources = sources.iter().map(|s| source_of(s)).collect();
        let result = sender::run(r, w, sender::SendOpts { sources: local_sources, commit: opts.commit, block: opts.block, threads: opts.threads, max_open, quiet: opts.quiet, excludes: excludes.clone(), delete: opts.delete, local: true });
        let received = receiver.join().unwrap_or_else(|_| Err(io::Error::other("the receiver thread died")));
        match (result, received) {
            (Ok(clean), Ok(stats)) => Ok(clean && stats.warnings == 0),
            (Err(e), _) | (_, Err(e)) => Err(e),
        }
    };
    process::exit(match clean {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => fail(e),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_for_both_shells() {
        assert_eq!(remote_quote(b"/home/max/dst"), "\"/home/max/dst\"");
        assert_eq!(remote_quote(b"C:\\Users\\Max\\dst"), "\"C:\\Users\\Max\\dst\"");
        // A backslash at the end, or before a quote, is doubled.
        assert_eq!(remote_quote(b"C:\\dst\\"), "\"C:\\dst\\\\\"");
        assert_eq!(remote_quote(b"a\"b"), "\"a\\\"b\"");
        assert_eq!(remote_quote(b"a\\\"b"), "\"a\\\\\\\"b\"");
        assert_eq!(remote_quote(b"$HOME/x `y`"), "\"\\$HOME/x \\`y\\`\"");
    }

    #[test]
    fn remote_or_not() {
        assert_eq!(remote_of(OsStr::new("host:dir")).map(|(h, _)| h), Some("host".to_string()));
        assert_eq!(remote_of(OsStr::new("user@host:/x")).map(|(_, p)| p), Some(b"/x".to_vec()));
        assert_eq!(remote_of(OsStr::new("host:C:/Users")).map(|(_, p)| p), Some(b"C:/Users".to_vec()));
        assert!(remote_of(OsStr::new("dir/a:b")).is_none());
        assert!(remote_of(OsStr::new(":x")).is_none());
        if cfg!(windows) {
            assert!(remote_of(OsStr::new("C:\\Users")).is_none());
            assert!(remote_of(OsStr::new("c:/users")).is_none());
            assert!(remote_of(OsStr::new("C:")).is_none());
            assert!(remote_of(OsStr::new(".\\a:b")).is_none());
        }
    }

    #[test]
    fn sources() {
        assert!(source_of(OsStr::new("dir/")).contents);
        assert!(source_of(OsStr::new(".")).contents);
        assert!(source_of(OsStr::new("..")).contents);
        assert!(!source_of(OsStr::new("dir")).contents);
        assert!(!source_of(OsStr::new("../dir")).contents);
        if cfg!(windows) {
            assert!(source_of(OsStr::new(".\\dir\\")).contents);
            assert!(source_of(OsStr::new("C:\\")).contents);
            assert!(source_of(OsStr::new("C:")).contents);
            assert!(!source_of(OsStr::new("..\\dir")).contents);
        }
    }
}
