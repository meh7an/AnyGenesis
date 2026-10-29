//! DMA: 68K-to-VDP transfers, VRAM fill and VRAM copy, with the time each takes.
//!
//! A transfer is applied at once when its second command word arrives and freezes the
//! 68000 for as long as it would take; fill and copy are applied at once too, but leave the
//! 68000 running and the status register's DMA bit set until their end. Afterwards the
//! length registers read 0 and the source registers hold the address after the last word
//! (fill and copy too), inside the same 128 KiB block: register 23 never changes.
//!
//! Durations come from the access slots a line offers the 68000 side ([`end`]). A slot
//! moves a VRAM byte or a CRAM/VSRAM word (Kabuto's DMA costs: 2.4 68000 cycles per word
//! to CRAM and VSRAM, 4.7 to VRAM). A copy needs two slots per byte (a read, then a write).
//! Chosen: a fill's first line is not one byte short.
//!
//! The slots: the VDP makes one memory access every two pixels, 210 a line in H40 and 171
//! in H32, in the order Kabuto's hardware notes give (Mask of Destiny's H32 capture on
//! SpritesMind agrees but for one sprite slot near the end). A drawn line leaves 18 (H40)
//! or 16 (H32) of them to the 68000 side, with 5 or 4 refresh slots among the rest; a
//! blanked line (vertical blanking or the display off) leaves it every slot but refresh,
//! 6 or 5 of them, 204 and 166 (Mask of Destiny's measurement; Sega's manual says 205 and
//! 167). Chosen, unverified: where the pattern sits on the H counter (no source says; the
//! H-scroll read, slot 0, is put at pixel counter 0x1E7 in H40 and 0x1E9 in H32, which
//! fits the largest gap measured, 26 accesses across a line's end, and the slots Kabuto saw
//! on screen), the H40 blanked
//! refresh slots (Mask of Destiny's positions, read as counting from the first sprite
//! slot), the H32 blanked ones (the drawn ones and one more 32 slots on), and the slots of
//! a line following its own line number's drawn or blanked state.
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

/// One access slot of the 68000 side: where it starts and ends, in master clocks from the
/// start of its line (where the H counter reads 0). The last one of an H32 line ends a
/// little into the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    start: u64,
    end: u64,
}

/// The VDP's memory accesses a line, of every kind.
const fn accesses(mode: HMode) -> u64 {
    match mode {
        HMode::H32 => 171,
        HMode::H40 => 210,
    }
}

/// The 32-access blocks of plane and sprite fetches a drawn line has.
const fn blocks(mode: HMode) -> u64 {
    match mode {
        HMode::H32 => 4,
        HMode::H40 => 5,
    }
}

/// The access (in Kabuto's order, 0 the H-scroll read) that starts at pixel 1 of the line,
/// the first after the H counter reads 0: access n starts at pixel 2n - 23 (H32) or
/// 2n - 25 (H40), counted round the line.
const fn first_access(mode: HMode) -> u64 {
    match mode {
        HMode::H32 => 12,
        HMode::H40 => 13,
    }
}

/// Whether access `n` of a drawn line is the 68000 side's. After the H-scroll read, four
/// sprite reads and a group of eight come the blocks, each of four groups of eight whose
/// second access is the 68000's in the first three and refresh in the fourth; then two of
/// the 68000's, and the sprite reads with one more of its slots in them (two in H32).
const fn drawn_external(mode: HMode, n: u64) -> bool {
    let tail = 13 + 32 * blocks(mode);
    if n >= 13 && n < tail {
        return (n - 13) % 8 == 1 && (n - 13) % 32 != 25;
    }
    match mode {
        HMode::H32 => n == tail || n == tail + 1 || n == tail + 15 || n == tail + 29,
        HMode::H40 => n == tail || n == tail + 1 || n == tail + 25,
    }
}

/// Whether access `n` of a blanked line is refresh, the only kind left there.
const fn blanked_refresh(mode: HMode, n: u64) -> bool {
    match mode {
        HMode::H32 => matches!(n, 38 | 70 | 102 | 134 | 166),
        HMode::H40 => matches!(n, 38 | 70 | 103 | 134 | 166 | 198),
    }
}

/// The 68000 side's slots of a line in `mode`, drawn or blanked, in line order.
const fn layout<const N: usize>(mode: HMode, blanked: bool) -> [Slot; N] {
    let mut slots = [Slot { start: 0, end: 0 }; N];
    let total = accesses(mode);
    let mut count = 0;
    let mut k = 0;
    while k < total {
        let n = (k + first_access(mode)) % total;
        let external = if blanked {
            !blanked_refresh(mode, n)
        } else {
            drawn_external(mode, n)
        };
        if external {
            slots[count] = Slot {
                start: mode.pixel_clock(1 + 2 * k),
                end: mode.pixel_clock(3 + 2 * k),
            };
            count += 1;
        }
        k += 1;
    }
    assert!(count == N, "the slot count of the layout");
    slots
}

