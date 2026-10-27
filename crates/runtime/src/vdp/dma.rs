//! DMA: 68K-to-VDP transfers, VRAM fill and VRAM copy, with the time each takes.
//!
//! A transfer is applied at once when its second command word arrives and freezes the
//! 68000 for as long as it would take; fill and copy are applied at once too, but leave the
//! 68000 running and the status register's DMA bit set until their end. Afterwards the
//! length registers read 0 and the source registers hold the address after the last word
//! (fill and copy too), inside the same 128 KiB block: register 23 never changes.
//!
//! Durations come from the access slots a line offers the 68000 side, by mode and by
//! whether the line is drawn: H40 18 drawn, 205 blanked; H32 16 and 167 (Eke on the
//! SpritesMind forum, matching Sega's software manual as quoted there). A slot moves a VRAM
//! byte or a CRAM/VSRAM word (Kabuto's DMA costs: 2.4 68000 cycles per word to CRAM and
//! VSRAM, 4.7 to VRAM). A copy needs two slots per byte (a read, then a write). Chosen: the
//! slots of a line are spread evenly over it, a line counts as blanked by its number and
//! the display bit when the DMA starts, and a fill's first line is not one byte short.
//!
//! A fill writes its data word as a normal write first; then, for each of its length steps,
//! the data's high byte goes to the address with bit 0 flipped, after the address moved on
//! by the increment. Sonic 1's fill macro sets the length to the byte count less one,
//! which this layout covers exactly.

use super::{Direction, Target, Vdp};
use crate::{
    hardware::Memory,
    log::AccessKind,
    timing::{HMode, LINE_CLOCKS, LINES, Timing},
};

/// Access slots a line offers: drawn, blanked.
fn slots_per_line(mode: HMode) -> (u64, u64) {
    match mode {
        HMode::H32 => (16, 167),
        HMode::H40 => (18, 205),
    }
}

/// The master clock where `slots` access slots from master clock `from` on have been used.
/// The one place the slot layout lives: real per-line slot positions would replace it.
pub(super) fn end(timing: &Timing, from: u64, slots: u64) -> u64 {
    let (drawn, blanked) = slots_per_line(timing.mode());
    let mut clock = from;
    let mut left = slots;
    loop {
        let position = clock % LINE_CLOCKS;
        let start = clock - position;
        let rate = if timing.line_blanked(start / LINE_CLOCKS % LINES) {
            blanked
        } else {
            drawn
        };
        // Slot k of the line starts at ceil(k * LINE_CLOCKS / rate): the first one starting at
        // or after the position, and how many the line still has. A run of slots ends where
        // the next one starts, so the next run takes that one.
        let first = if position == 0 {
            0
        } else {
            (position - 1) * rate / LINE_CLOCKS + 1
        };
        let free = rate - first;
        if left <= free {
            return start + ((first + left) * LINE_CLOCKS).div_ceil(rate);
        }
        left -= free;
        clock = start + LINE_CLOCKS;
    }
}

impl Vdp {
    /// Registers 19-20: words for a transfer, bytes for a fill or copy; 0 means 0x10000.
    fn dma_length(&self) -> u32 {
        match u32::from(u16::from_le_bytes([self.registers[19], self.registers[20]])) {
            0 => 0x1_0000,
            length => length,
        }
    }

    /// Registers 21-22: a transfer's source in words, a copy's in bytes.
    fn dma_source(&self) -> u16 {
        u16::from_le_bytes([self.registers[21], self.registers[22]])
    }

    /// Leaves the registers as hardware does after a DMA of the current length that read
    /// up to `source`.
    fn finish_dma(&mut self, source: u16) {
        [self.registers[21], self.registers[22]] = source.to_le_bytes();
        self.registers[19] = 0;
        self.registers[20] = 0;
        self.code &= !0x20;
    }

    /// The second command word asked for a DMA: register 23 bits 7-6 say which.
    pub(super) fn start_dma(&mut self, memory: Memory<'_>) {
        match self.registers[23] >> 6 {
            0 | 1 => self.transfer(memory),
            2 => self.fill = true,
            _ => self.copy(),
        }
    }

    /// 68K-to-VDP: words from ROM or work RAM into the command's target.
    fn transfer(&mut self, memory: Memory<'_>) {
        let Some(Direction {
            target,
            write: true,
        }) = Direction::of(self.code)
        else {
            self.fault(AccessKind::VdpDma);
            self.code &= !0x20;
            return;
        };
        let length = self.dma_length();
        // DMD0 is source bit 23; the word address wraps inside its 128 KiB block.
        let block = u32::from(self.registers[23] & 0x7F) << 17;
        let mut source = self.dma_source();
        let mut outside = false;
        for _ in 0..length {
            let address = block | u32::from(source) << 1;
            let word = memory.read_word(address).unwrap_or_else(|| {
                outside = true;
                0
            });
            self.write_target(target, word);
            source = source.wrapping_add(1);
        }
        self.finish_dma(source);
        if outside {
            self.fault(AccessKind::VdpDma);
        }
        let slots = match target {
            Target::Vram => 2 * u64::from(length),
            Target::Cram | Target::Vsram => length.into(),
        };
        // The transfer starts once the writes waiting in the FIFO have gone out.
        let end = end(&self.timing, self.drained(), slots);
        self.stall_until(end);
        self.counts.transfers += 1;
    }

