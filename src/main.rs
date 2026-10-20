//! bettrsync - rsync -rltv, with the files updated in place and the
//! comparing done by every core on both sides at once.
//!
//! bettrsync [OPTIONS] SOURCE... DESTINATION
//!
//! A source or the destination may be remote, [user@]host:path, reached
//! over ssh, which runs bettrsync there (--server).  Nothing is written
//! without --commit: the default run does all the reading, hashing and
//! comparing, reports what it would send, and how long each part took.

mod fsutil;
mod pool;
mod proto;
mod receiver;
mod scan;
mod sender;

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{self, Command, Stdio};
use std::thread;

use scan::Source;

const USAGE: &str = "\
usage: bettrsync [OPTIONS] SOURCE... DESTINATION

Copies directories, files and symlinks from the sources into the
destination, as rsync -rlt does, keeping modification times; files the
destination has already are updated in place, only the blocks that
differ being sent.  A source with a trailing slash contributes its
contents, without it the directory itself.  Either end may be remote,
[user@]host:path, over ssh, with bettrsync installed there too.

  -n, --dry-run        read, hash and compare, report, write nothing (default)
      --commit         write to the destination
  -c, --checksum       compare the blocks of every file, not only those whose
                       size or modification time differs
  -q, --quiet          no listing, only the summary
  -v, --verbose        the listing (default)
  -B, --block-size N   the block size, K, M or G suffix (default 128K)
  -j, --threads N      threads for hashing and scanning (default: the cores)
  -e, --rsh COMMAND    how to reach a remote host (default: ssh)
      --remote-bin P   bettrsync on the remote host (default: bettrsync)
      --version
  -h, --help
";

struct Opts {
    commit: bool,
    checksum: bool,
    quiet: bool,
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
        block: 128 << 10,
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
            "-n" | "--dry-run" => opts.commit = false,
            "--commit" => opts.commit = true,
            "-c" | "--checksum" => opts.checksum = true,
            "-q" | "--quiet" => opts.quiet = true,
            "-v" | "--verbose" => opts.quiet = false,
            "-B" | "--block-size" => {
                let v = value("a size");
                opts.block = parse_size(&v).unwrap_or_else(|| usage_error(format!("{v} is not a block size")));
            }
            "-j" | "--threads" => {
                let v = value("a count");
                opts.threads = v.parse().ok().filter(|n| *n > 0).unwrap_or_else(|| usage_error(format!("{v} is not a thread count")));
            }
            "-e" | "--rsh" => opts.rsh = value("a command"),
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
                // A cluster of short flags, -nq.
                for c in text[1..].chars() {
                    match c {
                        'n' => opts.commit = false,
                        'c' => opts.checksum = true,
                        'q' => opts.quiet = true,
                        'v' => opts.quiet = false,
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

/// A source as given: the trailing slash, or a name like . or .., asks
/// for the directory's contents.
fn source_of(path: &OsStr) -> Source {
    let bytes = path.as_bytes();
    let contents = bytes.ends_with(b"/") || PathBuf::from(path).file_name().is_none();
    Source { path: PathBuf::from(path), contents }
}

/// `[user@]host:path`, when the path has a colon before any slash.
fn remote_of(path: &OsStr) -> Option<(String, Vec<u8>)> {
    let bytes = path.as_bytes();
    let colon = bytes.iter().position(|b| *b == b':')?;
    if colon == 0 || bytes[..colon].contains(&b'/') {
        return None;
    }
    Some((String::from_utf8_lossy(&bytes[..colon]).into_owned(), bytes[colon + 1..].to_vec()))
}

fn shell_quote(bytes: &[u8]) -> String {
    format!("'{}'", String::from_utf8_lossy(bytes).replace('\'', "'\\''"))
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
    remote.push(format!("--block-size={}", opts.block));
    remote.push(format!("--threads={}", opts.threads));
    remote.push("--".to_string());
    remote.extend(paths.iter().map(|p| shell_quote(p)));
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).arg(host).arg(remote.join(" "));
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
    cmd
}

fn fail(e: io::Error) -> ! {
    eprintln!("bettrsync: {e}");
    process::exit(2);
}

fn main() {
    let opts = parse_args();

    // The far end of an ssh connection: the protocol on stdin and stdout.
    if let Some(send) = opts.server {
        let stdin: Box<dyn Read + Send> = Box::new(unsafe { std::fs::File::from_raw_fd(0) });
        let stdout: Box<dyn Write + Send> = Box::new(unsafe { std::fs::File::from_raw_fd(1) });
        if opts.paths.is_empty() {
            usage_error("the server needs a path".to_string());
        }
        let clean = if send {
            let sources = opts.paths.iter().map(|p| source_of(p)).collect();
            sender::run(stdin, stdout, sender::SendOpts { sources, commit: opts.commit, block: opts.block, threads: opts.threads, quiet: opts.quiet, local: false })
        } else {
            let dest = PathBuf::from(&opts.paths[0]);
            receiver::run(stdin, stdout, receiver::RecvOpts { dest, commit: opts.commit, checksum: opts.checksum, block: opts.block, threads: opts.threads, local: false })
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
        let r: Box<dyn Read + Send> = Box::new(child.stdout.take().unwrap());
        let w: Box<dyn Write + Send> = Box::new(child.stdin.take().unwrap());
        let local_sources = sources.iter().map(|s| source_of(s)).collect();
        let result = sender::run(r, w, sender::SendOpts { sources: local_sources, commit: opts.commit, block: opts.block, threads: opts.threads, quiet: opts.quiet, local: true });
        let status = child.wait().unwrap_or_else(|e| fail(e));
        result.map(|clean| clean && status.success())
    } else if any_remote_source {
        // Receiving from a remote sender.
        let paths: Vec<Vec<u8>> = remote_sources.iter().flatten().map(|(_, p)| p.clone()).collect();
        let mut child = remote_command(&opts, hosts[0], true, &paths).spawn().unwrap_or_else(|e| fail(e));
        let r: Box<dyn Read + Send> = Box::new(child.stdout.take().unwrap());
        let w: Box<dyn Write + Send> = Box::new(child.stdin.take().unwrap());
        let result = receiver::run(r, w, receiver::RecvOpts { dest: PathBuf::from(dest), commit: opts.commit, checksum: opts.checksum, block: opts.block, threads: opts.threads, local: true });
        let status = child.wait().unwrap_or_else(|e| fail(e));
        result.map(|stats| stats.warnings == 0 && status.success())
    } else {
        // Both here: the receiver in a thread, over a socket pair.
        let (a, b) = UnixStream::pair().unwrap_or_else(|e| fail(e));
        let (a2, b2) = (a.try_clone().unwrap_or_else(|e| fail(e)), b.try_clone().unwrap_or_else(|e| fail(e)));
        let dest = PathBuf::from(dest);
        let ropts = receiver::RecvOpts { dest, commit: opts.commit, checksum: opts.checksum, block: opts.block, threads: opts.threads, local: false };
        let receiver = thread::spawn(move || receiver::run(Box::new(b), Box::new(b2), ropts));
        let local_sources = sources.iter().map(|s| source_of(s)).collect();
        let result = sender::run(Box::new(a), Box::new(a2), sender::SendOpts { sources: local_sources, commit: opts.commit, block: opts.block, threads: opts.threads, quiet: opts.quiet, local: true });
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

#[allow(dead_code)]
fn os_string(bytes: Vec<u8>) -> OsString {
    OsString::from_vec(bytes)
}
