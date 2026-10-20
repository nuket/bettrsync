//! The wire protocol between the sending and the receiving side, the same
//! whether the two are threads in one process or a process here and one
//! over ssh.  A message is a tag byte and little-endian fields; a byte
//! string carries a u32 length first.

use std::io::{self, Read, Write};

pub const VERSION: u32 = 1;

pub const KIND_DIR: u8 = 0;
pub const KIND_FILE: u8 = 1;
pub const KIND_LINK: u8 = 2;

pub const DEC_SKIP: u8 = 0; // the destination has it already
pub const DEC_WHOLE: u8 = 1; // nothing there to compare with: send it all
pub const DEC_DELTA: u8 = 2; // a file to update in place, block by block
pub const DEC_FAIL: u8 = 3; // the receiver could not open or make it

const MAX_BYTES: usize = 64 << 20;

#[derive(Debug, Default, Clone)]
pub struct RecvStats {
    pub dirs_made: u64,
    pub links_made: u64,
    pub files_written: u64,
    pub bytes_written: u64,
    pub hashed_bytes: u64,
    pub hash_secs: f64,
    pub warnings: u64,
}

/// One entry of the sender's list.
#[derive(Debug)]
pub struct WireEntry {
    pub id: u32,
    pub kind: u8,
    pub path: Vec<u8>,
    pub size: u64,
    pub mode: u32,
    pub mtime_s: i64,
    pub mtime_ns: u32,
    pub target: Vec<u8>,
}

/// The receiver's answer for one entry.
#[derive(Debug, Clone, Copy)]
pub struct Decision {
    pub id: u32,
    pub kind: u8,
    pub dest_size: u64,
}

/// Entries and decisions travel in batches: one message for a hundred
/// thousand small files would be as bad as one message each.
pub const BATCH: usize = 256;

#[derive(Debug)]
pub enum Msg {
    Hello { version: u32, single_file: bool },
    Entries(Vec<WireEntry>),
    EntriesDone,
    Decisions(Vec<Decision>),
    Hashes { id: u32, first: u64, hashes: Vec<u128> },
    Data { id: u32, offset: u64, data: Vec<u8> },
    FileDone { id: u32 },
    FileAbort { id: u32 },
    Finish,
    Finished { stats: RecvStats },
    Report { text: String },
}

const T_HELLO: u8 = 1;
const T_ENTRIES: u8 = 2;
const T_ENTRIES_DONE: u8 = 3;
const T_DECISIONS: u8 = 4;
const T_HASHES: u8 = 5;
const T_DATA: u8 = 6;
const T_FILE_DONE: u8 = 7;
const T_FILE_ABORT: u8 = 8;
const T_FINISH: u8 = 9;
const T_FINISHED: u8 = 10;
const T_REPORT: u8 = 11;

fn put_u8<W: Write>(w: &mut W, v: u8) -> io::Result<()> {
    w.write_all(&[v])
}