    /// The data write a fill waited for has been made; the rest of the fill follows.
    pub(super) fn run_fill(&mut self, target: Target, value: u16) {
        self.fill = false;
        let length = self.dma_length();
        if target == Target::Vram {
            let [high, _] = value.to_be_bytes();
            for _ in 0..length {
                self.vram[usize::from(self.address ^ 1)] = high;
                self.address = self.address.wrapping_add(self.increment());
            }
        } else {
            // Not documented: taken to repeat the data write.
            self.fault(AccessKind::VdpDma);
            for _ in 0..length {
                self.write_target(target, value);
            }
        }
        // A length of 0x10000 brings the source round to where it started.
        let source = self.dma_source().wrapping_add(length as u16);
        self.finish_dma(source);
        // The data write went through the FIFO; the fill follows it.
        self.busy_until = end(&self.timing, self.drained(), length.into());
        self.counts.fills += 1;
    }

    /// VRAM to VRAM, byte by byte, from the source registers to the command's address.
    fn copy(&mut self) {
        let length = self.dma_length();
        let mut source = self.dma_source();
        for _ in 0..length {
            self.vram[usize::from(self.address)] = self.vram[usize::from(source)];
            source = source.wrapping_add(1);
            self.address = self.address.wrapping_add(self.increment());
        }
        self.finish_dma(source);
        self.busy_until = end(&self.timing, self.drained(), 2 * u64::from(length));
        self.counts.copies += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        hardware::{Hardware, port},
        vdp::{
            status,
            tests::{Sources, command, read, vdp, write, write_with},
        },
    };

    /// Register words for a DMA of `length` from `source` (a word address for transfers, a
    /// byte address for copies) of type `mode` (register 23 bits 7-6, or the source's top
    /// bits for a transfer).
    fn dma_registers(length: u16, source: u32, mode: u8) -> [u16; 6] {
        let [low, mid, ..] = source.to_le_bytes();
        let [length_low, length_high] = length.to_le_bytes();
        [
            0x8114,
            0x9300 | u16::from(length_low),
            0x9400 | u16::from(length_high),
            0x9500 | u16::from(low),
            0x9600 | u16::from(mid),
            0x9700 | u16::from(mode),
        ]
    }

    #[test]
    fn test_transfer_copies_rom_words_and_stalls_for_the_slots_used() {
        let mut vdp = vdp();
        let sources = Sources::new();
        // Four words from ROM 0x10: word address 8.
        write(&mut vdp, port::CONTROL, &dma_registers(4, 8, 0));
        let fault = write_with(&mut vdp, &sources, port::CONTROL, &command(0x21, 0x0100));
        assert_eq!(fault, None);
        assert_eq!(vdp.vram()[0x100..0x108], sources.rom[0x10..0x18]);
        assert_eq!(vdp.registers()[19..23], [0, 0, 12, 0]);
        // Display off: 167 slots a line in H32, two per VRAM word.
        let clocks = (8 * LINE_CLOCKS).div_ceil(167);
        assert_eq!(vdp.take_stall(), clocks.div_ceil(7) as u32);
        assert_eq!(vdp.take_stall(), 0);
        assert_eq!(vdp.counts().transfers, 1);
        // The command goes on as a plain write after the transfer.
        write(&mut vdp, port::DATA, &[0x1234]);
        assert_eq!(vdp.vram()[0x108..0x10A], [0x12, 0x34]);
        assert_eq!(vdp.counts().transfers, 1);
    }

    #[test]
    fn test_transfer_source_wraps_inside_its_128k_block() {
        let mut vdp = vdp();
        let mut sources = Sources::new();
        sources.ram[0xFFFE..].copy_from_slice(&[0x0A, 0xAA]);
        sources.ram[..2].copy_from_slice(&[0x0C, 0xCC]);
        // $FFFFFE >> 1: word 0xFFFF in the block 0x7F.
        write(&mut vdp, port::CONTROL, &dma_registers(2, 0xFFFF, 0x7F));
        write_with(&mut vdp, &sources, port::CONTROL, &command(0x23, 0));
        assert_eq!(vdp.cram()[..2], [0x0AAA, 0x0CCC]);
        assert_eq!(vdp.registers()[21..24], [0x01, 0x00, 0x7F]);
    }

