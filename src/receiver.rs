//! The receiving side: takes the sender's list of entries, decides for
//! each what the destination needs (nothing, the whole file, or a
//! block-by-block update), hashes the files to update with every core,
//! and writes what the sender then sends, in place.
//!
//! Three threads: the reader, which takes the sender's messages off the
//! wire and writes file data; the decider, which walks the entries in
//! order, makes directories and links, opens files and queues their
//! hashing; and the writer, which puts the messages for the sender on
//! the wire.  The pool hashes.  Up to MAX_OPEN files are open at a time:
//! the decider waits for the sender to finish with one before opening
//! the next, so neither side runs out of descriptors or memory.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::fsutil;
use crate::pool::{self, Pool};
use crate::proto::*;

const MAX_OPEN: usize = 64;
const TICK: Duration = Duration::from_millis(200);

pub struct RecvOpts {
    pub dest: PathBuf,
    pub commit: bool,
    pub checksum: bool,
    pub block: usize,
    pub threads: usize,
    /// This process is the one the user ran: warnings go to stderr, and
    /// the sender's report lines to stdout.  Else they go over the wire.
    pub local: bool,
}

struct Ent {
    kind: u8,
    rel: Vec<u8>,
    size: u64,
    mode: u32,
    mtime_s: i64,
    mtime_ns: u32,
    target: Vec<u8>,
}

struct Open {
    file: Option<Arc<File>>,
    path: PathBuf,
    tmp: Option<PathBuf>,
    size: u64,
    mtime_s: i64,
    mtime_ns: u32,
}

/// Where warnings go.
struct Sink {
    local: bool,
    tx: mpsc::Sender<Msg>,
    warnings: AtomicU64,
}

impl Sink {
    fn warn(&self, text: String) {
        self.warnings.fetch_add(1, Ordering::Relaxed);
        if self.local {
            eprintln!("bettrsync: {text}");
        } else {
            let _ = self.tx.send(Msg::Report { text: format!("bettrsync: {text}") });
        }
    }
}

struct Shared {
    opts: RecvOpts,
    single: bool,
    sink: Sink,
    // Entries as they arrive, consumed by the decider; the flag says the
    // list is complete.
    entries: Mutex<(VecDeque<(u32, Ent)>, bool)>,
    ecv: Condvar,
    open: Mutex<HashMap<u32, Open>>,
    ocv: Condvar,
    dirs: Mutex<Vec<(PathBuf, i64, u32)>>,
    dead: AtomicBool,
    hashed_bytes: AtomicU64,
    hash_first: Mutex<Option<Instant>>,
    hash_last: Mutex<Option<Instant>>,
    stats: Mutex<RecvStats>,
}

impl Shared {
    fn dest_path(&self, rel: &[u8]) -> PathBuf {
        if self.single {
            self.opts.dest.clone()
        } else {
            self.opts.dest.join(OsStr::from_bytes(rel))
        }
    }

    fn send(&self, m: Msg) {
        let _ = self.sink.tx.send(m);
    }
}

