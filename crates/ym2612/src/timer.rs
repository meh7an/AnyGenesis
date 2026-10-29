//! Timers A and B and their status flags, counted in output periods ("ticks", 144 input
//! clocks each) from power-on.
//!
//! A loaded timer overflows every period: A after 1024 - NA ticks, B after 16 x (256 - NB)
//! ticks (Nemesis on SpritesMind, t=386 start=75, p=5680; Plutiedev). At each overflow it
//! reloads from the registers as they are then; it restarts only when its load bit goes
//! from 0 to 1, never on a rewrite of the load bit, the enable or reset bits, or the
//! values (Nemesis's hardware test, p=5687). An overflow sets the flag only while the
//! enable bit is on; a reset bit clears its flag and is not kept (YM2608 manual 2-8-3).
//! Settled by ymfm, undocumented [U]: the first overflow comes a whole period after the
//! output period the load falls in, timer B counts on a divide-by-16 running free from
//! power-on (so its first period ends on a multiple of 16 ticks), and a timer counts on
//! with its enable bit off. The chip's interrupt line is not connected on the Genesis
//! (Nemesis, p=5592; md.railgun.works), so the flags are only seen by polling.
//!
//! The state is closed form: nothing steps between writes, and the flags at a tick are
//! worked out when asked.

use crate::registers::{TIMER_A_HIGH, TIMER_A_LOW, TIMER_B, TIMER_CONTROL};

/// Status bit of timer A's overflow flag.
pub const STATUS_TIMER_A: u8 = 0x01;
/// Status bit of timer B's overflow flag.
pub const STATUS_TIMER_B: u8 = 0x02;

/// Ticks per count of timer B.
const B_DIVIDER: u64 = 16;

/// Register $27's bits for each timer: load, enable the flag, reset the flag.
#[derive(Debug, Clone, Copy)]
struct Bits {
    load: u8,
    enable: u8,
    reset: u8,
    status: u8,
}

const A: Bits = Bits {
    load: 0x01,
    enable: 0x04,
    reset: 0x10,
    status: STATUS_TIMER_A,
};

const B: Bits = Bits {
    load: 0x02,
    enable: 0x08,
    reset: 0x20,
    status: STATUS_TIMER_B,
};

/// Both timers, their registers and their flags.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timers {
    /// Timer A's 10-bit value NA.
    value_a: u16,
    /// Timer B's value NB.
    value_b: u8,
    /// Register $27 as last written.
    control: u8,
    /// The flags as of the last write.
    flags: u8,
    /// The tick of each loaded timer's next overflow.
    next_a: Option<u64>,
    next_b: Option<u64>,
}

impl Timers {
    /// The overflow flags at `tick`, none of them before the last write.
    pub fn flags(&self, tick: u64) -> u8 {
        let mut flags = self.flags;
        for (next, bits) in [(self.next_a, A), (self.next_b, B)] {
            if next.is_some_and(|next| next <= tick) && self.control & bits.enable != 0 {
                flags |= bits.status;
            }
        }
        flags
    }

    /// Writes a timer register of part I at `tick`; other registers are ignored.
    pub fn write(&mut self, register: u8, value: u8, tick: u64) {
        self.advance(tick);
        match register {
            TIMER_A_HIGH => self.value_a = u16::from(value) << 2 | self.value_a & 0x03,
            TIMER_A_LOW => self.value_a = self.value_a & !0x03 | u16::from(value & 0x03),
            TIMER_B => self.value_b = value,
            TIMER_CONTROL => self.write_control(value, tick),
            _ => {}
        }
    }

    /// Whether timer A overflowed after the last call and by `tick`, taking the overflows up
    /// to `tick` into the flags.
    pub fn take_overflow_a(&mut self, tick: u64) -> bool {
        let overflowed = self.next_a.is_some_and(|next| next <= tick);
        self.advance(tick);
        overflowed
    }

