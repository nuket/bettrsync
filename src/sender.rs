//! The sending side: scans the sources, sends the list, and for each file
//! the receiver wants updated, hashes it with every core while the
//! receiver hashes its copy, sends only the blocks that differ, and
//! reports what was done and how long each part took.
//!
//! Threads: the reader takes the receiver's decisions and hashes off the
//! wire; the pool reads and hashes the source in chunks; the comparer
//! takes each chunk, waits for the receiver's hashes of the same blocks,
//! and queues the differing ones for the writer, which puts everything on
//! the wire.  The main thread feeds the pool, in the order the receiver
//! decides, up to `max_open` files at a time.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::fsutil::{commas, human};
use crate::pool::{self, Pool};
use crate::proto::*;
use crate::scan::{self, Entry, Source};

const TICK: Duration = Duration::from_millis(200);

pub struct SendOpts {
    pub sources: Vec<Source>,
    pub commit: bool,
    pub block: usize,
    pub threads: usize,
    /// Files in flight at a time.
    pub max_open: usize,
    pub quiet: bool,
    pub local: bool,
}

/// What a file in flight has come to.
struct Progress {
    rel: Vec<u8>,
    size: u64,
    nblocks: u64,
    dest_blocks: u64,
    chunks_left: usize,
    changed: u64,
    bytes: u64,
    failed: bool,
}

struct Sink {
    local: bool,
    quiet: bool,
    tx: mpsc::SyncSender<Msg>,
}

impl Sink {
    /// A line of the listing, which -q leaves out.
    fn line(&self, text: String) {
        if !self.quiet {
            self.say(text);
        }
    }

    /// A line of the summary.
    fn say(&self, text: String) {
        if self.local {
            println!("{text}");
        } else {
            let _ = self.tx.send(Msg::Report { text });
        }
    }

    fn warn(&self, text: String) {
        if self.local {
            eprintln!("bettrsync: {text}");
        } else {
            let _ = self.tx.send(Msg::Report { text: format!("bettrsync: {text}") });
        }
    }
}

struct Shared {
    opts: SendOpts,
    sink: Sink,
    decisions: Mutex<VecDeque<(u32, u8, u64)>>,
    dcv: Condvar,
    hashes: Mutex<HashMap<u32, Vec<Option<u128>>>>,
    hcv: Condvar,
    inflight: Mutex<HashMap<u32, Progress>>,
    icv: Condvar,
    finished: Mutex<Option<RecvStats>>,
    fcv: Condvar,
    dead: AtomicBool,
    warnings: AtomicU64,
    // Totals, kept by the comparer.
    changed_blocks: AtomicU64,
    sent_bytes: AtomicU64,
    hashed_bytes: AtomicU64,
    updated: AtomicU64,
    unchanged: AtomicU64,
    new_files: AtomicU64,
    failed: AtomicU64,
    pool_trace: crate::trace::Trace,
}

/// A chunk read and (for a delta) hashed by the pool.
struct Res {
    id: u32,
    first_block: u64,
    offset: u64,
    data: io::Result<Vec<u8>>,
    /// The blocks' hashes, for a delta.
    hashes: Vec<u128>,
    delta: bool,
}

fn gone() -> io::Error {
    io::Error::other("the receiver went away")
}

