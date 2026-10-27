//! The data port's write FIFO: four entries, each leaving through the access slots a line
//! offers the 68000 side ([`dma::end`]), a VRAM word in two slots and a CRAM or VSRAM word in
//! one. The 68000 waits while all four are taken (the M0.2 spike's model, which removed the
//! extra VBlank of Sonic 1's SEGA-to-title load).
//!
//! Chosen, unverified: a write reaches memory when it is queued, not when it leaves, so the
//! renderer, which draws whole lines, can see a write a few slots early.

use std::collections::VecDeque;

use super::dma;
use crate::timing::Timing;

/// Writes the FIFO holds.
pub(super) const DEPTH: usize = 4;

#[derive(Debug, Clone, Default)]
pub(super) struct Fifo {
    /// Master clocks where the queued writes leave, oldest first; at most [`DEPTH`].
    leaving: VecDeque<u64>,
}

impl Fifo {
    /// Writes still queued at master clock `now`.
    pub fn len(&self, now: u64) -> usize {
        self.leaving.iter().filter(|&&at| at > now).count()
    }

    /// Where the last queued write leaves, `now` when none is queued: from there on the
    /// FIFO is empty.
    pub fn drained(&self, now: u64) -> u64 {
        self.leaving.back().map_or(now, |&at| at.max(now))
    }

    /// Queues a write of `slots` access slots made at `now`. Returns where the 68000 can go
    /// on: `now`, or where the oldest write leaves when all entries are taken.
    pub fn push(&mut self, timing: &Timing, now: u64, slots: u64) -> u64 {
        while self.leaving.front().is_some_and(|&at| at <= now) {
            self.leaving.pop_front();
        }
        let ready = if self.leaving.len() == DEPTH {
            self.leaving.pop_front().unwrap_or(now)
        } else {
            now
        };
        let from = self.drained(ready);
        self.leaving.push_back(dma::end(timing, from, slots));
        ready
    }
}