const H32_DRAWN: [Slot; 16] = layout(HMode::H32, false);
const H32_BLANKED: [Slot; 166] = layout(HMode::H32, true);
const H40_DRAWN: [Slot; 18] = layout(HMode::H40, false);
const H40_BLANKED: [Slot; 204] = layout(HMode::H40, true);

fn slots_of(mode: HMode, blanked: bool) -> &'static [Slot] {
    match (mode, blanked) {
        (HMode::H32, false) => &H32_DRAWN,
        (HMode::H32, true) => &H32_BLANKED,
        (HMode::H40, false) => &H40_DRAWN,
        (HMode::H40, true) => &H40_BLANKED,
    }
}

/// The master clock where the last of `slots` access slots from master clock `from` on
/// ends: the slots that start at or after `from`, in the current mode, on drawn or blanked
/// lines as their line numbers and the display bit say now. The one place the slot layout
/// is used.
pub(super) fn end(timing: &Timing, from: u64, slots: u64) -> u64 {
    let mut line = from - from % LINE_CLOCKS;
    let mut position = from % LINE_CLOCKS;
    let mut left = slots;
    while left > 0 {
        let layout = slots_of(
            timing.mode(),
            timing.line_blanked(line / LINE_CLOCKS % LINES),
        );
        let first = layout.partition_point(|slot| slot.start < position);
        // A layout holds at most a few hundred slots.
        let free = (layout.len() - first) as u64;
        if left <= free {
            return line + layout[first + left as usize - 1].end;
        }
        left -= free;
        line += LINE_CLOCKS;
        position = 0;
    }
    from
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

    /// Where `slots` slots from master clock 0 end on a blanked H32 line, before its first
    /// refresh: one every two pixels of 10 master clocks, from pixel 1.
    fn blanked_h32_end(slots: u64) -> u64 {
        10 * (2 * slots + 1)
    }

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
        // Display off: two slots per VRAM word, the eighth ending at pixel 17.
        let clocks = blanked_h32_end(8);
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
        let end = blanked_h32_end(9);
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
    fn test_end_uses_the_slots_of_drawn_and_blanked_lines() {
        let mut timing = Timing::default();
        timing.set_register(12, 0x81);
        timing.set_register(1, 0x44);
        // A drawn H40 line: the first slot is the fourth access after the counter's 0,
        // pixels 3-4; the 18th is in the sprite reads, pixels 371-372, in the slow stretch.
        assert_eq!(end(&timing, 0, 1), 5 * 8);
        assert_eq!(end(&timing, 0, 18), 373 * 8 + 29 * 2);
        assert_eq!(end(&timing, 0, 36), LINE_CLOCKS + 373 * 8 + 29 * 2);
        // A slot that started before `from` is gone.
        assert_eq!(end(&timing, 3 * 8 + 1, 1), 21 * 8);
        // Blanked: every access but refresh, the first from pixel 1; 204 fill the line.
        timing.set_register(1, 0x04);
        assert_eq!(end(&timing, 0, 1), 3 * 8);
        assert_eq!(end(&timing, 0, 204), 421 * 8 + 30 * 2);
        assert_eq!(end(&timing, 0, 205), LINE_CLOCKS + 3 * 8);
        assert_eq!(end(&timing, 0, 0), 0);
    }

    #[test]
    fn test_drawn_slots_leave_the_measured_gaps() {
        for (mode, slots) in [(HMode::H32, 16), (HMode::H40, 18)] {
            let total = accesses(mode);
            let mine: Vec<u64> = (0..total)
                .filter(|k| drawn_external(mode, (k + first_access(mode)) % total))
                .collect();
            assert_eq!(mine.len(), slots);
            let inside = mine.windows(2).map(|pair| pair[1] - pair[0]).max();
            let across = mine[0] + total - mine[slots - 1];
            if mode == HMode::H40 {
                // Across the line's end 26 accesses, the largest gap TascoDLX measured (on
                // SpritesMind); inside it the 24 sprite reads after the pair that follows
                // the blocks.
                assert_eq!((inside, across), (Some(24), 26));
            }
        }
        assert_eq!(H40_DRAWN.len(), 18);
        assert_eq!(H32_BLANKED.len(), 166);
    }
}