pub fn run(mut r: Box<dyn Read + Send>, mut w: Box<dyn Write + Send>, opts: RecvOpts) -> io::Result<RecvStats> {
    // The handshake, before any thread touches the wire.
    let single_file = match read_msg(&mut r)? {
        Msg::Hello { version, single_file } => {
            if version != VERSION {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("the sender speaks protocol {version}, this is {VERSION}")));
            }
            single_file
        }
        m => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("expected a hello, got {m:?}"))),
    };
    write_msg(&mut w, &Msg::Hello { version: VERSION, single_file: false })?;
    w.flush()?;

    let single = single_file && !opts.dest.is_dir();
    if opts.commit && !single {
        fs::create_dir_all(&opts.dest)?;
    }
    let (tx, rx) = mpsc::channel::<Msg>();
    let shared = Arc::new(Shared {
        single,
        sink: Sink { local: opts.local, tx, warnings: AtomicU64::new(0) },
        opts,
        entries: Mutex::new((VecDeque::new(), false)),
        ecv: Condvar::new(),
        open: Mutex::new(HashMap::new()),
        ocv: Condvar::new(),
        dirs: Mutex::new(Vec::new()),
        dead: AtomicBool::new(false),
        hashed_bytes: AtomicU64::new(0),
        hash_first: Mutex::new(None),
        hash_last: Mutex::new(None),
        stats: Mutex::new(RecvStats::default()),
    });

    let writer = thread::spawn(move || -> io::Result<()> {
        let mut w = BufWriter::with_capacity(1 << 20, w);
        while let Ok(m) = rx.recv() {
            write_msg(&mut w, &m)?;
            loop {
                match rx.try_recv() {
                    Ok(m) => write_msg(&mut w, &m)?,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return w.flush(),
                }
            }
            w.flush()?;
        }
        w.flush()
    });
    let decider = {
        let shared = shared.clone();
        thread::spawn(move || decide(&shared))
    };

    let result = read_loop(&shared, &mut r);
    shared.dead.store(true, Ordering::Relaxed);
    shared.ecv.notify_all();
    shared.ocv.notify_all();
    let _ = decider.join();
    // The stats went out with Finished; nothing more to say.
    drop(shared);
    let written = writer.join().unwrap_or_else(|_| Err(io::Error::other("the writer thread died")));
    let stats = result?;
    written?;
    Ok(stats)
}

fn read_loop(shared: &Arc<Shared>, r: &mut Box<dyn Read + Send>) -> io::Result<RecvStats> {
    let mut r = BufReader::with_capacity(1 << 20, r);
    loop {
        match read_msg(&mut r)? {
            Msg::Entry { id, kind, path, size, mode, mtime_s, mtime_ns, target } => {
                let ent = Ent { kind, rel: path, size, mode, mtime_s, mtime_ns, target };
                shared.entries.lock().unwrap().0.push_back((id, ent));
                shared.ecv.notify_one();
            }
            Msg::EntriesDone => {
                shared.entries.lock().unwrap().1 = true;
                shared.ecv.notify_all();
            }
            Msg::Data { id, offset, data } => {
                let file = shared.open.lock().unwrap().get(&id).and_then(|o| o.file.clone());
                if let Some(file) = file {
                    if let Err(e) = file.write_all_at(&data, offset) {
                        let path = shared.open.lock().unwrap().get(&id).map(|o| o.path.clone()).unwrap_or_default();
                        shared.sink.warn(format!("{}: write failed: {e}", path.display()));
                    } else {
                        shared.stats.lock().unwrap().bytes_written += data.len() as u64;
                    }
                }
            }
            Msg::FileDone { id } => {
                let open = shared.open.lock().unwrap().remove(&id);
                shared.ocv.notify_all();
                if let Some(o) = open {
                    finish_file(shared, o);
                }
            }
            Msg::FileAbort { id } => {
                let open = shared.open.lock().unwrap().remove(&id);
                shared.ocv.notify_all();
                if let Some(o) = open {
                    if let Some(tmp) = o.tmp {
                        let _ = fs::remove_file(tmp);
                    }
                }
            }
            Msg::Finish => {
                // Every decision was made before the sender said this.
                if shared.opts.commit {
                    for (path, s, ns) in shared.dirs.lock().unwrap().iter().rev() {
                        let same = fs::symlink_metadata(path).map(|m| m.mtime() == *s && m.mtime_nsec() as u32 == *ns).unwrap_or(true);
                        if !same {
                            if let Err(e) = fsutil::set_mtime(path, *s, *ns) {
                                shared.sink.warn(format!("{}: cannot set time: {e}", path.display()));
                            }
                        }
                    }
                }
                let mut stats = shared.stats.lock().unwrap().clone();
                stats.hashed_bytes = shared.hashed_bytes.load(Ordering::Relaxed);
                stats.warnings = shared.sink.warnings.load(Ordering::Relaxed);
                if let (Some(a), Some(b)) = (*shared.hash_first.lock().unwrap(), *shared.hash_last.lock().unwrap()) {
                    stats.hash_secs = (b - a).as_secs_f64();
                }
                shared.send(Msg::Finished { stats: stats.clone() });
                return Ok(stats);
            }
            Msg::Report { text } => {
                if shared.opts.local {
                    println!("{text}");
                }
            }
            m => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected message {m:?}"))),
        }
    }
}

