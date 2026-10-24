// bettrsync
// Copyright (c) 2026 Max Vilimpoc
// SPDX-License-Identifier: MIT

//! What the file system is asked for that std does not offer the same
//! way everywhere: modification times to the nanosecond, set without
//! following a symlink and on an open file; permission bits, which
//! Windows has only as the read-only attribute; names as the wire
//! carries them; reads and writes at an offset; the standard streams
//! as plain bytes; and the two ends of a connection between a sender
//! and a receiver in one process. Everything else is std, which does
//! the same on both.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

/// A name as the wire carries it: the bytes the file system holds on
/// Unix; on Windows UTF-8 (WTF-8, for a name with an unpaired
/// surrogate). Parts of a path are joined with / on the wire.
#[cfg(unix)]
pub fn name_bytes(name: &OsStr) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    name.as_bytes()
}
#[cfg(windows)]
pub fn name_bytes(name: &OsStr) -> &[u8] {
    name.as_encoded_bytes()
}

/// A name from the wire. On Windows the parts are joined with \ as
/// paths are written there, and a name that is not UTF-8 (one a Linux
/// file system held as bytes) gets replacement characters.
#[cfg(unix)]
pub fn name_of(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes.to_vec())
}
#[cfg(windows)]
pub fn name_of(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).replace('/', "\\"))
}

/// A name as --delete compares it with what the sender listed: as it
/// is, or folded on Windows, where Foo and foo are one file.
#[cfg(unix)]
pub fn fold_name(bytes: &[u8]) -> Vec<u8> {
    bytes.to_vec()
}
#[cfg(windows)]
pub fn fold_name(bytes: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(bytes).to_lowercase().into_bytes()
}

/// A symlink's target as the wire carries it: with / between the
/// parts, whatever the link holds.
pub fn target_bytes(target: &Path) -> Vec<u8> {
    let bytes = name_bytes(target.as_os_str());
    if cfg!(windows) {
        bytes.iter().map(|b| if *b == b'\\' { b'/' } else { *b }).collect()
    } else {
        bytes.to_vec()
    }
}

/// Whether the link at `path` points where the wire says.
pub fn link_points_to(path: &Path, target: &[u8]) -> bool {
    fs::read_link(path).map(|t| target_bytes(&t) == target).unwrap_or(false)
}

/// Makes `path` a symlink to the wire's `target`.
#[cfg(unix)]
pub fn symlink(target: &[u8], path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(name_of(target), path)
}
#[cfg(windows)]
pub fn symlink(target: &[u8], path: &Path) -> io::Result<()> {
    // Windows wants backslashes in a link, and to be told whether it
    // leads to a directory; that needs a privilege an ordinary session
    // does not have, in which case this fails and says so.
    let target: Vec<u8> = target.iter().map(|b| if *b == b'/' { b'\\' } else { *b }).collect();
    let target = std::path::PathBuf::from(name_of(&target));
    let there = if target.is_absolute() { target.clone() } else { path.parent().unwrap_or(Path::new(".")).join(&target) };
    if there.is_dir() {
        std::os::windows::fs::symlink_dir(&target, path)
    } else {
        std::os::windows::fs::symlink_file(&target, path)
    }
}

/// The modification time: seconds and nanoseconds since the epoch.
#[cfg(unix)]
pub fn mtime(m: &Metadata) -> (i64, u32) {
    use std::os::unix::fs::MetadataExt;
    (m.mtime(), m.mtime_nsec() as u32)
}
#[cfg(windows)]
pub fn mtime(m: &Metadata) -> (i64, u32) {
    use std::os::windows::fs::MetadataExt;
    // NTFS keeps 100 ns ticks since 1601.
    let ticks = m.last_write_time() as i64 - EPOCH_TICKS;
    (ticks.div_euclid(10_000_000), (ticks.rem_euclid(10_000_000) * 100) as u32)
}
#[cfg(windows)]
const EPOCH_TICKS: i64 = 116_444_736_000_000_000;

