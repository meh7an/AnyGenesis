//! The VDP's ports and memories: 24 registers, 64 KiB of VRAM, 64 CRAM and 40 VSRAM words,
//! the control port's two-word commands, the data port with auto-increment, DMA, the status
//! register and the HV counter. Time and interrupts come from the embedded [`Timing`].
//!
//! As time passes, each picture line is drawn ([`render`]) where its horizontal blanking
//! ends, from the state at that moment; a frame is finished once line 223 is drawn, before
//! its vertical interrupt point, and [`Vdp::frame`] gives it until the next one is.
//!
//! Commands latch in two words: the first sets the address bits 13-0 and the code bits
//! CD1-CD0 at once, the second the code bits CD5-CD2 and the address bits 15-14, and starts
//! a DMA when CD5 and register 1 bit 4 are set. A word whose top bits are `10` while no
//! first word waits is a register write instead. Status reads and data-port accesses drop
//! a waiting first word.
//!
//! Data-port writes go through a four-entry FIFO ([`fifo`]) that drains through the access
//! slots of each line; the 68000 waits while it is full, a 68K-to-VDP DMA, a fill or a copy
//! starts once it has drained, and a data-port read waits for it to drain and for a slot.
//! The status register shows it empty or full, and keeps the sprite overflow and collision
//! bits of the lines drawn since the last status-port read. VRAM keeps bytes in the 68000's
//! order; a word written to an odd address lands with its bytes swapped. CRAM keeps its 9
//! colour bits as 0x0EEE, VSRAM 11 bits.
//!
//! Sources (behaviour only, no emulator code read): Plutiedev's VDP register reference
//! (register bits, DMA registers and types), Kabuto's hardware notes (status upper bits,
//! FIFO, DMA costs) and Sega Retro, plus Sonic 1's own DMA fill macro for how a fill's
//! length counts the data write. Chosen and open to revision: the status register's upper
//! bits read 0x3400 (real hardware leaves bus residue there), register writes leave the
//! command latch alone, odd-address word writes swap bytes, CRAM and VSRAM reads give zeros
//! outside their bits, and VSRAM above its 40 words reads 0 and ignores writes.

mod dma;
mod fifo;
pub mod render;

use anygenesis_shared::Size;
pub use render::Frame;

use crate::{
    hardware::{Hardware, Memory},
    log::AccessKind,
    timing::{ACTIVE_LINES, CPU_DIVIDER, Timing},
    vdp::{
        fifo::Fifo,
        render::{Screen, Source},
    },
};

/// Bytes of VRAM.
pub const VRAM_SIZE: usize = 0x1_0000;
/// Words of CRAM: four palettes of 16 colours.
pub const CRAM_WORDS: usize = 64;
/// Words of VSRAM: a vertical scroll value per plane for each of 20 two-cell columns.
pub const VSRAM_WORDS: usize = 40;
/// Registers the VDP has in Mega Drive mode.
pub const REGISTERS: usize = 24;
/// The bits a CRAM word keeps: three per colour channel.
pub const CRAM_MASK: u16 = 0x0EEE;
/// The bits a VSRAM word keeps.
pub const VSRAM_MASK: u16 = 0x07FF;

/// Status register bits apart from the ones [`Timing`] gives.
pub mod status {
    /// The upper six bits as read here.
    pub const FIXED: u16 = 0x3400;
    /// The write FIFO is empty.
    pub const FIFO_EMPTY: u16 = 0x0200;
    /// The write FIFO is full: the next data-port write waits.
    pub const FIFO_FULL: u16 = 0x0100;
    /// A line drawn since the last status read met more sprites than it takes.
    pub const SPRITE_OVERFLOW: u16 = 0x0040;
    /// Two sprites' opaque pixels met on a line drawn since the last status read.
    pub const SPRITE_COLLISION: u16 = 0x0020;
    /// A fill or copy DMA is in progress.
    pub const DMA_BUSY: u16 = 0x0002;
}

/// The memory a command points the data port at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Vram,
    Cram,
    Vsram,
}

