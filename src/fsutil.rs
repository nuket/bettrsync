//! The few file system calls std does not offer: setting a modification
//! time without following a symlink, and on an open file.

use std::ffi::CString;
use std::fs::{self, File};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

fn times(s: i64, ns: u32) -> [libc::timespec; 2] {
    [
        libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT },
        libc::timespec { tv_sec: s as libc::time_t, tv_nsec: ns as libc::c_long },
    ]
}

pub fn set_mtime(path: &Path, s: i64, ns: u32) -> io::Result<()> {
    let c = CString::new(path.as_os_str().as_bytes())?;
    let t = times(s, ns);
    if unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), t.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn set_mtime_fd(file: &File, s: i64, ns: u32) -> io::Result<()> {
    let t = times(s, ns);
    if unsafe { libc::futimens(file.as_raw_fd(), t.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Makes sure this process may have `want` files open, raising the
/// soft limit as far as the hard one allows, and says how many it may.
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

/// Removes whatever is at `path`: a directory with its contents, or
/// anything else as one entry.
pub fn remove_any(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
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