pub fn run(mut r: Box<dyn Read + Send>, mut w: Box<dyn Write + Send>, opts: SendOpts) -> io::Result<bool> {
    let t0 = Instant::now();
    let single_file = opts.sources.len() == 1 && !opts.sources[0].contents && !opts.sources[0].path.is_dir();
    write_msg(&mut w, &Msg::Hello { version: VERSION, single_file })?;
    w.flush()?;
    match read_msg(&mut r)? {
        Msg::Hello { version, .. } if version == VERSION => {}
        Msg::Hello { version, .. } => {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("the receiver speaks protocol {version}, this is {VERSION}")))
        }
        m => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("expected a hello, got {m:?}"))),
    }

    let (tx, rx) = mpsc::sync_channel::<Msg>(16);
    let writer = thread::spawn(move || -> io::Result<()> {
        let trace = crate::trace::Trace::new("sender writer");
        let mut w = BufWriter::with_capacity(1 << 20, w);
        while let Ok(m) = rx.recv() {
            let t = Instant::now();
            write_msg(&mut w, &m)?;
            trace.add("writing messages", t);
            loop {
                match rx.try_recv() {
                    Ok(m) => {
                        let t = Instant::now();
                        write_msg(&mut w, &m)?;
                        trace.add("writing messages", t);
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return w.flush(),
                }
            }
            let t = Instant::now();
            w.flush()?;
            trace.add("flushing", t);
        }
        trace.report();
        w.flush()
    });
    let shared = Arc::new(Shared {
        sink: Sink { local: opts.local, quiet: opts.quiet, tx: tx.clone() },
        opts,
        decisions: Mutex::new(VecDeque::new()),
        dcv: Condvar::new(),
        hashes: Mutex::new(HashMap::new()),
        hcv: Condvar::new(),
        inflight: Mutex::new(HashMap::new()),
        icv: Condvar::new(),
        finished: Mutex::new(None),
        fcv: Condvar::new(),
        dead: AtomicBool::new(false),
        warnings: AtomicU64::new(0),
        changed_blocks: AtomicU64::new(0),
        sent_bytes: AtomicU64::new(0),
        hashed_bytes: AtomicU64::new(0),
        updated: AtomicU64::new(0),
        unchanged: AtomicU64::new(0),
        new_files: AtomicU64::new(0),
        failed: AtomicU64::new(0),
        pool_trace: crate::trace::Trace::new("sender pool"),
    });

    // The scan.
    let (entries, warnings) = scan::scan(&shared.opts.sources, shared.opts.threads);
    for text in warnings {
        shared.warnings.fetch_add(1, Ordering::Relaxed);
        shared.sink.warn(text);
    }
    let t_scan = Instant::now();
    let (mut n_dirs, mut n_files, mut n_links, mut total) = (0u64, 0u64, 0u64, 0u64);
    for e in &entries {
        match e.kind {
            KIND_DIR => n_dirs += 1,
            KIND_LINK => n_links += 1,
            _ => {
                n_files += 1;
                total += e.size;
            }
        }
    }

    let reader = {
        let shared = shared.clone();
        thread::spawn(move || {
            let outcome = read_loop(&shared, r);
            shared.dead.store(true, Ordering::Relaxed);
            outcome
        })
    };

    for (first, batch) in entries.chunks(BATCH).enumerate().map(|(i, b)| (i * BATCH, b)) {
        let batch = batch
            .iter()
            .enumerate()
            .map(|(i, e)| WireEntry {
                id: (first + i) as u32,
                kind: e.kind,
                path: e.rel.clone(),
                size: e.size,
                mode: e.mode,
                mtime_s: e.mtime_s,
                mtime_ns: e.mtime_ns,
                target: e.target.clone(),
            })
            .collect();
        tx.send(Msg::Entries(batch)).map_err(|_| gone())?;
    }
    tx.send(Msg::EntriesDone).map_err(|_| gone())?;

    let (res_tx, res_rx) = mpsc::sync_channel::<Res>(shared.opts.threads.max(1) * 2);
    let comparer = {
        let shared = shared.clone();
        thread::spawn(move || compare_loop(&shared, res_rx))
    };
    let pool = Pool::new(shared.opts.threads);

    let mut new_dirs = 0u64;
    let mut new_links = 0u64;
    let outcome = (|| -> io::Result<()> {
        for _ in 0..entries.len() {
            let (id, kind, dest_size) = {
                let mut d = shared.decisions.lock().unwrap();
                loop {
                    if let Some(x) = d.pop_front() {
                        break x;
                    }
                    if shared.dead.load(Ordering::Relaxed) {
                        return Err(gone());
                    }
                    d = shared.dcv.wait_timeout(d, TICK).unwrap().0;
                }
            };
            let e = &entries[id as usize];
            let name = if e.rel.is_empty() { ".".into() } else { String::from_utf8_lossy(&e.rel) };
            match (e.kind, kind) {
                (_, DEC_FAIL) => {
                    shared.failed.fetch_add(1, Ordering::Relaxed);
                }
                (KIND_DIR, DEC_SKIP) | (KIND_LINK, DEC_SKIP) | (KIND_FILE, DEC_SKIP) => {
                    shared.unchanged.fetch_add(1, Ordering::Relaxed);
                }
                (KIND_DIR, _) => {
                    new_dirs += 1;
                    shared.sink.line(format!("mkdir    {name}/"));
                }
                (KIND_LINK, _) => {
                    new_links += 1;
                    shared.sink.line(format!("link     {name} -> {}", String::from_utf8_lossy(&e.target)));
                }
                (_, _) => feed_file(&shared, &pool, &res_tx, id, e, kind == DEC_DELTA, dest_size)?,
            }
        }
        // Everything in flight, then the receiver's last word.
        {
            let mut i = shared.inflight.lock().unwrap();
            while !i.is_empty() {
                if shared.dead.load(Ordering::Relaxed) {
                    return Err(gone());
                }
                i = shared.icv.wait_timeout(i, TICK).unwrap().0;
            }
        }
        tx.send(Msg::Finish).map_err(|_| gone())?;
        Ok(())
    })();
    drop(pool);
    drop(res_tx);
    let _ = comparer.join();
    shared.pool_trace.report();
    outcome?;

    let stats = {
        let mut f = shared.finished.lock().unwrap();
        loop {
            if let Some(s) = f.take() {
                break s;
            }
            if shared.dead.load(Ordering::Relaxed) {
                return match reader.join() {
                    Ok(Err(e)) => Err(e),
                    _ => Err(gone()),
                };
            }
            f = shared.fcv.wait_timeout(f, TICK).unwrap().0;
        }
    };
    let t_end = Instant::now();

    // The report.
    let s = &shared;
    let sent = s.sent_bytes.load(Ordering::Relaxed);
    let here = s.hashed_bytes.load(Ordering::Relaxed);
    let there = stats.hashed_bytes;
    let sync_secs = (t_end - t_scan).as_secs_f64();
    let blocks = s.changed_blocks.load(Ordering::Relaxed);
    s.sink.say(String::new());
    s.sink.say(format!(
        "scanned   {:6.2} s   {} entries: {} dirs, {} files ({}), {} links",
        (t_scan - t0).as_secs_f64(),
        commas(entries.len() as u64),
        commas(n_dirs),
        commas(n_files),
        human(total),
        commas(n_links)
    ));
    if here + there > 0 {
        s.sink.say(format!(
            "compared  {:6.2} s   hashed {} here, {} there ({}/s)",
            sync_secs,
            human(here),
            human(there),
            human(((here + there) as f64 / sync_secs.max(1e-9)) as u64)
        ));
    }
    s.sink.say(format!(
        "{}  {:6.2} s   {} in {} blocks of {} ({}/s)",
        if s.opts.commit { "sent    " } else { "to send " },
        sync_secs,
        human(sent),
        commas(blocks),
        human(s.opts.block as u64),
        human((sent as f64 / sync_secs.max(1e-9)) as u64)
    ));
    s.sink.say(format!(
        "total     {:6.2} s   unchanged {}, updated {}, new {} files, {} dirs, {} links, failed {}",
        (t_end - t0).as_secs_f64(),
        commas(s.unchanged.load(Ordering::Relaxed)),
        commas(s.updated.load(Ordering::Relaxed)),
        commas(s.new_files.load(Ordering::Relaxed)),
        commas(new_dirs),
        commas(new_links),
        commas(s.failed.load(Ordering::Relaxed))
    ));
    if !s.opts.commit {
        s.sink.say("dry run: nothing was written; add --commit to write".to_string());
    }
    let clean = s.warnings.load(Ordering::Relaxed) == 0 && stats.warnings == 0 && s.failed.load(Ordering::Relaxed) == 0;

    drop(tx);
    drop(shared);
    writer.join().unwrap_or_else(|_| Err(io::Error::other("the writer thread died")))?;
    match reader.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(io::Error::other("the reader thread died")),
    }
    Ok(clean)
}