    fn write_control(&mut self, value: u8, tick: u64) {
        for bits in [A, B] {
            if value & bits.reset != 0 {
                self.flags &= !bits.status;
            }
        }
        self.control = value;
        self.next_a = match self.next_a {
            _ if value & A.load == 0 => None,
            None => Some(tick + self.period_a()),
            running => running,
        };
        self.next_b = match self.next_b {
            _ if value & B.load == 0 => None,
            None => Some(tick - tick % B_DIVIDER + self.period_b()),
            running => running,
        };
    }

    /// Takes every overflow up to `tick` into the flags and moves each timer to its next.
    fn advance(&mut self, tick: u64) {
        self.flags = self.flags(tick);
        let (period_a, period_b) = (self.period_a(), self.period_b());
        for (next, period) in [(&mut self.next_a, period_a), (&mut self.next_b, period_b)] {
            if let Some(at) = next.filter(|&at| at <= tick) {
                *next = Some(at + ((tick - at) / period + 1) * period);
            }
        }
    }

    fn period_a(&self) -> u64 {
        1024 - u64::from(self.value_a)
    }

    fn period_b(&self) -> u64 {
        B_DIVIDER * (256 - u64::from(self.value_b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timers(writes: &[(u8, u8)]) -> Timers {
        let mut timers = Timers::default();
        for &(register, value) in writes {
            timers.write(register, value, 0);
        }
        timers
    }

    #[test]
    fn test_timer_a_flags_after_1024_minus_na_ticks() {
        let timers = timers(&[
            (TIMER_A_HIGH, 0xFF),
            (TIMER_A_LOW, 0x02),
            (TIMER_CONTROL, 0x05),
        ]);
        assert_eq!(timers.flags(1), 0);
        assert_eq!(timers.flags(2), STATUS_TIMER_A);
    }

    #[test]
    fn test_timer_b_counts_on_the_free_running_divider() {
        let mut timers = timers(&[(TIMER_B, 0xFF)]);
        timers.write(TIMER_CONTROL, 0x0A, 5);
        assert_eq!(timers.flags(15), 0);
        assert_eq!(timers.flags(16), STATUS_TIMER_B);
    }

    #[test]
    fn test_overflows_without_enable_set_no_flag_but_keep_counting() {
        let mut timers = timers(&[(TIMER_A_HIGH, 0xFF), (TIMER_CONTROL, 0x01)]);
        assert_eq!(timers.flags(100), 0);
        timers.write(TIMER_CONTROL, 0x05, 9);
        assert_eq!(timers.flags(11), 0);
        assert_eq!(timers.flags(12), STATUS_TIMER_A);
    }

    #[test]
    fn test_a_new_value_takes_effect_at_the_next_reload() {
        let mut timers = timers(&[(TIMER_A_HIGH, 0xFF), (TIMER_CONTROL, 0x05)]);
        timers.write(TIMER_A_HIGH, 0xFE, 1);
        timers.write(TIMER_CONTROL, 0x15, 4);
        assert_eq!(timers.flags(11), 0);
        assert_eq!(timers.flags(12), STATUS_TIMER_A);
    }

    #[test]
    fn test_rewriting_load_keeps_the_count_and_clearing_it_stops_the_timer() {
        let mut timers = timers(&[(TIMER_A_HIGH, 0xFF), (TIMER_CONTROL, 0x05)]);
        timers.write(TIMER_CONTROL, 0x05, 3);
        assert_eq!(timers.flags(4), STATUS_TIMER_A);
        timers.write(TIMER_CONTROL, 0x14, 4);
        assert_eq!(timers.flags(1000), 0);
    }

    #[test]
    fn test_reset_bits_clear_only_their_own_flag() {
        let mut timers = timers(&[(TIMER_A_HIGH, 0xFF), (TIMER_B, 0xFF), (TIMER_CONTROL, 0x0F)]);
        timers.write(TIMER_CONTROL, 0x2F, 16);
        assert_eq!(timers.flags(16), STATUS_TIMER_A);
    }
}
