//! When the run thread starts each frame.
//!
//! The run keeps its own schedule of one frame per [`FRAME_PERIOD`], so it neither drifts
//! with the display nor waits for the window. It catches up after a short delay, at most
//! [`MAX_LAG`] of frames back to back; after a longer one, and whenever the run was held or
//! rebuilt, the schedule starts afresh from the present in a new epoch. Time missed then is
//! dropped, never caught up. Anything kept in step with the schedule (sound) restarts with
//! each new epoch.

use std::time::{Duration, Instant};

use crate::FRAME_PERIOD;

/// How far the run may fall behind its schedule before it stops catching up: two frames.
pub const MAX_LAG: Duration = FRAME_PERIOD.saturating_mul(2);

/// The run's frame schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pacer {
    /// When the next frame is due.
    due: Instant,
    /// How many times the schedule has started afresh.
    epoch: u64,
}

impl Pacer {
    /// A schedule whose first frame is due at `now`.
    pub fn new(now: Instant) -> Self {
        Self { due: now, epoch: 0 }
    }

    pub fn due(&self) -> Instant {
        self.due
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Moves on after the frame that was due ran, given that it is `now`: the next one is due
    /// a period later, unless that is more than [`MAX_LAG`] past, when the schedule starts
    /// afresh at `now`.
    pub fn advance(&mut self, now: Instant) {
        let next = self.due + FRAME_PERIOD;
        if now.saturating_duration_since(next) > MAX_LAG {
            self.restart(now);
        } else {
            self.due = next;
        }
    }

    /// Starts the schedule afresh at `now`, after the run was held or rebuilt: the next frame
    /// is due at once.
    pub fn restart(&mut self, now: Instant) {
        self.due = now;
        self.epoch += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_lag_is_two_frames() {
        assert_eq!(MAX_LAG, FRAME_PERIOD * 2);
    }

    #[test]
    fn test_pacer_first_frame_is_due_at_once() {
        let start = Instant::now();
        let pacer = Pacer::new(start);
        assert_eq!(pacer.due(), start);
        assert_eq!(pacer.epoch(), 0);
    }

    #[test]
    fn test_pacer_advance_keeps_the_schedule_while_on_time() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        pacer.advance(start);
        assert_eq!(pacer.due(), start + FRAME_PERIOD);
        pacer.advance(start + FRAME_PERIOD + Duration::from_millis(3));
        assert_eq!(pacer.due(), start + FRAME_PERIOD * 2);
        assert_eq!(pacer.epoch(), 0);
    }

    #[test]
    fn test_pacer_advance_catches_up_at_most_max_lag() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        pacer.advance(start + FRAME_PERIOD + MAX_LAG);
        assert_eq!(pacer.due(), start + FRAME_PERIOD);
        assert_eq!(pacer.epoch(), 0);
    }

    #[test]
    fn test_pacer_advance_starts_afresh_when_far_behind() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        let late = start + FRAME_PERIOD + MAX_LAG + Duration::from_millis(1);
        pacer.advance(late);
        assert_eq!(pacer.due(), late);
        assert_eq!(pacer.epoch(), 1);
        pacer.advance(late);
        assert_eq!(pacer.due(), late + FRAME_PERIOD);
        assert_eq!(pacer.epoch(), 1);
    }

    #[test]
    fn test_pacer_restart_is_due_at_once_in_a_new_epoch() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        pacer.advance(start);
        let resumed = start + Duration::from_secs(30);
        pacer.restart(resumed);
        assert_eq!(pacer.due(), resumed);
        assert_eq!(pacer.epoch(), 1);
    }
}