/// What the data port does under the current command code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Direction {
    target: Target,
    write: bool,
}

impl Direction {
    /// Decodes the code bits CD3-CD0; `None` for codes with no target, among them the
    /// undocumented 8-bit VRAM read (0x0C).
    fn of(code: u8) -> Option<Self> {
        let (target, write) = match code & 0x0F {
            0x00 => (Target::Vram, false),
            0x01 => (Target::Vram, true),
            0x03 => (Target::Cram, true),
            0x04 => (Target::Vsram, false),
            0x05 => (Target::Vsram, true),
            0x08 => (Target::Cram, false),
            _ => return None,
        };
        Some(Self { target, write })
    }
}

/// What the VDP has been told so far, for reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub register_writes: u64,
    /// Command words (both halves) written to the control port.
    pub command_words: u64,
    pub data_writes: u64,
    pub data_reads: u64,
    pub status_reads: u64,
    /// 68K-to-VDP DMA transfers.
    pub transfers: u64,
    pub fills: u64,
    pub copies: u64,
    /// 68000 cycles transfers froze the CPU for.
    pub stall_cycles: u64,
    pub psg_writes: u64,
}

/// The VDP, ready for the [`Hardware`] slot.
#[derive(Debug, Clone)]
pub struct Vdp {
    timing: Timing,
    registers: [u8; REGISTERS],
    vram: Box<[u8; VRAM_SIZE]>,
    cram: [u16; CRAM_WORDS],
    vsram: [u16; VSRAM_WORDS],
    /// The command code, CD5-CD0.
    code: u8,
    address: u16,
    /// The control port holds the first word of a command.
    pending: bool,
    /// A fill DMA waits for its data write.
    fill: bool,
    /// Master clock where the running fill or copy ends.
    busy_until: u64,
    /// The HV counter as frozen by register 0 bit 1.
    latch: Option<u16>,
    fifo: Fifo,
    /// 68000 cycles the last access froze the CPU for, until the bus takes them.
    stall: u32,
    /// A register write may have moved the next interrupt, until the scheduler asks.
    rescheduled: bool,
    /// What the last access did that no rule covers, until the bus takes it.
    fault: Option<AccessKind>,
    counts: Counts,
    screen: Screen,
    /// The sprite overflow and collision bits, until a status read clears them.
    sprite_status: u16,
}

impl Default for Vdp {
    fn default() -> Self {
        Self {
            timing: Timing::default(),
            registers: [0; REGISTERS],
            vram: Box::new([0; VRAM_SIZE]),
            cram: [0; CRAM_WORDS],
            vsram: [0; VSRAM_WORDS],
            code: 0,
            address: 0,
            pending: false,
            fill: false,
            busy_until: 0,
            latch: None,
            fifo: Fifo::default(),
            stall: 0,
            rescheduled: false,
            fault: None,
            counts: Counts::default(),
            screen: Screen::default(),
            sprite_status: 0,
        }
    }
}

impl Vdp {
    /// The beam position and interrupt state.
    pub fn timing(&self) -> &Timing {
        &self.timing
    }

    /// Registers 0-23 as last written.
    pub fn registers(&self) -> &[u8; REGISTERS] {
        &self.registers
    }

    /// VRAM, bytes in the 68000's order: a word's high byte at the even address.
    pub fn vram(&self) -> &[u8; VRAM_SIZE] {
        &self.vram
    }

    /// CRAM: 64 colours as 0x0BGR words, each channel's three bits at 3-1.
    pub fn cram(&self) -> &[u16; CRAM_WORDS] {
        &self.cram
    }

    /// VSRAM: 40 vertical scroll words of 11 bits.
    pub fn vsram(&self) -> &[u16; VSRAM_WORDS] {
        &self.vsram
    }

    /// What the VDP has been told so far.
    pub fn counts(&self) -> &Counts {
        &self.counts
    }