    #[test]
    fn test_transfer_outside_rom_and_ram_or_through_a_read_code_is_a_fault() {
        let mut vdp = vdp();
        // 0xA00000, the Z80 window.
        write(&mut vdp, port::CONTROL, &dma_registers(1, 0, 0x50));
        assert_eq!(
            write(&mut vdp, port::CONTROL, &command(0x21, 0)),
            Some(AccessKind::VdpDma)
        );
        write(&mut vdp, port::CONTROL, &dma_registers(1, 8, 0));
        assert_eq!(
            write(&mut vdp, port::CONTROL, &command(0x20, 0x0200)),
            Some(AccessKind::VdpDma)
        );
        assert_eq!(vdp.vram()[0x200..0x202], [0, 0]);
        assert_eq!(vdp.counts().transfers, 1);
    }

    #[test]
    fn test_dma_disabled_leaves_cd5_commands_as_plain_ones() {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &dma_registers(4, 8, 0));
        write(&mut vdp, port::CONTROL, &[0x8104]);
        write(&mut vdp, port::CONTROL, &command(0x21, 0x0100));
        assert_eq!(vdp.take_stall(), 0);
        write(&mut vdp, port::DATA, &[0xBEEF]);
        assert_eq!(vdp.vram()[0x100..0x102], [0xBE, 0xEF]);
        assert_eq!(vdp.registers()[19], 4);
    }

    #[test]
    fn test_fill_writes_the_data_then_its_high_byte_and_reads_busy() {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &dma_registers(7, 0, 0x80));
        write(&mut vdp, port::CONTROL, &[0x8F01]);
        write(&mut vdp, port::CONTROL, &command(0x21, 0x0200));
        assert_eq!(vdp.counts().fills, 0);
        assert_eq!(write(&mut vdp, port::DATA, &[0x5AFF]), None);
        // The data write fills 0x200-0x201; then the high byte goes to each next address
        // with bit 0 flipped, which leaves 0x201 with the low byte.
        assert_eq!(
            vdp.vram()[0x200..0x209],
            [0x5A, 0xFF, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x00]
        );
        assert_eq!(vdp.registers()[19..22], [0, 0, 7]);
        assert_eq!(vdp.take_stall(), 0);
        // The data write's two slots in the FIFO, then the fill's seven.
        let end = (9 * LINE_CLOCKS).div_ceil(167);
        assert_ne!(read(&mut vdp, port::CONTROL) & status::DMA_BUSY, 0);
        vdp.advance((end / 7) as u32);
        assert_ne!(vdp.status() & status::DMA_BUSY, 0);
        vdp.advance(1);
        assert_eq!(vdp.status() & status::DMA_BUSY, 0);
        // The fill is over: the next data write is a plain one.
        write(&mut vdp, port::DATA, &[0x0102]);
        assert_eq!(vdp.vram()[0x208..0x20A], [0x01, 0x02]);
        assert_eq!(vdp.counts().fills, 1);
    }

    #[test]
    fn test_fill_of_cram_repeats_the_write_and_is_a_fault() {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &dma_registers(2, 0, 0x80));
        write(&mut vdp, port::CONTROL, &command(0x23, 0));
        assert_eq!(
            write(&mut vdp, port::DATA, &[0x0EEE]),
            Some(AccessKind::VdpDma)
        );
        assert_eq!(vdp.cram()[..4], [0x0EEE, 0x0EEE, 0x0EEE, 0]);
    }

    #[test]
    fn test_copy_moves_vram_bytes_and_reads_busy() {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &command(0x01, 0x0300));
        write(&mut vdp, port::DATA, &[0x0102, 0x0304]);
        write(&mut vdp, port::CONTROL, &dma_registers(4, 0x0300, 0xC0));
        write(&mut vdp, port::CONTROL, &[0x8F01]);
        assert_eq!(write(&mut vdp, port::CONTROL, &command(0x30, 0x0401)), None);
        assert_eq!(vdp.vram()[0x400..0x406], [0, 1, 2, 3, 4, 0]);
        assert_eq!(vdp.registers()[19..23], [0, 0, 0x04, 0x03]);
        assert_ne!(vdp.status() & status::DMA_BUSY, 0);
        assert_eq!(vdp.counts().copies, 1);
    }

    #[test]
    fn test_end_spreads_slots_over_drawn_and_blanked_lines() {
        let mut timing = Timing::default();
        timing.set_register(12, 0x81);
        timing.set_register(1, 0x44);
        // 18 slots a drawn H40 line: 36 take two whole lines.
        assert_eq!(end(&timing, 0, 36), 2 * LINE_CLOCKS);
        timing.set_register(1, 0x04);
        assert_eq!(end(&timing, 0, 36), (36 * LINE_CLOCKS).div_ceil(205));
        assert_eq!(end(&timing, 0, 0), 0);
        // From inside a line, the slots left in it come first.
        timing.set_register(1, 0x44);
        let half = LINE_CLOCKS / 2;
        assert_eq!(end(&timing, half, 9), LINE_CLOCKS);
        assert_eq!(
            end(&timing, half, 10),
            LINE_CLOCKS + LINE_CLOCKS.div_ceil(18)
        );
    }
}
