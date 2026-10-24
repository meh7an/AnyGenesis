use crate::{Address, Size};

/// The 68000's view of memory and devices, shared by the IR interpreter and the runtime.
///
/// Accesses never fail. An unmapped address yields whatever open-bus value the implementor
/// chooses, and logging is the implementor's policy. Alignment is the caller's concern: the
/// 68000 raises an address error before an odd word or long access reaches the bus. Reads
/// take `&mut self` because device reads have side effects, such as the VDP status read
/// clearing its pending flags.
///
/// The trait is dyn-compatible, so the runtime can hand generated code a `&mut dyn Bus`.
pub trait Bus {
    /// Reads one byte.
    fn read_byte(&mut self, address: Address) -> u8;

    /// Reads one word; `address` is even.
    fn read_word(&mut self, address: Address) -> u16;

    /// Writes one byte.
    fn write_byte(&mut self, address: Address, value: u8);

    /// Writes one word; `address` is even.
    fn write_word(&mut self, address: Address, value: u16);

    /// Reads a long word as two word reads, high word first, as the 68000's 16-bit bus does.
    fn read_long(&mut self, address: Address) -> u32 {
        let high = self.read_word(address);
        let low = self.read_word(address.wrapping_add(2));
        (u32::from(high) << 16) | u32::from(low)
    }

    /// Writes a long word as two word writes, high word first.
    fn write_long(&mut self, address: Address, value: u32) {
        self.write_word(address, (value >> 16) as u16);
        self.write_word(address.wrapping_add(2), value as u16);
    }

    /// Reads an operand of `size`, zero-extended to 32 bits.
    fn read(&mut self, size: Size, address: Address) -> u32 {
        match size {
            Size::Byte => u32::from(self.read_byte(address)),
            Size::Word => u32::from(self.read_word(address)),
            Size::Long => self.read_long(address),
        }
    }

    /// Writes the low `size` bits of `value`.
    fn write(&mut self, size: Size, address: Address, value: u32) {
        match size {
            Size::Byte => self.write_byte(address, value as u8),
            Size::Word => self.write_word(address, value as u16),
            Size::Long => self.write_long(address, value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Access {
        ReadByte(u32),
        ReadWord(u32),
        WriteByte(u32, u8),
        WriteWord(u32, u16),
    }

    /// Records every access; word reads return the address's low 16 bits.
    #[derive(Debug, Default)]
    struct RecordingBus {
        log: Vec<Access>,
    }

    impl Bus for RecordingBus {
        fn read_byte(&mut self, address: Address) -> u8 {
            self.log.push(Access::ReadByte(address.get()));
            0xA5
        }

        fn read_word(&mut self, address: Address) -> u16 {
            self.log.push(Access::ReadWord(address.get()));
            address.get() as u16
        }

        fn write_byte(&mut self, address: Address, value: u8) {
            self.log.push(Access::WriteByte(address.get(), value));
        }

        fn write_word(&mut self, address: Address, value: u16) {
            self.log.push(Access::WriteWord(address.get(), value));
        }
    }

    #[test]
    fn test_bus_read_long_two_words_high_first() {
        let mut bus = RecordingBus::default();
        assert_eq!(bus.read_long(Address::new(0xFF_0010)), 0x0010_0012);
        let expected = [Access::ReadWord(0xFF_0010), Access::ReadWord(0xFF_0012)];
        assert_eq!(bus.log, expected);
    }

    #[test]
    fn test_bus_write_long_two_words_high_first() {
        let mut bus = RecordingBus::default();
        bus.write_long(Address::new(0xFF_0010), 0x1234_5678);
        let expected = [
            Access::WriteWord(0xFF_0010, 0x1234),
            Access::WriteWord(0xFF_0012, 0x5678),
        ];
        assert_eq!(bus.log, expected);
    }

    #[test]
    fn test_bus_read_long_at_top_wraps_second_word() {
        let mut bus = RecordingBus::default();
        bus.read_long(Address::new(0xFF_FFFE));
        let expected = [Access::ReadWord(0xFF_FFFE), Access::ReadWord(0x00_0000)];
        assert_eq!(bus.log, expected);
    }

    #[test]
    fn test_bus_write_sized_truncates_to_size() {
        let mut bus = RecordingBus::default();
        bus.write(Size::Byte, Address::new(1), 0x1234_5678);
        bus.write(Size::Word, Address::new(2), 0x1234_5678);
        let expected = [Access::WriteByte(1, 0x78), Access::WriteWord(2, 0x5678)];
        assert_eq!(bus.log, expected);
    }

    #[test]
    fn test_bus_read_sized_through_dyn_dispatches_by_size() {
        let mut recording = RecordingBus::default();
        let bus: &mut dyn Bus = &mut recording;
        assert_eq!(bus.read(Size::Byte, Address::new(3)), 0xA5);
        assert_eq!(bus.read(Size::Word, Address::new(4)), 0x0004);
        assert_eq!(recording.log, [Access::ReadByte(3), Access::ReadWord(4)]);
    }
}
