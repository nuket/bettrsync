// bettrsync
// Copyright (c) 2026 Max Vilimpoc
// SPDX-License-Identifier: MIT

//! Where a thread's time went, printed at its end when BETTRSYNC_TRACE
//! is set: for finding what a slow sync was waiting on.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Trace {
    who: &'static str,
    on: bool,
    start: Instant,
    spent: Mutex<BTreeMap<&'static str, (Duration, u64)>>,
}

impl Trace {
    pub fn new(who: &'static str) -> Trace {
        Trace { who, on: std::env::var_os("BETTRSYNC_TRACE").is_some(), start: Instant::now(), spent: Mutex::new(BTreeMap::new()) }
    }

    /// Adds the time since `since` to `what`.
    pub fn add(&self, what: &'static str, since: Instant) {
        if self.on {
            let mut spent = self.spent.lock().unwrap();
            let e = spent.entry(what).or_insert((Duration::ZERO, 0));
            e.0 += since.elapsed();
            e.1 += 1;
        }
    }

    pub fn report(&self) {
        if !self.on {
            return;
        }
        let spent = self.spent.lock().unwrap();
        let mut line = format!("trace {} ({:.2} s alive):", self.who, self.start.elapsed().as_secs_f64());
        for (what, (d, n)) in spent.iter() {
            line += &format!(" {what} {:.2} s/{n}", d.as_secs_f64());
        }
        eprintln!("{line}");
    }
}
