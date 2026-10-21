//! The source tree, walked by several threads at once: every directory,
//! regular file and symlink under the sources, with the path each has
//! under the destination.  The entries are handed out a directory at a
//! time as the walk goes, sorted within the directory, and a directory
//! is always handed out before anything in it.

use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::thread;

use crate::filter::Excludes;
use crate::fsutil;
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

/// Takes a directory's worth of entries, and any warnings, as the walk
/// finds them.
pub type Emit<'a> = &'a (dyn Fn(Vec<Entry>, Vec<String>) + Sync);

struct Walk<'a> {
    // Directories still to read, and how many are being read right now.
    queue: Mutex<(Vec<(PathBuf, Vec<u8>)>, usize)>,
    cv: Condvar,
    emit: Emit<'a>,
    excludes: &'a Excludes,
}

fn entry_of(rel: Vec<u8>, abs: PathBuf, meta: &std::fs::Metadata, kind: u8, target: Vec<u8>) -> Entry {
    let (mtime_s, mtime_ns) = fsutil::mtime(meta);
    Entry { kind, rel, abs, size: if kind == KIND_FILE { meta.len() } else { 0 }, mode: fsutil::mode_of(meta), mtime_s, mtime_ns, target }
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
            Ok(t) => Some(entry_of(rel, abs, meta, KIND_LINK, fsutil::target_bytes(&t))),
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

pub fn scan(sources: &[Source], threads: usize, excludes: &Excludes, emit: Emit) {
    let walk = Walk { queue: Mutex::new((Vec::new(), 0)), cv: Condvar::new(), emit, excludes };
    {
        let mut queue = walk.queue.lock().unwrap();
        let mut entries = Vec::new();
        let mut warnings = Vec::new();
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
            let name = s.path.file_name().map(|n| fsutil::name_bytes(n).to_vec()).unwrap_or_default();
            if let Some(entry) = classify(name.clone(), s.path.clone(), &meta, &mut warnings) {
                if entry.kind == KIND_DIR {
                    queue.0.push((s.path.clone(), name));
                }
                entries.push(entry);
            }
        }
        // The sources themselves go out before any directory is read.
        (walk.emit)(entries, warnings);
    }
    thread::scope(|sc| {
        for _ in 0..threads.max(1) {
            sc.spawn(|| worker(&walk));
        }
    });
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
                r.extend_from_slice(fsutil::name_bytes(&item.file_name()));
                let meta = match std::fs::symlink_metadata(&abs) {
                    Ok(m) => m,
                    Err(e) => {
                        warnings.push(format!("{}: {e}", abs.display()));
                        continue;
                    }
                };
                if w.excludes.matches(&r, meta.is_dir()) {
                    continue;
                }
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
    // Handed out before the directories in it are queued, so that
    // whoever reads them finds their parent already sent.
    if !found.is_empty() || !warnings.is_empty() {
        found.sort_by(|a, b| a.rel.cmp(&b.rel));
        (w.emit)(found, warnings);
    }
    if !dirs.is_empty() {
        w.queue.lock().unwrap().0.extend(dirs);
        w.cv.notify_all();
    }
}
