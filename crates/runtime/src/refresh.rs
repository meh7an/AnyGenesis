//! DRAM refresh as the 68000 feels it (the M0.2 model, placed in M1.1): every
//! [`INTERVAL`] cycles it executes, refresh holds it for [`DELAY`] more. Cycles it does not
//! execute (device stalls, waiting in STOP) cause none.
//!
//! The model is uniform: real refresh lands on bus cycles, so an instruction without one
//! is not delayed. It fits BlastEm's traces of the Nemesis decompressor to 175 cycles over
//! 1.83M (M0.2), and M1.2 keeps it: budgets end where the executed cycles and the refresh
//! they cause first reach the event ([`Refresh::until`]).

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

    /// The fewest cycles to execute, after `done` executed so far, for `cycles` to pass
    /// together with the refresh they cause: a budget ending there has its first
    /// instruction boundary at or after the event `cycles` away.
    pub fn until(&self, done: u32, cycles: u32) -> u32 {
        let passed = |more: u32| more + self.pending(done + more) - self.pending(done);
        // Within a refresh or two of the answer, which the walks settle.
        let mut more =
            (u64::from(cycles) * u64::from(INTERVAL) / u64::from(INTERVAL + DELAY)) as u32;
        while more > 0 && passed(more - 1) >= cycles {
            more -= 1;
        }
        while passed(more) < cycles {
            more += 1;
        }
        more
    }
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
    fn test_until_is_the_first_point_reaching_the_cycles() {
        let mut refresh = Refresh::default();
        // 126 executed pass 126; the refresh at 128 makes 128 executed pass 130.
        assert_eq!(refresh.until(0, 126), 126);
        assert_eq!(refresh.until(0, 129), 128);
        assert_eq!(refresh.until(0, 130), 128);
        assert_eq!(refresh.until(0, 131), 129);
        assert_eq!(refresh.until(0, 0), 0);
        // With a carried remainder and cycles already executed this budget.
        // 100 carried and 20 done: the refresh falls 8 cycles on.
        refresh.charge(100);
        assert_eq!(refresh.until(20, 10), 8);
        for done in [0, 7, 127, 300] {
            for cycles in 1..2_000 {
                let more = refresh.until(done, cycles);
                let passed =
                    |more: u32| more + refresh.pending(done + more) - refresh.pending(done);
                assert!(passed(more) >= cycles);
                assert!(more == 0 || passed(more - 1) < cycles, "{done} {cycles}");
            }
        }
    }
}