    /// The status register.
    pub fn status(&self) -> u16 {
        let now = self.timing.clock();
        let busy = if now < self.busy_until {
            status::DMA_BUSY
        } else {
            0
        };
        let fifo = match self.fifo.len(now) {
            0 => status::FIFO_EMPTY,
            fifo::DEPTH.. => status::FIFO_FULL,
            _ => 0,
        };
        status::FIXED | fifo | self.sprite_status | self.timing.status() | busy
    }

    /// The HV counter, frozen while register 0 bit 1 is set.
    pub fn hv(&self) -> u16 {
        self.latch.unwrap_or_else(|| self.timing.hv())
    }

    /// The last finished frame; black until the first one is.
    pub fn frame(&self) -> &Frame {
        self.screen.frame()
    }

    /// Whether lines keep the hardware's sprite and pixel limits (on by default). Off, the
    /// sprites past them are drawn too; the status flags stay the hardware's (M6.2).
    pub fn set_sprite_limit(&mut self, on: bool) {
        self.screen.unlimited_sprites = !on;
    }

    /// Draws the line whose pixels start now, if it is a picture line.
    fn draw_line(&mut self) {
        let line = self.timing.line();
        if line < ACTIVE_LINES {
            let source = Source {
                registers: &self.registers,
                vram: &self.vram,
                cram: &self.cram,
                vsram: &self.vsram,
            };
            // Below 224, so it fits.
            let flags = self.screen.draw_line(source, line as usize);
            if flags.overflow {
                self.sprite_status |= status::SPRITE_OVERFLOW;
            }
            if flags.collision {
                self.sprite_status |= status::SPRITE_COLLISION;
            }
        }
    }

    fn fault(&mut self, kind: AccessKind) {
        self.fault = Some(kind);
    }

    fn increment(&self) -> u16 {
        self.registers[15].into()
    }

    fn control(&mut self, value: u16, memory: Memory<'_>) {
        if self.pending {
            self.pending = false;
            self.counts.command_words += 1;
            self.address = self.address & 0x3FFF | (value & 0x0003) << 14;
            // CD5-CD2 sit in bits 7-4 of the second word.
            self.code = self.code & 0x03 | (value >> 2 & 0x3C) as u8;
            if self.code & 0x20 != 0 && self.registers[1] & 0x10 != 0 {
                self.start_dma(memory);
            }
        } else if value & 0xC000 == 0x8000 {
            self.write_register(usize::from(value >> 8 & 0x1F), value as u8);
        } else {
            self.pending = true;
            self.counts.command_words += 1;
            self.address = self.address & 0xC000 | value & 0x3FFF;
            self.code = self.code & 0x3C | (value >> 14) as u8;
        }
    }

    fn write_register(&mut self, register: usize, value: u8) {
        self.counts.register_writes += 1;
        let Some(slot) = self.registers.get_mut(register) else {
            self.fault(AccessKind::VdpRegister);
            return;
        };
        *slot = value;
        self.timing.set_register(register, value);
        // The interrupt enables, the line counter's reload value and the mode move events.
        self.rescheduled |= matches!(register, 0 | 1 | 10 | 12);
        match register {
            0 if value & 0x02 == 0 => self.latch = None,
            0 => {
                self.latch.get_or_insert(self.timing.hv());
            }
            // Mode 4 (bit 2 clear) and 128 KiB VRAM (bit 7) are not built.
            1 if value & 0x04 == 0 || value & 0x80 != 0 => self.fault(AccessKind::VdpRegister),
            // Only both resolution bits set or both clear are documented; double-resolution
            // interlace (bits 2-1 = 3) is not drawn.
            12 if (value & 0x01 != 0) != (value & 0x80 != 0) || value & 0x06 == 0x06 => {
                self.fault(AccessKind::VdpRegister)
            }
            // A size field of `10` is prohibited, and a plane larger than 8 KiB does not fit
            // the name-table address space.
            16 if value & 0x03 == 0x02
                || value & 0x30 == 0x20
                || render::plane_cells(value) * render::plane_cells(value >> 4) > 4096 =>
            {
                self.fault(AccessKind::VdpRegister)
            }
            _ => {}
        }
    }