fn put_u32<W: Write>(w: &mut W, v: u32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn put_u64<W: Write>(w: &mut W, v: u64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn put_bytes<W: Write>(w: &mut W, b: &[u8]) -> io::Result<()> {
    put_u32(w, b.len() as u32)?;
    w.write_all(b)
}

fn get_u8<R: Read>(r: &mut R) -> io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn get_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn get_u64<R: Read>(r: &mut R) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn get_u128<R: Read>(r: &mut R) -> io::Result<u128> {
    let mut b = [0u8; 16];
    r.read_exact(&mut b)?;
    Ok(u128::from_le_bytes(b))
}

fn get_bytes<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    let n = get_u32(r)? as usize;
    if n > MAX_BYTES {
        return Err(bad(format!("message of {n} bytes is too long")));
    }
    let mut v = vec![0u8; n];
    r.read_exact(&mut v)?;
    Ok(v)
}

/// A batch's length, within reason.
fn count<R: Read>(r: &mut R) -> io::Result<usize> {
    let n = get_u32(r)? as usize;
    if n > MAX_BYTES / 16 {
        return Err(bad(format!("a batch of {n} is too many")));
    }
    Ok(n)
}

fn bad(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

pub fn write_msg<W: Write>(w: &mut W, m: &Msg) -> io::Result<()> {
    match m {
        Msg::Hello { version, single_file } => {
            put_u8(w, T_HELLO)?;
            put_u32(w, *version)?;
            put_u8(w, *single_file as u8)
        }
        Msg::Entries(entries) => {
            put_u8(w, T_ENTRIES)?;
            put_u32(w, entries.len() as u32)?;
            for e in entries {
                put_u32(w, e.id)?;
                put_u8(w, e.kind)?;
                put_bytes(w, &e.path)?;
                put_u64(w, e.size)?;
                put_u32(w, e.mode)?;
                put_u64(w, e.mtime_s as u64)?;
                put_u32(w, e.mtime_ns)?;
                put_bytes(w, &e.target)?;
            }
            Ok(())
        }
        Msg::EntriesDone => put_u8(w, T_ENTRIES_DONE),
        Msg::Decisions(decisions) => {
            put_u8(w, T_DECISIONS)?;
            put_u32(w, decisions.len() as u32)?;
            for d in decisions {
                put_u32(w, d.id)?;
                put_u8(w, d.kind)?;
                put_u64(w, d.dest_size)?;
            }
            Ok(())
        }
        Msg::Hashes { id, first, hashes } => {
            put_u8(w, T_HASHES)?;
            put_u32(w, *id)?;
            put_u64(w, *first)?;
            put_u32(w, hashes.len() as u32)?;
            for h in hashes {
                w.write_all(&h.to_le_bytes())?;
            }
            Ok(())
        }
        Msg::Data { id, offset, data } => {
            put_u8(w, T_DATA)?;
            put_u32(w, *id)?;
            put_u64(w, *offset)?;
            put_bytes(w, data)
        }
        Msg::FileDone { id } => {
            put_u8(w, T_FILE_DONE)?;
            put_u32(w, *id)
        }
        Msg::FileAbort { id } => {
            put_u8(w, T_FILE_ABORT)?;
            put_u32(w, *id)
        }
        Msg::Finish => put_u8(w, T_FINISH),
        Msg::Finished { stats } => {
            put_u8(w, T_FINISHED)?;
            for v in [stats.dirs_made, stats.links_made, stats.files_written, stats.bytes_written, stats.hashed_bytes, stats.warnings] {
                put_u64(w, v)?;
            }
            put_u64(w, stats.hash_secs.to_bits())
        }
        Msg::Report { text } => {
            put_u8(w, T_REPORT)?;
            put_bytes(w, text.as_bytes())
        }
    }
}

pub fn read_msg<R: Read>(r: &mut R) -> io::Result<Msg> {
    Ok(match get_u8(r)? {
        T_HELLO => Msg::Hello { version: get_u32(r)?, single_file: get_u8(r)? != 0 },
        T_ENTRIES => {
            let n = count(r)?;
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                entries.push(WireEntry {
                    id: get_u32(r)?,
                    kind: get_u8(r)?,
                    path: get_bytes(r)?,
                    size: get_u64(r)?,
                    mode: get_u32(r)?,
                    mtime_s: get_u64(r)? as i64,
                    mtime_ns: get_u32(r)?,
                    target: get_bytes(r)?,
                });
            }
            Msg::Entries(entries)
        }
        T_ENTRIES_DONE => Msg::EntriesDone,
        T_DECISIONS => {
            let n = count(r)?;
            let mut decisions = Vec::with_capacity(n);
            for _ in 0..n {
                decisions.push(Decision { id: get_u32(r)?, kind: get_u8(r)?, dest_size: get_u64(r)? });
            }
            Msg::Decisions(decisions)
        }
        T_HASHES => {
            let id = get_u32(r)?;
            let first = get_u64(r)?;
            let n = get_u32(r)? as usize;
            if n > MAX_BYTES / 16 {
                return Err(bad(format!("{n} hashes in one message is too many")));
            }
            let mut hashes = Vec::with_capacity(n);
            for _ in 0..n {
                hashes.push(get_u128(r)?);
            }
            Msg::Hashes { id, first, hashes }
        }
        T_DATA => Msg::Data { id: get_u32(r)?, offset: get_u64(r)?, data: get_bytes(r)? },
        T_FILE_DONE => Msg::FileDone { id: get_u32(r)? },
        T_FILE_ABORT => Msg::FileAbort { id: get_u32(r)? },
        T_FINISH => Msg::Finish,
        T_FINISHED => {
            let mut v = [0u64; 6];
            for x in v.iter_mut() {
                *x = get_u64(r)?;
            }
            let hash_secs = f64::from_bits(get_u64(r)?);
            Msg::Finished {
                stats: RecvStats {
                    dirs_made: v[0],
                    links_made: v[1],
                    files_written: v[2],
                    bytes_written: v[3],
                    hashed_bytes: v[4],
                    warnings: v[5],
                    hash_secs,
                },
            }
        }
        T_REPORT => Msg::Report { text: String::from_utf8_lossy(&get_bytes(r)?).into_owned() },
        t => return Err(bad(format!("unknown message tag {t}"))),
    })
}
