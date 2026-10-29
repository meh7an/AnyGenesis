//! The YM2612 as either CPU sees it before there is any synthesis: the status register's
//! busy flag after data writes. Writes reach no register file and sound stays silent.
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

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Written {
    /// For a data write, the part (1 or 2) and the register it went to.
    pub data: Option<(u8, u8)>,
    /// It loads or enables a timer, which this model does not build.
    pub timer: bool,
}

impl Written {
    const NONE: Self = Self {
        data: None,
        timer: false,
    };
}

/// The busy flag's end and the address each part's data writes go to.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Ym2612 {
    busy_until: u64,
    address_one: u8,
    address_two: u8,
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

    /// A write at master clock `now`.
    pub fn write(&mut self, offset: u32, value: u8, now: u64) -> Written {
        let (part, register) = match Port::of(offset) {
            Port::AddressOne => {
                self.address_one = value;
                return Written::NONE;
            }
            Port::AddressTwo => {
                self.address_two = value;
                return Written::NONE;
            }
            Port::DataOne => (1, self.address_one),
            Port::DataTwo => (2, self.address_two),
        };
        self.busy_until = now + BUSY_CLOCKS;
        Written {
            data: Some((part, register)),
            timer: part == 1 && register == TIMER_CONTROL && value & TIMER_RUN != 0,
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
        assert!(!ym.write(1, 0x00, 0).timer);
        assert!(!ym.write(1, 0x70, 0).timer);
        assert!(ym.write(1, 0x05, 0).timer);
        ym.write(2, TIMER_CONTROL, 0);
        assert!(!ym.write(3, 0x0F, 0).timer);
        ym.write(0, 0x28, 0);
        assert!(!ym.write(1, 0x0F, 0).timer);
    }

    #[test]
    fn test_data_writes_name_their_part_and_register() {
        let mut ym = Ym2612::default();
        assert_eq!(ym.write(0, 0x2A, 0), Written::NONE);
        ym.write(2, 0xB4, 0);
        assert_eq!(ym.write(1, 0x80, 0).data, Some((1, 0x2A)));
        assert_eq!(ym.write(3, 0xC0, 0).data, Some((2, 0xB4)));
    }
}
