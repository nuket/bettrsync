//! The threads that read and hash blocks, and the block geometry both
//! sides agree on: a file is hashed in blocks of `block` bytes, read from
//! disk in chunks of CHUNK_BYTES (or one block, if larger) so the reads
//! are large and sequential while the hashes stay fine-grained.

use std::fs::File;
use std::io;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use xxhash_rust::xxh3::xxh3_128;

pub const CHUNK_BYTES: u64 = 8 << 20;

type Job = Box<dyn FnOnce() + Send + 'static>;

pub struct Pool {
    tx: Option<mpsc::Sender<Job>>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl Pool {
    pub fn new(threads: usize) -> Pool {
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let workers = (0..threads.max(1))
            .map(|_| {
                let rx = rx.clone();
                thread::spawn(move || loop {
                    let job = rx.lock().unwrap().recv();
                    match job {
                        Ok(job) => job(),
                        Err(_) => break,
                    }
                })
            })
            .collect();
        Pool { tx: Some(tx), workers }
    }

    pub fn run(&self, job: impl FnOnce() + Send + 'static) {
        self.tx.as_ref().unwrap().send(Box::new(job)).expect("pool is gone");
    }
}

impl Drop for Pool {
    /// Waits for every job queued so far.
    fn drop(&mut self) {
        self.tx.take();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

/// One read of a file: the blocks it covers and where they lie.
pub struct Chunk {
    pub first_block: u64,
    pub offset: u64,
    pub len: usize,
}

/// The chunks a file of `len` bytes is read in.
pub fn chunks(len: u64, block: usize) -> Vec<Chunk> {
    let block = block as u64;
    let chunk_len = block * (CHUNK_BYTES / block).max(1);
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < len {
        let this = (len - offset).min(chunk_len);
        out.push(Chunk { first_block: offset / block, offset, len: this as usize });
        offset += this;
    }
    out
}

/// `len` bytes of `file` from `offset`, or fewer if the file ends first.
pub fn read_chunk(file: &File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut got = 0;
    while got < len {
        let n = crate::fsutil::read_at(file, &mut buf[got..], offset + got as u64)?;
        if n == 0 {
            break;
        }
        got += n;
    }
    buf.truncate(got);
    Ok(buf)
}

pub fn hash_blocks(data: &[u8], block: usize) -> Vec<u128> {
    data.chunks(block).map(xxh3_128).collect()
}