/// Whether the time a file has is the time asked for, as far as its
/// file system keeps it: Windows keeps 100 ns, and a Unix file system
/// that keeps whole seconds reports 0 ns.
pub fn same_mtime(m: &Metadata, s: i64, ns: u32) -> bool {
    let (ms, mns) = mtime(m);
    if cfg!(windows) {
        ms == s && mns / 100 == ns / 100
    } else {
        ms == s && (mns == ns || mns == 0)
    }
}

#[cfg(unix)]
fn times(s: i64, ns: u32) -> [libc::timespec; 2] {
    [
        libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT },
        libc::timespec { tv_sec: s as libc::time_t, tv_nsec: ns as libc::c_long },
    ]
}

/// Sets the modification time of what is at `path`, a symlink's own
/// if it is one.
#[cfg(unix)]
pub fn set_mtime(path: &Path, s: i64, ns: u32) -> io::Result<()> {
    let c = std::ffi::CString::new(name_bytes(path.as_os_str()))?;
    let t = times(s, ns);
    if unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), t.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(windows)]
pub fn set_mtime(path: &Path, s: i64, ns: u32) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    // The thing itself, link, directory or file, for its attributes only.
    let f = OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).open(path)?;
    set_mtime_fd(&f, s, ns)
}
#[cfg(windows)]
const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

#[cfg(unix)]
pub fn set_mtime_fd(file: &File, s: i64, ns: u32) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let t = times(s, ns);
    if unsafe { libc::futimens(file.as_raw_fd(), t.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(windows)]
pub fn set_mtime_fd(file: &File, s: i64, ns: u32) -> io::Result<()> {
    use std::time::{Duration, UNIX_EPOCH};
    let t = if s >= 0 { UNIX_EPOCH + Duration::new(s as u64, ns) } else { UNIX_EPOCH - Duration::from_secs(s.unsigned_abs()) + Duration::from_nanos(ns as u64) };
    file.set_modified(t)
}

/// The permission bits, the 0o7777 of the mode. Windows has only the
/// read-only attribute, which reads as 0o444 against 0o644, and a
/// directory as 0o755.
#[cfg(unix)]
pub fn mode_of(m: &Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    m.mode() & 0o7777
}
#[cfg(windows)]
pub fn mode_of(m: &Metadata) -> u32 {
    if m.is_dir() {
        0o755
    } else if m.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

/// The bits the destination can hold of the source's `mode`: all of
/// them on Unix; on Windows whether the owner may write, and nothing
/// of a directory's.
#[cfg(unix)]
pub fn wanted_mode(mode: u32, _is_dir: bool) -> u32 {
    mode
}
#[cfg(windows)]
pub fn wanted_mode(mode: u32, is_dir: bool) -> u32 {
    if is_dir {
        0o755
    } else if mode & 0o200 == 0 {
        0o444
    } else {
        0o644
    }
}

/// The permission bits of an open file.
#[cfg(unix)]
pub fn set_mode_fd(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))
}
#[cfg(windows)]
pub fn set_mode_fd(file: &File, mode: u32) -> io::Result<()> {
    let mut p = file.metadata()?.permissions();
    p.set_readonly(mode & 0o200 == 0);
    file.set_permissions(p)
}

/// The permission bits of a directory (or anything else that is not a
/// symlink).
#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}
#[cfg(windows)]
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    let mut p = fs::metadata(path)?.permissions();
    p.set_readonly(mode & 0o200 == 0);
    fs::set_permissions(path, p)
}