    fn write_data(&mut self, value: u16) {
        self.pending = false;
        self.counts.data_writes += 1;
        match Direction::of(self.code) {
            Some(Direction {
                target,
                write: true,
            }) => {
                self.queue_write(target);
                self.write_target(target, value);
                if self.fill {
                    self.run_fill(target, value);
                }
            }
            _ => self.fault(AccessKind::VdpCommand),
        }
    }

    fn read_data(&mut self) -> u16 {
        self.pending = false;
        self.counts.data_reads += 1;
        match Direction::of(self.code) {
            Some(Direction {
                target,
                write: false,
            }) => {
                // Chosen, unverified: the read takes the first slot after the FIFO drained.
                let ready = dma::end(&self.timing, self.drained(), 1);
                self.stall_until(ready);
                let value = self.read_target(target);
                self.address = self.address.wrapping_add(self.increment());
                value
            }
            _ => {
                self.fault(AccessKind::VdpCommand);
                0
            }
        }
    }

    /// Stores `value` where the address points and moves the address on.
    fn write_target(&mut self, target: Target, value: u16) {
        let address = usize::from(self.address);
        match target {
            Target::Vram => {
                let [high, low] = value.to_be_bytes();
                let even = address & !1;
                let (first, second) = if address & 1 == 0 {
                    (high, low)
                } else {
                    (low, high)
                };
                self.vram[even] = first;
                self.vram[even + 1] = second;
            }
            Target::Cram => self.cram[address >> 1 & (CRAM_WORDS - 1)] = value & CRAM_MASK,
            Target::Vsram => {
                if let Some(word) = self.vsram.get_mut(address >> 1 & 0x3F) {
                    *word = value & VSRAM_MASK;
                }
            }
        }
        self.address = self.address.wrapping_add(self.increment());
    }

    fn read_target(&self, target: Target) -> u16 {
        let address = usize::from(self.address);
        match target {
            Target::Vram => {
                let even = address & !1;
                u16::from_be_bytes([self.vram[even], self.vram[even + 1]])
            }
            Target::Cram => self.cram[address >> 1 & (CRAM_WORDS - 1)],
            Target::Vsram => self.vsram.get(address >> 1 & 0x3F).copied().unwrap_or(0),
        }
    }

    /// Where the FIFO's queued writes have all gone out: now when it is empty.
    fn drained(&self) -> u64 {
        self.fifo.drained(self.timing.clock())
    }

    /// Queues a data-port write to `target`, freezing the 68000 while the FIFO is full.
    fn queue_write(&mut self, target: Target) {
        let slots = match target {
            Target::Vram => 2,
            Target::Cram | Target::Vsram => 1,
        };
        let ready = self.fifo.push(&self.timing, self.timing.clock(), slots);
        self.stall_until(ready);
    }

    /// Moves time on by `cycles` 68000 cycles: [`Hardware::advance_to`] for tests and tools
    /// that drive the VDP alone.
    pub fn advance(&mut self, cycles: u32) {
        self.advance_to(self.timing.clock() + u64::from(cycles) * CPU_DIVIDER);
    }

    /// Freezes the 68000 until master clock `end`.
    fn stall_until(&mut self, end: u64) {
        let clocks = end.saturating_sub(self.timing.clock());
        // A DMA moves at most 0x10000 words, far fewer cycles than u32::MAX.
        let cycles = clocks.div_ceil(CPU_DIVIDER) as u32;
        self.stall += cycles;
        self.counts.stall_cycles += u64::from(cycles);
    }
}

impl Hardware for Vdp {
    fn read(&mut self, port: u8, _size: Size) -> u16 {
        match port {
            0x00..=0x03 => self.read_data(),
            0x04..=0x07 => {
                // Reading the status ends a half-written command and clears the sprite bits.
                self.pending = false;
                self.counts.status_reads += 1;
                let value = self.status();
                self.sprite_status = 0;
                value
            }
            0x08..=0x0F => self.hv(),
            _ => {
                self.fault(AccessKind::VdpCommand);
                0
            }
        }
    }

