//! What either CPU sees of the chip: the address latch, the busy flag and the timers behind
//! the status register. The emulation keeps this half, so a status read sees the timers at
//! the master clock it is made; it is small and `Copy`, to be put back when the Z80 rolls
//! back.
//!
//! Sources: status bits 0, 1 and 7 (YM2608 manual 5-3); every port reads the status on the
//! discrete YM2612, unused bits 0 (MacDonald, Nemesis, Snake and l_oliveira on SpritesMind,
//! t=386 start=75 and p=33971); one address latch for both parts, holding the part of the
//! port it was written through, so either data port writes the latched part (Nemesis's
//! hardware test, t=386 start=405, p=7673). Busy is set by data writes only (Sauraen,
//! p=27138); its length is ymfm's 32 internal cycles, which Sauraen's die reading suggests
//! but testers measure as varying by register [U]. That a data write through the other
//! part's port sets busy too is inferred [U].

use crate::{
    MASTER_DIVIDER, SAMPLE_CLOCKS,
    registers::{Port, TIMER_CONTROL},
    timer::Timers,
};

/// Input clocks per internal cycle of the chip.
const PRESCALE: u64 = 6;
/// How long the chip stays busy after a data write, in master clocks: 32 internal cycles.
pub const BUSY_CLOCKS: u64 = 32 * PRESCALE * MASTER_DIVIDER;
/// Status bit of the busy flag.
pub const STATUS_BUSY: u8 = 0x80;
/// Register $27's load and enable bits of timers A and B.
const TIMER_RUN: u8 = 0x0F;

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    /// For a data write, the part (1 or 2) and the register it went to.
    pub data: Option<(u8, u8)>,
    /// It wrote timer control with a timer's load or enable bit set.
    pub timer: bool,
}

impl Written {
    const NONE: Self = Self {
        data: None,
        timer: false,
    };
}

/// The busy flag's end, the latched part and register, and the timers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    busy_until: u64,
    /// The part (1 or 2) and register the next data write goes to; part I register 0 at
    /// power-on, which nothing documents [U].
    latch: (u8, u8),
    timers: Timers,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            busy_until: 0,
            latch: (1, 0),
            timers: Timers::default(),
        }
    }
}

impl Control {
    /// The status register at master clock `now`, which every port reads.
    pub fn status(&self, now: u64) -> u8 {
        let mut status = self.timers.flags(now / SAMPLE_CLOCKS);
        if now < self.busy_until {
            status |= STATUS_BUSY;
        }
        status
    }

    /// A write to the port at `offset` in the chip's slot at master clock `now`.
    pub fn write(&mut self, offset: u32, value: u8, now: u64) -> Written {
        let (part, register) = match Port::of(offset) {
            Port::AddressOne => {
                self.latch = (1, value);
                return Written::NONE;
            }
            Port::AddressTwo => {
                self.latch = (2, value);
                return Written::NONE;
            }
            Port::DataOne | Port::DataTwo => self.latch,
        };
        self.busy_until = now + BUSY_CLOCKS;
        if part == 1 {
            self.timers.write(register, value, now / SAMPLE_CLOCKS);
        }
        Written {
            data: Some((part, register)),
            timer: part == 1 && register == TIMER_CONTROL && value & TIMER_RUN != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{registers::TIMER_A_HIGH, timer::STATUS_TIMER_A};

    #[test]
    fn test_busy_lasts_from_a_data_write_for_192_cpu_cycles() {
        let mut ym = Control::default();
        assert_eq!(BUSY_CLOCKS, 192 * MASTER_DIVIDER);
        assert_eq!(ym.status(0), 0);
        ym.write(0, 0x2B, 100);
        assert_eq!(ym.status(100), 0);
        ym.write(1, 0x80, 100);
        assert_eq!(ym.status(100), STATUS_BUSY);
        assert_eq!(ym.status(100 + BUSY_CLOCKS - 1), STATUS_BUSY);
        assert_eq!(ym.status(100 + BUSY_CLOCKS), 0);
    }

    #[test]
    fn test_part_two_data_writes_set_busy_and_address_writes_do_not() {
        let mut ym = Control::default();
        ym.write(2, 0x30, 0);
        assert_eq!(ym.status(0), 0);
        ym.write(3, 0x01, 0);
        assert_eq!(ym.status(0), STATUS_BUSY);
    }

    #[test]
    fn test_a_later_data_write_restarts_busy() {
        let mut ym = Control::default();
        ym.write(1, 0, 0);
        ym.write(1, 0, 50);
        assert_eq!(ym.status(BUSY_CLOCKS), STATUS_BUSY);
        assert_eq!(ym.status(50 + BUSY_CLOCKS), 0);
    }

    #[test]
    fn test_either_data_port_writes_the_part_the_address_was_latched_for() {
        let mut ym = Control::default();
        ym.write(0, 0x30, 0);
        assert_eq!(ym.write(3, 0x01, 0).data, Some((1, 0x30)));
        ym.write(2, 0xB4, 0);
        assert_eq!(ym.write(1, 0xC0, 0).data, Some((2, 0xB4)));
        assert_eq!(ym.write(1, 0x80, BUSY_CLOCKS).data, Some((2, 0xB4)));
        assert_eq!(ym.status(BUSY_CLOCKS), STATUS_BUSY);
    }

    #[test]
    fn test_timer_flags_show_at_the_tick_they_overflow() {
        let mut ym = Control::default();
        ym.write(0, TIMER_A_HIGH, 0);
        ym.write(1, 0xFF, 0);
        ym.write(0, TIMER_CONTROL, 0);
        ym.write(1, 0x05, SAMPLE_CLOCKS - 1);
        assert_eq!(ym.status(4 * SAMPLE_CLOCKS - 1), 0);
        assert_eq!(ym.status(4 * SAMPLE_CLOCKS), STATUS_TIMER_A);
    }

    #[test]
    fn test_timers_only_take_part_one_writes() {
        let mut ym = Control::default();
        ym.write(2, TIMER_CONTROL, 0);
        assert!(!ym.write(3, 0x0F, 0).timer);
        ym.write(0, 0x28, 0);
        assert!(!ym.write(1, 0x0F, 0).timer);
        ym.write(0, TIMER_CONTROL, 0);
        assert!(!ym.write(1, 0x70, 0).timer);
        assert!(ym.write(1, 0x05, 0).timer);
    }

    #[test]
    fn test_data_writes_name_their_part_and_register() {
        let mut ym = Control::default();
        assert_eq!(ym.write(0, 0x2A, 0), Written::NONE);
        assert_eq!(ym.write(1, 0x80, 0).data, Some((1, 0x2A)));
        ym.write(2, 0xB4, 0);
        assert_eq!(ym.write(3, 0xC0, 0).data, Some((2, 0xB4)));
    }
}
