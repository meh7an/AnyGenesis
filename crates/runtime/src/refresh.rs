//! DRAM refresh as the 68000 feels it (the M0.2 model, placed in M1.1): every
//! [`INTERVAL`] cycles it executes, refresh holds it for [`DELAY`] more. Cycles it does not
//! execute (device stalls, waiting in STOP) cause none.
//!
//! The model is uniform: real refresh lands on bus cycles, so an instruction without one
//! is not delayed. It fits BlastEm's traces of the Nemesis decompressor to 175 cycles over
//! 1.83M (M0.2). M1.2's master-clock scheduler takes it over.

/// Executed cycles per refresh.
pub const INTERVAL: u32 = 128;

/// Cycles each refresh holds the 68000.
pub const DELAY: u32 = 2;

/// Executed cycles carried towards the next refresh.
#[derive(Debug, Default)]
pub(crate) struct Refresh {
    count: u32,
}

impl Refresh {
    /// The refresh cycles `executed` more cycles cause, carrying the remainder.
    pub fn charge(&mut self, executed: u32) -> u32 {
        let total = self.count + executed;
        self.count = total % INTERVAL;
        total / INTERVAL * DELAY
    }

    /// The refresh cycles [`Refresh::charge`] would return for `executed`, without charging.
    pub fn pending(&self, executed: u32) -> u32 {
        (self.count + executed) / INTERVAL * DELAY
    }
}

/// A budget of `cycles` shrunk so that executing it, with the refresh it causes, ends near
/// where `cycles` would; at least 1.
pub(crate) fn shrink(cycles: i32) -> i32 {
    let interval = i64::from(INTERVAL);
    let shrunk = i64::from(cycles) * interval / (interval + i64::from(DELAY));
    // At most `cycles`, so it fits.
    shrunk.max(1) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_refresh_carries_the_remainder() {
        let mut refresh = Refresh::default();
        assert_eq!(refresh.charge(100), 0);
        assert_eq!(refresh.pending(28), 2);
        assert_eq!(refresh.charge(28), 2);
        assert_eq!(refresh.charge(256), 4);
        assert_eq!(refresh.charge(127), 0);
    }

    #[test]
    fn test_shrunk_budget_with_refresh_ends_near_the_budget() {
        assert_eq!(shrink(488), 480);
        assert_eq!(shrink(0), 1);
        let mut refresh = Refresh::default();
        let budget = shrink(488);
        let used = budget as u32 + refresh.charge(budget as u32);
        assert!(used.abs_diff(488) <= DELAY, "{used}");
    }
}