    fn write(&mut self, port: u8, _size: Size, value: u16, memory: Memory<'_>) {
        match port {
            0x00..=0x03 => self.write_data(value),
            0x04..=0x07 => self.control(value, memory),
            0x10..=0x17 => self.counts.psg_writes += 1,
            _ => self.fault(AccessKind::VdpCommand),
        }
    }

    fn advance_to(&mut self, target: u64) {
        loop {
            let pixels = self.timing.next_pixels();
            if pixels > target {
                break;
            }
            self.timing.advance_to(pixels);
            self.draw_line();
        }
        self.timing.advance_to(target);
    }

    fn next_interrupt(&self) -> Option<u64> {
        self.timing.next_interrupt()
    }

    fn next_frame(&self) -> u64 {
        self.timing.next_frame()
    }

    fn take_rescheduled(&mut self) -> bool {
        std::mem::take(&mut self.rescheduled)
    }

    fn frames(&self) -> u64 {
        self.timing.frames()
    }

    fn pending_interrupt(&self) -> Option<u8> {
        self.timing.pending()
    }

    fn enabled_interrupts(&self) -> u8 {
        self.timing.enabled()
    }

    fn acknowledge(&mut self, level: u8) {
        self.timing.acknowledge(level);
    }

    fn take_stall(&mut self) -> u32 {
        std::mem::take(&mut self.stall)
    }

    fn take_fault(&mut self) -> Option<AccessKind> {
        self.fault.take()
    }

    fn z80_interrupt(&self, clock: u64) -> bool {
        self.timing.z80_interrupt(clock)
    }

    /// Drops a half-written command only: the 68000's RESET instruction is taken not to
    /// reach the VDP's registers or memories (chosen, unverified).
    fn reset(&mut self) {
        self.pending = false;
    }
}

#[cfg(test)]
mod tests {
    use anygenesis_abi::RAM_SIZE;

    use super::*;
    use crate::{hardware::port, timing};

    /// Work RAM and a small ROM for DMA sources.
    pub(super) struct Sources {
        pub rom: Vec<u8>,
        pub ram: Box<[u8; RAM_SIZE]>,
    }

    impl Sources {
        pub fn new() -> Self {
            Self {
                rom: (0..0x100).map(|byte| byte as u8).collect(),
                ram: Box::new([0; RAM_SIZE]),
            }
        }