fn read_loop(shared: &Shared, r: Box<dyn Read + Send>) -> io::Result<()> {
    let mut r = BufReader::with_capacity(1 << 20, r);
    let trace = crate::trace::Trace::new("sender reader");
    loop {
        let t = Instant::now();
        let msg = read_msg(&mut r)?;
        trace.add("reading messages", t);
        match msg {
            Msg::Decisions(decisions) => {
                let mut d = shared.decisions.lock().unwrap();
                d.extend(decisions.iter().map(|d| (d.id, d.kind, d.dest_size)));
                drop(d);
                shared.dcv.notify_one();
            }
            Msg::Hashes { id, first, hashes } => {
                let mut h = shared.hashes.lock().unwrap();
                let v = h.entry(id).or_default();
                let end = first as usize + hashes.len();
                if v.len() < end {
                    v.resize(end, None);
                }
                for (i, x) in hashes.into_iter().enumerate() {
                    v[first as usize + i] = Some(x);
                }
                drop(h);
                shared.hcv.notify_all();
            }
            Msg::Finished { stats } => {
                *shared.finished.lock().unwrap() = Some(stats);
                shared.fcv.notify_all();
                trace.report();
                return Ok(());
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

/// Queues a file's chunks on the pool, once there is room among the
/// files in flight.
fn feed_file(shared: &Arc<Shared>, pool: &Pool, res_tx: &mpsc::SyncSender<Res>, id: u32, e: &Entry, delta: bool, dest_size: u64) -> io::Result<()> {
    let block = shared.opts.block as u64;
    {
        let mut i = shared.inflight.lock().unwrap();
        while i.len() >= shared.opts.max_open {
            if shared.dead.load(Ordering::Relaxed) {
                return Err(gone());
            }
            i = shared.icv.wait_timeout(i, TICK).unwrap().0;
        }
    }
    let name = String::from_utf8_lossy(&e.rel).into_owned();
    if !delta && !shared.opts.commit {
        // Nothing to compare: a dry run need not even read it.
        shared.new_files.fetch_add(1, Ordering::Relaxed);
        shared.sent_bytes.fetch_add(e.size, Ordering::Relaxed);
        shared.changed_blocks.fetch_add((e.size + block - 1) / block, Ordering::Relaxed);
        shared.sink.line(format!("new      {name}  {}", human(e.size)));
        shared.sink.tx.send(Msg::FileDone { id }).map_err(|_| gone())?;
        return Ok(());
    }
    let file = match File::open(&e.abs) {
        Ok(f) => Arc::new(f),
        Err(err) => {
            shared.warnings.fetch_add(1, Ordering::Relaxed);
            shared.failed.fetch_add(1, Ordering::Relaxed);
            shared.sink.warn(format!("{}: cannot open: {err}", e.abs.display()));
            shared.sink.tx.send(Msg::FileAbort { id }).map_err(|_| gone())?;
            return Ok(());
        }
    };
    let chunks = pool::chunks(e.size, shared.opts.block);
    let nblocks = (e.size + block - 1) / block;
    let dest_blocks = if delta { (dest_size.min(e.size) + block - 1) / block } else { 0 };
    if delta {
        shared.hashed_bytes.fetch_add(e.size, Ordering::Relaxed);
    }
    if chunks.is_empty() {
        // An empty file: the receiver truncates and stamps it.
        if delta {
            shared.updated.fetch_add(1, Ordering::Relaxed);
            shared.sink.line(format!("update   {name}  0 of 0 blocks"));
        } else {
            shared.new_files.fetch_add(1, Ordering::Relaxed);
            shared.sink.line(format!("new      {name}  0 B"));
        }
        shared.sink.tx.send(Msg::FileDone { id }).map_err(|_| gone())?;
        return Ok(());
    }
    shared.inflight.lock().unwrap().insert(
        id,
        Progress { rel: e.rel.clone(), size: e.size, nblocks, dest_blocks, chunks_left: chunks.len(), changed: 0, bytes: 0, failed: false },
    );
    for chunk in chunks {
        let file = file.clone();
        let res_tx = res_tx.clone();
        let shared = shared.clone();
        pool.run(move || {
            let t = Instant::now();
            let data = pool::read_chunk(&file, chunk.offset, chunk.len);
            shared.pool_trace.add("reading", t);
            let t = Instant::now();
            let hashes = match (&data, delta) {
                (Ok(d), true) => pool::hash_blocks(d, block as usize),
                _ => Vec::new(),
            };
            shared.pool_trace.add("hashing", t);
            let t = Instant::now();
            let _ = res_tx.send(Res { id, first_block: chunk.first_block, offset: chunk.offset, data, hashes, delta });
            shared.pool_trace.add("handing over", t);
        });
    }
    Ok(())
}

/// The comparer: each chunk against the receiver's hashes of it.
fn compare_loop(shared: &Arc<Shared>, rx: mpsc::Receiver<Res>) {
    let block = shared.opts.block;
    let trace = crate::trace::Trace::new("sender comparer");
    loop {
        let t = Instant::now();
        let Ok(res) = rx.recv() else { break };
        trace.add("waiting for chunks", t);
        let mut changed = 0u64;
        let mut bytes = 0u64;
        let mut failed = false;
        match res.data {
            Err(e) => {
                shared.warnings.fetch_add(1, Ordering::Relaxed);
                shared.sink.warn(format!("read failed at {}: {e}", res.offset));
                failed = true;
            }
            Ok(data) => {
                let n = (data.len() + block - 1) / block;
                let mut differs = vec![true; n];
                if res.delta {
                    let dest_blocks = shared.inflight.lock().unwrap().get(&res.id).map(|p| p.dest_blocks).unwrap_or(0);
                    let last = (res.first_block + n as u64).min(dest_blocks);
                    if res.first_block < last {
                        let mine = &res.hashes;
                        let t = Instant::now();
                        let theirs = wait_hashes(shared, res.id, res.first_block, last);
                        trace.add("waiting for their hashes", t);
                        let Some(theirs) = theirs else { break };
                        for (i, h) in theirs.iter().enumerate() {
                            differs[i] = *h != mine[i];
                        }
                    }
                }
                // Differing blocks go out in runs, the fewer messages the better.
                let mut i = 0;
                while i < n {
                    if !differs[i] {
                        i += 1;
                        continue;
                    }
                    let start = i;
                    while i < n && differs[i] {
                        i += 1;
                    }
                    let from = start * block;
                    let to = (i * block).min(data.len());
                    changed += (i - start) as u64;
                    bytes += (to - from) as u64;
                    if shared.opts.commit {
                        let m = Msg::Data { id: res.id, offset: res.offset + from as u64, data: data[from..to].to_vec() };
                        let t = Instant::now();
                        if shared.sink.tx.send(m).is_err() {
                            return;
                        }
                        trace.add("queueing data", t);
                    }
                }
            }
        }
        shared.changed_blocks.fetch_add(changed, Ordering::Relaxed);
        shared.sent_bytes.fetch_add(bytes, Ordering::Relaxed);
        // The file stays in flight until its FileDone is on the channel:
        // the main thread sends Finish once nothing is in flight, and
        // Finish must follow every FileDone.
        let done = {
            let mut inflight = shared.inflight.lock().unwrap();
            let Some(p) = inflight.get_mut(&res.id) else { continue };
            p.changed += changed;
            p.bytes += bytes;
            p.failed |= failed;
            p.chunks_left -= 1;
            if p.chunks_left == 0 {
                Some(Progress { rel: std::mem::take(&mut p.rel), ..*p })
            } else {
                None
            }
        };
        if let Some(p) = done {
            shared.hashes.lock().unwrap().remove(&res.id);
            let name = String::from_utf8_lossy(&p.rel);
            if p.failed {
                shared.failed.fetch_add(1, Ordering::Relaxed);
                let _ = shared.sink.tx.send(Msg::FileAbort { id: res.id });
            } else if res.delta {
                if p.changed == 0 {
                    shared.unchanged.fetch_add(1, Ordering::Relaxed);
                } else {
                    shared.updated.fetch_add(1, Ordering::Relaxed);
                    shared.sink.line(format!("update   {name}  {} of {} blocks, {}", commas(p.changed), commas(p.nblocks), human(p.bytes)));
                }
                let _ = shared.sink.tx.send(Msg::FileDone { id: res.id });
            } else {
                shared.new_files.fetch_add(1, Ordering::Relaxed);
                shared.sink.line(format!("new      {name}  {}", human(p.size)));
                let _ = shared.sink.tx.send(Msg::FileDone { id: res.id });
            }
            shared.inflight.lock().unwrap().remove(&res.id);
            shared.icv.notify_all();
        }
    }
    trace.report();
}

/// The receiver's hashes of blocks [first, last) of file `id`, once they
/// are all here; None if the receiver is gone.
fn wait_hashes(shared: &Shared, id: u32, first: u64, last: u64) -> Option<Vec<u128>> {
    let (first, last) = (first as usize, last as usize);
    let mut h = shared.hashes.lock().unwrap();
    loop {
        if let Some(v) = h.get(&id) {
            if v.len() >= last && v[first..last].iter().all(|x| x.is_some()) {
                return Some(v[first..last].iter().map(|x| x.unwrap()).collect());
            }
        }
        if shared.dead.load(Ordering::Relaxed) {
            return None;
        }
        h = shared.hcv.wait_timeout(h, TICK).unwrap().0;
    }
}