fn finish_file(shared: &Shared, o: Open) {
    if !shared.opts.commit {
        return;
    }
    let Some(file) = o.file else { return };
    let outcome = (|| -> io::Result<()> {
        file.set_len(o.size)?;
        fsutil::set_mtime_fd(&file, o.mtime_s, o.mtime_ns)?;
        drop(file);
        if let Some(tmp) = &o.tmp {
            fs::rename(tmp, &o.path)?;
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => shared.stats.lock().unwrap().files_written += 1,
        Err(e) => {
            shared.sink.warn(format!("{}: {e}", o.path.display()));
            if let Some(tmp) = o.tmp {
                let _ = fs::remove_file(tmp);
            }
        }
    }
}

fn tmp_name(path: &PathBuf) -> PathBuf {
    let mut name = b".".to_vec();
    name.extend_from_slice(path.file_name().map(|n| n.as_bytes()).unwrap_or(b"file"));
    name.extend_from_slice(b".bettrsync.tmp");
    path.with_file_name(OsStr::from_bytes(&name))
}

/// The decider: entries in order, each answered with a Decision.
fn decide(shared: &Arc<Shared>) {
    let pool = Pool::new(shared.opts.threads);
    loop {
        let next = {
            let mut e = shared.entries.lock().unwrap();
            loop {
                if let Some(next) = e.0.pop_front() {
                    break Some(next);
                }
                if e.1 || shared.dead.load(Ordering::Relaxed) {
                    break None;
                }
                e = shared.ecv.wait_timeout(e, TICK).unwrap().0;
            }
        };
        let Some((id, ent)) = next else { return };
        let (kind, dest_size) = match ent.kind {
            KIND_DIR => (decide_dir(shared, &ent), 0),
            KIND_LINK => (decide_link(shared, &ent), 0),
            _ => decide_file(shared, &pool, id, &ent),
        };
        shared.send(Msg::Decision { id, kind, dest_size });
    }
}

fn decide_dir(shared: &Shared, ent: &Ent) -> u8 {
    let path = shared.dest_path(&ent.rel);
    let exists = match fs::symlink_metadata(&path) {
        Ok(m) if m.is_dir() => true,
        Ok(_) => {
            if shared.opts.commit {
                if let Err(e) = fsutil::remove_any(&path) {
                    shared.sink.warn(format!("{}: cannot replace with a directory: {e}", path.display()));
                    return DEC_FAIL;
                }
            }
            false
        }
        Err(_) => false,
    };
    if !exists && shared.opts.commit {
        if let Err(e) = fs::create_dir(&path) {
            shared.sink.warn(format!("{}: cannot make directory: {e}", path.display()));
            return DEC_FAIL;
        }
    }
    shared.dirs.lock().unwrap().push((path, ent.mtime_s, ent.mtime_ns));
    if exists {
        DEC_SKIP
    } else {
        shared.stats.lock().unwrap().dirs_made += 1;
        DEC_WHOLE
    }
}

fn decide_link(shared: &Shared, ent: &Ent) -> u8 {
    let path = shared.dest_path(&ent.rel);
    let meta = fs::symlink_metadata(&path).ok();
    if let Some(m) = &meta {
        if m.file_type().is_symlink() && fs::read_link(&path).map(|t| t.as_os_str().as_bytes() == &ent.target[..]).unwrap_or(false) {
            if m.mtime() != ent.mtime_s && shared.opts.commit {
                let _ = fsutil::set_mtime(&path, ent.mtime_s, ent.mtime_ns);
            }
            return DEC_SKIP;
        }
    }
    if shared.opts.commit {
        let made = (|| -> io::Result<()> {
            if meta.is_some() {
                fsutil::remove_any(&path)?;
            }
            std::os::unix::fs::symlink(OsStr::from_bytes(&ent.target), &path)?;
            fsutil::set_mtime(&path, ent.mtime_s, ent.mtime_ns)
        })();
        if let Err(e) = made {
            shared.sink.warn(format!("{}: cannot make link: {e}", path.display()));
            return DEC_FAIL;
        }
    }
    shared.stats.lock().unwrap().links_made += 1;
    DEC_WHOLE
}

fn decide_file(shared: &Arc<Shared>, pool: &Pool, id: u32, ent: &Ent) -> (u8, u64) {
    let path = shared.dest_path(&ent.rel);
    let commit = shared.opts.commit;
    let existing = match fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_file() => {
            // The same size and time: ours was set to the nanosecond, so
            // the nanoseconds count too, unless this file system keeps none.
            let same_time = m.mtime() == ent.mtime_s && (m.mtime_nsec() as u32 == ent.mtime_ns || m.mtime_nsec() == 0);
            if !shared.opts.checksum && m.len() == ent.size && same_time {
                return (DEC_SKIP, 0);
            }
            Some(m)
        }
        Ok(_) => {
            if commit {
                if let Err(e) = fsutil::remove_any(&path) {
                    shared.sink.warn(format!("{}: cannot replace: {e}", path.display()));
                    return (DEC_FAIL, 0);
                }
            }
            None
        }
        Err(_) => None,
    };
    // A slot: the sender finishes with a file before we open the 65th.
    {
        let mut open = shared.open.lock().unwrap();
        while open.len() >= MAX_OPEN {
            if shared.dead.load(Ordering::Relaxed) {
                return (DEC_FAIL, 0);
            }
            open = shared.ocv.wait_timeout(open, TICK).unwrap().0;
        }
    }
    let mut o = Open { file: None, path: path.clone(), tmp: None, size: ent.size, mtime_s: ent.mtime_s, mtime_ns: ent.mtime_ns };
    let decision = match existing {
        Some(m) => {
            let opened = OpenOptions::new().read(true).write(commit).open(&path);
            match opened {
                Ok(f) => {
                    let file = Arc::new(f);
                    o.file = Some(file.clone());
                    queue_hashes(shared, pool, id, file, m.len().min(ent.size));
                    (DEC_DELTA, m.len())
                }
                Err(e) => {
                    shared.sink.warn(format!("{}: cannot open: {e}", path.display()));
                    return (DEC_FAIL, 0);
                }
            }
        }
        None => {
            if commit {
                let tmp = tmp_name(&path);
                match OpenOptions::new().write(true).create(true).truncate(true).mode(ent.mode | 0o600).open(&tmp) {
                    Ok(f) => {
                        o.file = Some(Arc::new(f));
                        o.tmp = Some(tmp);
                    }
                    Err(e) => {
                        shared.sink.warn(format!("{}: cannot create: {e}", path.display()));
                        return (DEC_FAIL, 0);
                    }
                }
            }
            (DEC_WHOLE, 0)
        }
    };
    shared.open.lock().unwrap().insert(id, o);
    decision
}

/// Hashes the first `len` bytes of `file` on the pool, a chunk per job,
/// each sent to the sender as it is done.
fn queue_hashes(shared: &Arc<Shared>, pool: &Pool, id: u32, file: Arc<File>, len: u64) {
    let block = shared.opts.block;
    {
        let mut first = shared.hash_first.lock().unwrap();
        if first.is_none() {
            *first = Some(Instant::now());
        }
    }
    for chunk in pool::chunks(len, block) {
        let shared = shared.clone();
        let file = file.clone();
        pool.run(move || {
            let want = (chunk.len + block - 1) / block;
            let hashes = match pool::read_chunk(&file, chunk.offset, chunk.len) {
                Ok(data) => {
                    let mut h = pool::hash_blocks(&data, block);
                    // Read short (the file shrank under us): the rest is sent.
                    h.resize(want, 0);
                    h
                }
                Err(e) => {
                    shared.sink.warn(format!("read failed while hashing: {e}"));
                    vec![0; want]
                }
            };
            shared.hashed_bytes.fetch_add(chunk.len as u64, Ordering::Relaxed);
            *shared.hash_last.lock().unwrap() = Some(Instant::now());
            shared.send(Msg::Hashes { id, first: chunk.first_block, hashes });
        });
    }
}