        pub fn memory(&self) -> Memory<'_> {
            Memory {
                rom: &self.rom,
                ram: &self.ram,
            }
        }
    }

    /// Writes words to `port`, then takes the fault the last one raised.
    pub(super) fn write(vdp: &mut Vdp, port: u8, words: &[u16]) -> Option<AccessKind> {
        let sources = Sources::new();
        write_with(vdp, &sources, port, words)
    }

    pub(super) fn write_with(
        vdp: &mut Vdp,
        sources: &Sources,
        port: u8,
        words: &[u16],
    ) -> Option<AccessKind> {
        for &word in words {
            vdp.write(port, Size::Word, word, sources.memory());
        }
        vdp.take_fault()
    }

    pub(super) fn read(vdp: &mut Vdp, port: u8) -> u16 {
        vdp.read(port, Size::Word)
    }

    /// A VDP in mode 5 with the display off, auto-increment 2.
    pub(super) fn vdp() -> Vdp {
        let mut vdp = Vdp::default();
        write(&mut vdp, port::CONTROL, &[0x8104, 0x8F02]);
        vdp
    }

    /// A command word pair for `code` (CD5-CD0) at `address`.
    pub(super) fn command(code: u8, address: u16) -> [u16; 2] {
        let code = u16::from(code);
        [
            (code & 3) << 14 | address & 0x3FFF,
            (code & 0x3C) << 2 | address >> 14,
        ]
    }

    #[test]
    fn test_register_writes_are_told_from_command_halves() {
        let mut vdp = vdp();
        assert_eq!(write(&mut vdp, port::CONTROL, &[0x8C81, 0x4000]), None);
        // The second half of a command may look like a register write.
        write(&mut vdp, port::CONTROL, &[0x8000, 0x8B03]);
        assert_eq!(vdp.registers()[0xC], 0x81);
        assert_eq!(vdp.registers()[0xB], 0x03);
        assert_eq!(vdp.registers()[0], 0);
        assert_eq!(vdp.counts().register_writes, 4);
        assert_eq!(vdp.counts().command_words, 2);
        assert_eq!(vdp.timing().mode(), timing::HMode::H40);
    }

    #[test]
    fn test_status_read_ends_a_half_written_command() {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &[0x4000]);
        // Power-on: the beam at H 0 is inside horizontal blanking, and the display is off.
        assert_eq!(
            read(&mut vdp, port::CONTROL),
            status::FIXED | status::FIFO_EMPTY | timing::status::VBLANK | timing::status::HBLANK
        );
        write(&mut vdp, port::CONTROL, &[0x8B03]);
        assert_eq!(vdp.registers()[0xB], 0x03);
    }

    #[test]
    fn test_vram_words_auto_increment_and_odd_addresses_swap_bytes() {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &command(0x01, 0x1234));
        write(&mut vdp, port::DATA, &[0xAABB, 0xCCDD]);
        assert_eq!(vdp.vram()[0x1234..0x1238], [0xAA, 0xBB, 0xCC, 0xDD]);
        write(&mut vdp, port::CONTROL, &command(0x01, 0x2001));
        write(&mut vdp, port::DATA, &[0x1122]);
        assert_eq!(vdp.vram()[0x2000..0x2002], [0x22, 0x11]);
        // Reads take the word at the even address, whatever bit 0 says.
        write(&mut vdp, port::CONTROL, &command(0x00, 0x1235));
        assert_eq!(read(&mut vdp, port::DATA), 0xAABB);
        assert_eq!(read(&mut vdp, port::DATA), 0xCCDD);
        assert_eq!(vdp.take_fault(), None);
        assert_eq!(vdp.counts().data_reads, 2);
    }

    #[test]
    fn test_address_bits_15_14_come_from_the_second_word_and_wrap() {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &command(0x01, 0xFFFE));
        write(&mut vdp, port::DATA, &[0x0102, 0x0304]);
        assert_eq!(vdp.vram()[0xFFFE..], [0x01, 0x02]);
        assert_eq!(vdp.vram()[..2], [0x03, 0x04]);
    }

    #[test]
    fn test_cram_and_vsram_keep_their_bits_and_bounds() {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &command(0x03, 0x007E));
        write(&mut vdp, port::DATA, &[0xFFFF, 0x0123]);
        assert_eq!(vdp.cram()[63], CRAM_MASK);
        // Past the end the address wraps to the first colour.
        assert_eq!(vdp.cram()[0], 0x0022);
        write(&mut vdp, port::CONTROL, &command(0x05, 0x004E));
        write(&mut vdp, port::DATA, &[0xFFFF, 0x0456]);
        assert_eq!(vdp.vsram()[39], VSRAM_MASK);
        // Word 40 does not exist: the write is lost and reads give 0.
        assert!(vdp.vsram().iter().take(39).all(|&word| word == 0));
        write(&mut vdp, port::CONTROL, &command(0x04, 0x0050));
        assert_eq!(read(&mut vdp, port::DATA), 0);
        write(&mut vdp, port::CONTROL, &command(0x08, 0x007E));
        assert_eq!(read(&mut vdp, port::DATA), CRAM_MASK);
        assert_eq!(vdp.take_fault(), None);
    }

    #[test]
    fn test_data_access_against_the_code_direction_is_a_fault() {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &command(0x01, 0));
        assert_eq!(read(&mut vdp, port::DATA), 0);
        assert_eq!(vdp.take_fault(), Some(AccessKind::VdpCommand));
        write(&mut vdp, port::CONTROL, &command(0x00, 0));
        assert_eq!(
            write(&mut vdp, port::DATA, &[0x1234]),
            Some(AccessKind::VdpCommand)
        );
        assert_eq!(vdp.vram()[..2], [0, 0]);
        // The undocumented 8-bit VRAM read.
        write(&mut vdp, port::CONTROL, &command(0x0C, 0));
        read(&mut vdp, port::DATA);
        assert_eq!(vdp.take_fault(), Some(AccessKind::VdpCommand));
    }

    #[test]
    fn test_registers_and_modes_not_built_are_faults() {
        let mut vdp = vdp();
        assert_eq!(
            write(&mut vdp, port::CONTROL, &[0x9800]),
            Some(AccessKind::VdpRegister)
        );
        assert_eq!(
            write(&mut vdp, port::CONTROL, &[0x8100]),
            Some(AccessKind::VdpRegister)
        );
        assert_eq!(
            write(&mut vdp, port::CONTROL, &[0x8184]),
            Some(AccessKind::VdpRegister)
        );
        assert_eq!(
            write(&mut vdp, port::CONTROL, &[0x8C01]),
            Some(AccessKind::VdpRegister)
        );
        assert_eq!(write(&mut vdp, port::CONTROL, &[0x8C00, 0x8174]), None);
    }

    #[test]
    fn test_modes_the_renderer_does_not_draw_are_faults() {
        let mut vdp = vdp();
        // Double-resolution interlace; shadow/highlight and normal interlace are drawn.
        for (word, fault) in [
            (0x8C87, Some(AccessKind::VdpRegister)),
            (0x8C89, None),
            (0x8C83, None),
        ] {
            assert_eq!(write(&mut vdp, port::CONTROL, &[word]), fault, "{word:04X}");
        }
        // Plane sizes: a `10` field, then 128x64 cells (16 KiB), then 64x64 and 128x32.
        for (word, fault) in [
            (0x9002, Some(AccessKind::VdpRegister)),
            (0x9020, Some(AccessKind::VdpRegister)),
            (0x9013, Some(AccessKind::VdpRegister)),
            (0x9011, None),
            (0x9003, None),
        ] {
            assert_eq!(write(&mut vdp, port::CONTROL, &[word]), fault, "{word:04X}");
        }
    }

    #[test]
    fn test_other_ports_count_the_psg_and_fault_the_rest() {
        let mut vdp = vdp();
        vdp.write(port::PSG + 1, Size::Byte, 0x9F, Sources::new().memory());
        assert_eq!(vdp.counts().psg_writes, 1);
        assert_eq!(vdp.take_fault(), None);
        assert_eq!(
            write(&mut vdp, port::HV, &[0]),
            Some(AccessKind::VdpCommand)
        );
        assert_eq!(read(&mut vdp, 0x1C), 0);
        assert_eq!(vdp.take_fault(), Some(AccessKind::VdpCommand));
    }

    #[test]
    fn test_hv_counter_follows_the_beam_and_freezes_while_latched() {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &[0x8C81, 0x8144]);
        vdp.advance(10 * 489);
        let hv = read(&mut vdp, port::HV);
        assert_eq!(hv, vdp.timing().hv());
        assert_eq!(hv >> 8, 10);
        write(&mut vdp, port::CONTROL, &[0x8002]);
        vdp.advance(1000);
        assert_eq!(read(&mut vdp, port::HV), hv);
        write(&mut vdp, port::CONTROL, &[0x8000]);
        assert_ne!(read(&mut vdp, port::HV), hv);
    }

    /// A VDP in H40 with the display on, writing VRAM from 0: 18 slots a line.
    fn drawing() -> Vdp {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &[0x8C81, 0x8144]);
        write(&mut vdp, port::CONTROL, &command(0x01, 0));
        vdp
    }

    /// 68000 cycles from master clock 0 to `clock`, rounded up.
    fn cycles_to(clock: u64) -> u32 {
        clock.div_ceil(CPU_DIVIDER) as u32
    }

    #[test]
    fn test_fifo_holds_four_writes_and_stalls_the_fifth_until_the_first_leaves() {
        let mut vdp = drawing();
        write(&mut vdp, port::DATA, &[1, 2, 3]);
        assert_eq!(vdp.take_stall(), 0);
        assert_eq!(vdp.status() & (status::FIFO_EMPTY | status::FIFO_FULL), 0);
        write(&mut vdp, port::DATA, &[4]);
        assert_eq!(vdp.take_stall(), 0);
        assert_ne!(vdp.status() & status::FIFO_FULL, 0);
        // A VRAM word takes two slots: the fifth write waits for the first's to end.
        let slots = |count| dma::end(&vdp.timing().clone(), 0, count);
        let (first, fifth) = (slots(2), slots(10));
        write(&mut vdp, port::DATA, &[5]);
        assert_eq!(vdp.take_stall(), cycles_to(first));
        // Writes reach VRAM when queued.
        assert_eq!(vdp.vram()[8..10], [0, 5]);
        // The fifth leaves after ten slots.
        vdp.advance(cycles_to(fifth) - 1);
        assert_eq!(vdp.status() & status::FIFO_EMPTY, 0);
        vdp.advance(1);
        assert_ne!(vdp.status() & status::FIFO_EMPTY, 0);
    }

    #[test]
    fn test_fifo_takes_one_slot_per_cram_word_and_drains_fast_when_blanked() {
        let mut vdp = drawing();
        write(&mut vdp, port::CONTROL, &command(0x03, 0));
        let first = dma::end(vdp.timing(), 0, 1);
        write(&mut vdp, port::DATA, &[1, 2, 3, 4, 5]);
        assert_eq!(vdp.take_stall(), cycles_to(first));
        // Display off: a slot every two pixels, so a VRAM word leaves within 40 master
        // clocks.
        let mut vdp = drawing();
        write(&mut vdp, port::CONTROL, &[0x8104]);
        write(&mut vdp, port::CONTROL, &command(0x01, 0));
        write(&mut vdp, port::DATA, &[1, 2, 3, 4, 5]);
        assert_eq!(vdp.take_stall(), cycles_to(5 * 8));
    }

    #[test]
    fn test_dma_transfer_starts_once_the_fifo_drained() {
        let mut vdp = drawing();
        write(&mut vdp, port::DATA, &[1, 2]);
        // Source word 8, one word, to VRAM 0x100.
        let registers = [0x8154, 0x9301, 0x9400, 0x9508, 0x9600, 0x9700];
        write(&mut vdp, port::CONTROL, &registers);
        write(&mut vdp, port::CONTROL, &command(0x21, 0x0100));
        // Four slots for the queued writes, then two for the transfer.
        assert_eq!(vdp.take_stall(), cycles_to(dma::end(vdp.timing(), 0, 6)));
    }

    #[test]
    fn test_data_read_waits_for_the_fifo_and_a_slot() {
        let mut vdp = drawing();
        write(&mut vdp, port::DATA, &[0x1234]);
        write(&mut vdp, port::CONTROL, &command(0x00, 0));
        assert_eq!(read(&mut vdp, port::DATA), 0x1234);
        assert_eq!(vdp.take_stall(), cycles_to(dma::end(vdp.timing(), 0, 3)));
    }

    #[test]
    fn test_register_writes_that_move_interrupts_reschedule() {
        let mut vdp = vdp();
        vdp.take_rescheduled();
        for (word, moved) in [
            (0x8F02, false),
            (0x8A10, true),
            (0x8014, true),
            (0x8C81, true),
        ] {
            write(&mut vdp, port::CONTROL, &[word]);
            assert_eq!(vdp.take_rescheduled(), moved, "{word:04X}");
        }
    }

    #[test]
    fn test_reset_drops_a_half_written_command_only() {
        let mut vdp = vdp();
        write(&mut vdp, port::CONTROL, &command(0x01, 0x0100));
        write(&mut vdp, port::CONTROL, &[0x4000]);
        vdp.reset();
        write(&mut vdp, port::CONTROL, &[0x8B03]);
        assert_eq!(vdp.registers()[0xB], 0x03);
        assert_eq!(vdp.registers()[0xF], 0x02);
    }
}