/// A file its owner may not write cannot be updated in place: gives it
/// owner write for the update, and says so, so the caller sets the
/// source's bits at the end. On Windows that is the read-only
/// attribute, which also stops a delete until it is cleared.
#[cfg(unix)]
pub fn clear_readonly(path: &Path, m: &Metadata) -> io::Result<bool> {
    let mode = mode_of(m);
    if mode & 0o200 != 0 {
        return Ok(false);
    }
    set_mode(path, mode | 0o200)?;
    Ok(true)
}
#[cfg(windows)]
pub fn clear_readonly(path: &Path, m: &Metadata) -> io::Result<bool> {
    if !m.permissions().readonly() {
        return Ok(false);
    }
    set_mode(path, 0o644)?;
    Ok(true)
}

/// Creates the file, empty, with the source's bits plus owner rw where
/// bits are a thing; read-only on Windows is set at the finish.
pub fn create(path: &Path, mode: u32) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(mode | 0o600);
    }
    #[cfg(windows)]
    let _ = mode;
    o.open(path)
}

/// Removes whatever is at `path`: a directory with its contents, or
/// anything else as one entry.
pub fn remove_any(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if meta.file_type().is_symlink_dir() {
            return fs::remove_dir(path);
        }
        if !meta.is_dir() {
            clear_readonly(path, &meta)?;
        }
    }
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Reads at `offset`, without moving any file position another thread
/// relies on.
#[cfg(unix)]
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buf, offset)
}
#[cfg(windows)]
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buf, offset)
}

/// Writes all of `data` at `offset`.
#[cfg(unix)]
pub fn write_all_at(file: &File, data: &[u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(data, offset)
}
#[cfg(windows)]
pub fn write_all_at(file: &File, mut data: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        match file.seek_write(data, offset) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "failed to write whole buffer")),
            Ok(n) => {
                data = &data[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Makes sure this process may have `want` files open, raising the
/// soft limit as far as the hard one allows, and says how many it may.
#[cfg(unix)]
pub fn allow_open_files(want: u64) -> u64 {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return want;
    }
    if (lim.rlim_cur as u64) < want {
        lim.rlim_cur = (want as libc::rlim_t).min(lim.rlim_max);
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) };
    }
    lim.rlim_cur as u64
}
#[cfg(windows)]
pub fn allow_open_files(want: u64) -> u64 {
    // Handles are not limited that way.
    want
}

pub type Reader = Box<dyn Read + Send>;
pub type Writer = Box<dyn Write + Send>;

/// Standard input and output as plain byte streams, for the far end of
/// an ssh connection.
#[cfg(unix)]
pub fn stdio() -> (Reader, Writer) {
    use std::os::unix::io::FromRawFd;
    (Box::new(unsafe { File::from_raw_fd(0) }), Box::new(unsafe { File::from_raw_fd(1) }))
}
#[cfg(windows)]
pub fn stdio() -> (Reader, Writer) {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    // The process's own handles, which std's Stdin and Stdout never
    // close; these Files do, at the end, which is the end of the run.
    let i = unsafe { File::from_raw_handle(io::stdin().as_raw_handle()) };
    let o = unsafe { File::from_raw_handle(io::stdout().as_raw_handle()) };
    (Box::new(i), Box::new(o))
}

/// Two connected ends, each to read from and write to, for the sender
/// and the receiver in one process: a socket pair, or on Windows a
/// pipe each way.
#[cfg(unix)]
pub fn local_pair() -> io::Result<((Reader, Writer), (Reader, Writer))> {
    use std::os::unix::net::UnixStream;
    let (a, b) = UnixStream::pair()?;
    let (a2, b2) = (a.try_clone()?, b.try_clone()?);
    Ok(((Box::new(a), Box::new(a2)), (Box::new(b), Box::new(b2))))
}
#[cfg(windows)]
pub fn local_pair() -> io::Result<((Reader, Writer), (Reader, Writer))> {
    let (a_reads, b_writes) = io::pipe()?;
    let (b_reads, a_writes) = io::pipe()?;
    Ok(((Box::new(a_reads), Box::new(a_writes)), (Box::new(b_reads), Box::new(b_writes))))
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

pub fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}
