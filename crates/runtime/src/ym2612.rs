//! The YM2612 as the 68000 sees it before there is any synthesis: the status register's busy
//! flag after data writes. Writes reach no register file and sound stays silent.
//!
//! Source: the busy duration, the status port and register $27's bits follow ymfm (pinned
//! SHA in PROGRESS.md, read as a reference, nothing copied).

use crate::timing::CPU_DIVIDER;

/// Master clocks per YM2612 input clock: the chip runs at the 68000's clock.
const INPUT_DIVIDER: u64 = CPU_DIVIDER;
/// Input clocks per internal cycle of the chip.
const PRESCALE: u64 = 6;
/// How long the chip stays busy after a data write, in master clocks: 32 internal cycles.
pub(crate) const BUSY_CLOCKS: u64 = 32 * PRESCALE * INPUT_DIVIDER;
const STATUS_BUSY: u8 = 0x80;
/// The timer control register, in part I.
const TIMER_CONTROL: u8 = 0x27;
/// Register $27's load and enable bits of timers A and B; the rest select the channel 3 mode
/// or reset flags that never get set while no timer runs.
const TIMER_RUN: u8 = 0x0F;

/// One of the four ports, from the low two bits of an address in the YM2612 slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Port {
    AddressOne,
    DataOne,
    AddressTwo,
    DataTwo,
}

impl Port {
    fn of(offset: u32) -> Self {
        match offset & 3 {
            0 => Self::AddressOne,
            1 => Self::DataOne,
            2 => Self::AddressTwo,
            _ => Self::DataTwo,
        }
    }
}

/// The busy flag's end and the part I address, the one place a timer can be turned on.
#[derive(Debug, Default)]
pub(crate) struct Ym2612 {
    busy_until: u64,
    address_one: u8,
}

impl Ym2612 {
    /// The status register at master clock `now`; only the first port answers, the others
    /// read 0. Timer flags stay clear because no timer runs.
    pub fn status(&self, offset: u32, now: u64) -> u8 {
        match Port::of(offset) {
            Port::AddressOne if now < self.busy_until => STATUS_BUSY,
            _ => 0,
        }
    }

    /// A write at master clock `now`. Returns whether it loads or enables a timer, which
    /// this model does not build.
    pub fn write(&mut self, offset: u32, value: u8, now: u64) -> bool {
        match Port::of(offset) {
            Port::AddressOne => {
                self.address_one = value;
                false
            }
            Port::AddressTwo => false,
            port => {
                self.busy_until = now + BUSY_CLOCKS;
                port == Port::DataOne && self.address_one == TIMER_CONTROL && value & TIMER_RUN != 0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_busy_lasts_from_a_data_write_for_192_cpu_cycles() {
        let mut ym = Ym2612::default();
        assert_eq!(BUSY_CLOCKS, 192 * CPU_DIVIDER);
        assert_eq!(ym.status(0, 0), 0);
        ym.write(0, 0x2B, 100);
        assert_eq!(ym.status(0, 100), 0);
        ym.write(1, 0x80, 100);
        assert_eq!(ym.status(0, 100), STATUS_BUSY);
        assert_eq!(ym.status(0, 100 + BUSY_CLOCKS - 1), STATUS_BUSY);
        assert_eq!(ym.status(0, 100 + BUSY_CLOCKS), 0);
    }

    #[test]
    fn test_part_two_data_writes_set_busy_and_address_writes_do_not() {
        let mut ym = Ym2612::default();
        ym.write(2, 0x30, 0);
        assert_eq!(ym.status(0, 0), 0);
        ym.write(3, 0x01, 0);
        assert_eq!(ym.status(0, 0), STATUS_BUSY);
    }

    #[test]
    fn test_a_later_data_write_restarts_busy() {
        let mut ym = Ym2612::default();
        ym.write(1, 0, 0);
        ym.write(1, 0, 50);
        assert_eq!(ym.status(0, BUSY_CLOCKS), STATUS_BUSY);
        assert_eq!(ym.status(0, 50 + BUSY_CLOCKS), 0);
    }

    #[test]
    fn test_status_answers_at_the_first_port_and_its_mirrors_only() {
        let mut ym = Ym2612::default();
        ym.write(1, 0, 0);
        assert_eq!(ym.status(4, 0), STATUS_BUSY);
        assert_eq!(ym.status(0x1FFC, 0), STATUS_BUSY);
        for offset in 1..4 {
            assert_eq!(ym.status(offset, 0), 0);
        }
    }

    #[test]
    fn test_only_timer_load_or_enable_bits_are_reported() {
        let mut ym = Ym2612::default();
        ym.write(0, TIMER_CONTROL, 0);
        assert!(!ym.write(1, 0x00, 0));
        assert!(!ym.write(1, 0x70, 0));
        assert!(ym.write(1, 0x05, 0));
        ym.write(2, TIMER_CONTROL, 0);
        assert!(!ym.write(3, 0x0F, 0));
        ym.write(0, 0x28, 0);
        assert!(!ym.write(1, 0x0F, 0));
    }
}
