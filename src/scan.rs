//! The source tree, walked by several threads at once: every directory,
//! regular file and symlink under the sources, with the path each has
//! under the destination.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::thread;

use crate::proto::{KIND_DIR, KIND_FILE, KIND_LINK};

/// A source as given on the command line.  With `contents` (a trailing
/// slash, or a name like . that has no base name) the directory's
/// contents land in the destination; else the directory itself does.
pub struct Source {
    pub path: PathBuf,
    pub contents: bool,
}

pub struct Entry {
    pub kind: u8,
    pub rel: Vec<u8>,
    pub abs: PathBuf,
    pub size: u64,
    pub mode: u32,
    pub mtime_s: i64,
    pub mtime_ns: u32,
    pub target: Vec<u8>,
}

struct Walk {
    // Directories still to read, and how many are being read right now.
    queue: Mutex<(Vec<(PathBuf, Vec<u8>)>, usize)>,
    cv: Condvar,
    entries: Mutex<Vec<Entry>>,
    warnings: Mutex<Vec<String>>,
}

fn entry_of(rel: Vec<u8>, abs: PathBuf, meta: &std::fs::Metadata, kind: u8, target: Vec<u8>) -> Entry {
    Entry {
        kind,
        rel,
        abs,
        size: if kind == KIND_FILE { meta.len() } else { 0 },
        mode: meta.mode() & 0o7777,
        mtime_s: meta.mtime(),
        mtime_ns: meta.mtime_nsec() as u32,
        target,
    }
}

/// The entry for what lies at `abs`, or None with a warning for anything
/// that is not a directory, file or symlink.
fn classify(rel: Vec<u8>, abs: PathBuf, meta: &std::fs::Metadata, warnings: &mut Vec<String>) -> Option<Entry> {
    let ft = meta.file_type();
    if ft.is_dir() {
        Some(entry_of(rel, abs, meta, KIND_DIR, Vec::new()))
    } else if ft.is_file() {
        Some(entry_of(rel, abs, meta, KIND_FILE, Vec::new()))
    } else if ft.is_symlink() {
        match std::fs::read_link(&abs) {
            Ok(t) => Some(entry_of(rel, abs, meta, KIND_LINK, t.as_os_str().as_bytes().to_vec())),
            Err(e) => {
                warnings.push(format!("{}: cannot read link: {e}", abs.display()));
                None
            }
        }
    } else {
        warnings.push(format!("{}: skipped, not a file, directory or symlink", abs.display()));
        None
    }
}

pub fn scan(sources: &[Source], threads: usize) -> (Vec<Entry>, Vec<String>) {
    let walk = Walk {
        queue: Mutex::new((Vec::new(), 0)),
        cv: Condvar::new(),
        entries: Mutex::new(Vec::new()),
        warnings: Mutex::new(Vec::new()),
    };
    {
        let mut queue = walk.queue.lock().unwrap();
        let mut entries = walk.entries.lock().unwrap();
        let mut warnings = walk.warnings.lock().unwrap();
        for s in sources {
            let meta = match std::fs::symlink_metadata(&s.path) {
                Ok(m) => m,
                Err(e) => {
                    warnings.push(format!("{}: {e}", s.path.display()));
                    continue;
                }
            };
            if s.contents {
                if !meta.is_dir() {
                    warnings.push(format!("{}/: not a directory", s.path.display()));
                    continue;
                }
                // The destination itself, for its modification time.
                entries.push(entry_of(Vec::new(), s.path.clone(), &meta, KIND_DIR, Vec::new()));
                queue.0.push((s.path.clone(), Vec::new()));
                continue;
            }
            let name = s.path.file_name().map(|n| n.as_bytes().to_vec()).unwrap_or_default();
            if let Some(entry) = classify(name.clone(), s.path.clone(), &meta, &mut warnings) {
                if entry.kind == KIND_DIR {
                    queue.0.push((s.path.clone(), name));
                }
                entries.push(entry);
            }
        }
    }
    thread::scope(|sc| {
        for _ in 0..threads.max(1) {
            sc.spawn(|| worker(&walk));
        }
    });
    let mut entries = walk.entries.into_inner().unwrap();
    let mut warnings = walk.warnings.into_inner().unwrap();
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    entries.dedup_by(|b, a| {
        if a.rel == b.rel {
            if !a.rel.is_empty() {
                warnings.push(format!("{}: the same destination path as {}, skipped", b.abs.display(), a.abs.display()));
            }
            true
        } else {
            false
        }
    });
    (entries, warnings)
}

fn worker(w: &Walk) {
    loop {
        let (dir, rel) = {
            let mut q = w.queue.lock().unwrap();
            loop {
                if let Some(d) = q.0.pop() {
                    q.1 += 1;
                    break d;
                }
                if q.1 == 0 {
                    return;
                }
                q = w.cv.wait(q).unwrap();
            }
        };
        read_dir(w, &dir, &rel);
        w.queue.lock().unwrap().1 -= 1;
        w.cv.notify_all();
    }
}

fn read_dir(w: &Walk, dir: &Path, rel: &[u8]) {
    let mut found = Vec::new();
    let mut dirs = Vec::new();
    let mut warnings = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(it) => {
            for item in it {
                let item = match item {
                    Ok(i) => i,
                    Err(e) => {
                        warnings.push(format!("{}: {e}", dir.display()));
                        continue;
                    }
                };
                let abs = item.path();
                let mut r = rel.to_vec();
                if !r.is_empty() {
                    r.push(b'/');
                }
                r.extend_from_slice(item.file_name().as_bytes());
                let meta = match std::fs::symlink_metadata(&abs) {
                    Ok(m) => m,
                    Err(e) => {
                        warnings.push(format!("{}: {e}", abs.display()));
                        continue;
                    }
                };
                if let Some(entry) = classify(r.clone(), abs.clone(), &meta, &mut warnings) {
                    if entry.kind == KIND_DIR {
                        dirs.push((abs, r));
                    }
                    found.push(entry);
                }
            }
        }
        Err(e) => warnings.push(format!("{}: {e}", dir.display())),
    }
    if !found.is_empty() {
        w.entries.lock().unwrap().extend(found);
    }
    if !warnings.is_empty() {
        w.warnings.lock().unwrap().extend(warnings);
    }
    if !dirs.is_empty() {
        w.queue.lock().unwrap().0.extend(dirs);
        w.cv.notify_all();
    }
}
