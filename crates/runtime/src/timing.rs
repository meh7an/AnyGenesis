//! NTSC video timing: where the beam is, the H and V counters read there, and the two
//! interrupts the VDP derives from it.
//!
//! Time is kept in master clocks, of which the 68000 gets one in seven, so lines and frames
//! stay exact (a line is 488 4/7 68000 cycles). A line starts where the H counter reads 0.
//! The vertical interrupt becomes pending on line 224, the first line below a 224-line
//! picture; that is where a frame ends. The horizontal interrupt comes from a line counter
//! that steps once per line while the picture is drawn and reloads from register 10 below
//! it.
//!
//! Inside a line the VDP counts pixels with a 9-bit counter whose upper 8 bits are the H
//! counter. Both modes take 3420 master clocks a line: H32 counts 342 pixels of 10 master
//! clocks, H40 420 pixels of 8, except for 30 slower ones of 10 during horizontal sync.
//! Each counter jumps once a line (H32 0x127 to 0x1D2, H40 0x16C to 0x1C9), which is why
//! the H counter skips from 0x93 to 0xE9 (H32) and from 0xB6 to 0xE4 (H40).
//!
//! Sources (behaviour only): Kabuto's hardware notes as mirrored on Plutiedev (line length,
//! line count, H/V counter ranges and transitions, blanking flags, where the vertical
//! interrupt flag sets, the slow H40 pixels during horizontal blanking), Nemesis's pixel
//! counter tables and posts on the SpritesMind forum (the 9-bit counter, 342 and 420 pixels
//! a line), Charles MacDonald's VDP document and Sega Retro's interrupt page (line counter
//! reload), and a SpritesMind post on VBlank reading set while the display is off. Where
//! the slow H40 stretch starts is chosen: Kabuto measured 32 slow pixels 24 after the right
//! edge, and a 3420-clock line leaves room for 30.

/// Master clocks per second on an NTSC console: fifteen times the colour subcarrier.
pub const MASTER_CLOCK_HZ: u64 = 53_693_175;
/// Master clocks per 68000 cycle.
pub const CPU_DIVIDER: u64 = 7;
/// Master clocks per line, in both H32 and H40.
pub const LINE_CLOCKS: u64 = 3420;
/// Lines per NTSC frame.
pub const LINES: u64 = 262;
/// Master clocks per frame.
pub const FRAME_CLOCKS: u64 = LINE_CLOCKS * LINES;
/// Picture lines in V28, the only vertical mode NTSC displays properly.
pub const ACTIVE_LINES: u64 = 224;

/// The vertical interrupt's level.
pub const VINT_LEVEL: u8 = 6;
/// The horizontal interrupt's level.
pub const HINT_LEVEL: u8 = 4;

/// Status register bits that come from the timing.
pub mod status {
    /// A vertical interrupt is pending (F).
    pub const VINT: u16 = 0x0080;
    /// The odd field of an interlaced picture is being drawn.
    pub const ODD: u16 = 0x0010;
    /// Vertical blanking, or the display is off.
    pub const VBLANK: u16 = 0x0008;
    /// Horizontal blanking.
    pub const HBLANK: u16 = 0x0004;
}

/// The last line (by the V counter's numbering) of vertical blanking: the flag clears as
/// the V counter reaches 0x1FF, the last line of the frame.
const LAST_VBLANK_LINE: u64 = LINES - 2;
/// The V counter reads the line number up to 0xEA, then jumps to 0x1E5 for the rest.
const V_LAST_BEFORE_JUMP: u64 = 0xEA;
const V_AFTER_JUMP: u64 = 0x1E5;
/// The H40 pixels that take 10 master clocks instead of 8.
const H40_SLOW_START: u64 = 344;
const H40_SLOW_PIXELS: u64 = 30;

/// The horizontal resolution, register 12 bit 0: it sets the pixel counter's layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HMode {
    /// 256 pixels wide, the power-on mode.
    #[default]
    H32,
    /// 320 pixels wide.
    H40,
}

/// Where a line's events fall, in master clocks from its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Positions {
    /// The vertical interrupt flag sets as the H counter goes from 0x00 to 0x01.
    vint: u64,
    /// The V counter steps here (H counter 0xA5 in H40, 0x85 in H32).
    v_step: u64,
    /// Horizontal blanking starts (H counter 0xB3 in H40, 0x93 in H32); the line counter
    /// steps here too.
    hblank_start: u64,
    /// Horizontal blanking ends (H counter 0x06 in H40, 0x05 in H32).
    hblank_end: u64,
}

impl HMode {
    const H32_POSITIONS: Positions = Self::H32.positions([0x01, 0x85, 0x93, 0x05]);
    const H40_POSITIONS: Positions = Self::H40.positions([0x01, 0xA5, 0xB3, 0x06]);

    /// The pixel counter's last value before its jump and the value it jumps to.
    const fn jump(self) -> (u64, u64) {
        match self {
            Self::H32 => (0x127, 0x1D2),
            Self::H40 => (0x16C, 0x1C9),
        }
    }

    /// Master clocks from the start of a line to the start of its pixel `pixel`.
    const fn pixel_clock(self, pixel: u64) -> u64 {
        match self {
            Self::H32 => pixel * 10,
            Self::H40 => {
                let slow = if pixel < H40_SLOW_START {
                    0
                } else if pixel < H40_SLOW_START + H40_SLOW_PIXELS {
                    pixel - H40_SLOW_START
                } else {
                    H40_SLOW_PIXELS
                };
                pixel * 8 + slow * 2
            }
        }
    }

    /// The pixel being drawn `position` master clocks into the line.
    const fn pixel_at(self, position: u64) -> u64 {
        match self {
            Self::H32 => position / 10,
            Self::H40 => {
                let slow = H40_SLOW_START * 8;
                let fast = slow + H40_SLOW_PIXELS * 10;
                if position < slow {
                    position / 8
                } else if position < fast {
                    H40_SLOW_START + (position - slow) / 10
                } else {
                    H40_SLOW_START + H40_SLOW_PIXELS + (position - fast) / 8
                }
            }
        }
    }

    /// The pixel counter's value at pixel `pixel` of the line.
    const fn counter(self, pixel: u64) -> u64 {
        let (last, resume) = self.jump();
        if pixel <= last {
            pixel
        } else {
            pixel - last - 1 + resume
        }
    }

    /// Master clocks from the start of a line to where the H counter first reads `h`.
    const fn h_position(self, h: u64) -> u64 {
        let (last, resume) = self.jump();
        let value = h * 2;
        let pixel = if value <= last {
            value
        } else if value < resume {
            last + 1
        } else {
            value - resume + last + 1
        };
        self.pixel_clock(pixel)
    }

    /// The positions of the H counter values `[vint, v_step, hblank_start, hblank_end]`.
    const fn positions(self, h: [u64; 4]) -> Positions {
        Positions {
            vint: self.h_position(h[0]),
            v_step: self.h_position(h[1]),
            hblank_start: self.h_position(h[2]),
            hblank_end: self.h_position(h[3]),
        }
    }

    const fn line_positions(self) -> Positions {
        match self {
            Self::H32 => Self::H32_POSITIONS,
            Self::H40 => Self::H40_POSITIONS,
        }
    }
}

/// Interlacing, register 12 bits 2-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interlace {
    #[default]
    Off,
    /// Mode 1: two fields of the same lines.
    Normal,
    /// Mode 2: two fields of different lines, doubling the vertical resolution.
    Double,
}

/// The beam position and the interrupt state derived from it.
#[derive(Debug, Clone, Default)]
pub struct Timing {
    /// Master clocks since power-on.
    clock: u64,
    /// Vertical interrupt points passed: frames that have ended.
    frames: u64,
    line_counter: u8,
    /// Register 10.
    line_reload: u8,
    /// Register 0 bit 4.
    hint_enabled: bool,
    /// Register 1 bit 5.
    vint_enabled: bool,
    /// Register 1 bit 6.
    display: bool,
    mode: HMode,
    interlace: Interlace,
    /// The odd field is being drawn; it flips every frame while interlaced.
    odd: bool,
    hint_pending: bool,
    vint_pending: bool,
}

impl Timing {
    /// Takes note of a VDP register write; only registers 0, 1, 10 and 12 matter here.
    pub fn set_register(&mut self, register: usize, value: u8) {
        match register {
            0 => self.hint_enabled = value & 0x10 != 0,
            1 => {
                self.vint_enabled = value & 0x20 != 0;
                self.display = value & 0x40 != 0;
            }
            // Not loaded into the counter now: the counter takes it when it next reloads.
            10 => self.line_reload = value,
            12 => {
                self.mode = if value & 0x01 != 0 {
                    HMode::H40
                } else {
                    HMode::H32
                };
                self.interlace = match value >> 1 & 3 {
                    1 => Interlace::Normal,
                    3 => Interlace::Double,
                    _ => Interlace::Off,
                };
            }
            _ => {}
        }
    }

    /// Moves time on by `cycles` 68000 cycles, raising whatever comes due on the way.
    pub fn advance(&mut self, cycles: u32) {
        self.advance_to(self.clock + u64::from(cycles) * CPU_DIVIDER);
    }

    /// Moves time on to master clock `target`, raising whatever comes due on the way.
    pub fn advance_to(&mut self, target: u64) {
        loop {
            let event = self.next_event();
            if event > target {
                break;
            }
            self.clock = event;
            self.apply(event);
        }
        self.clock = target;
    }

    /// 68000 cycles until the next point where an interrupt may become pending, at least 1.
    pub fn until_event(&self) -> u32 {
        let clocks = self.next_event() - self.clock;
        // A line is far below u32::MAX cycles.
        clocks.div_ceil(CPU_DIVIDER) as u32
    }

    /// The level the VDP asserts: a pending interrupt whose enable bit is set.
    pub fn pending(&self) -> Option<u8> {
        if self.vint_pending && self.vint_enabled {
            Some(VINT_LEVEL)
        } else if self.hint_pending && self.hint_enabled {
            Some(HINT_LEVEL)
        } else {
            None
        }
    }

    /// The 68000 acknowledged `level`, which clears that interrupt's pending flag.
    pub fn acknowledge(&mut self, level: u8) {
        match level {
            VINT_LEVEL => self.vint_pending = false,
            HINT_LEVEL => self.hint_pending = false,
            _ => {}
        }
    }

    /// The levels the VDP may raise, one bit per level.
    pub fn enabled(&self) -> u8 {
        let mut levels = 0;
        if self.vint_enabled {
            levels |= 1 << VINT_LEVEL;
        }
        if self.hint_enabled {
            levels |= 1 << HINT_LEVEL;
        }
        levels
    }

    /// The status register bits for the current position.
    pub fn status(&self) -> u16 {
        let position = self.position();
        let at = self.mode.line_positions();
        let mut bits = 0;
        if self.vint_pending {
            bits |= status::VINT;
        }
        if self.odd && self.interlace != Interlace::Off {
            bits |= status::ODD;
        }
        if !self.display || (ACTIVE_LINES..=LAST_VBLANK_LINE).contains(&self.v_line()) {
            bits |= status::VBLANK;
        }
        if !(at.hblank_end..at.hblank_start).contains(&position) {
            bits |= status::HBLANK;
        }
        bits
    }

    /// The HV counter: the V counter's low byte (reworked while interlaced) over the H
    /// counter.
    pub fn hv(&self) -> u16 {
        let v = self.v_counter();
        let v_byte = match self.interlace {
            Interlace::Off => v,
            Interlace::Normal => v & 0xFE | v >> 8 & 1,
            Interlace::Double => v << 1 | v >> 8 & 1,
        } & 0xFF;
        let h = self.mode.counter(self.mode.pixel_at(self.position())) >> 1;
        // Both halves are 8 bits wide.
        (v_byte << 8 | h) as u16
    }

    /// The 9-bit V counter: the line number, stepped where the line's V step falls, with
    /// its jump below the picture.
    pub fn v_counter(&self) -> u64 {
        let line = self.v_line();
        if line <= V_LAST_BEFORE_JUMP {
            line
        } else {
            line - V_LAST_BEFORE_JUMP - 1 + V_AFTER_JUMP
        }
    }

    /// Whether `line` is drawn with the display off or inside vertical blanking, where the
    /// VDP has its memory to itself.
    pub fn line_blanked(&self, line: u64) -> bool {
        !self.display || (ACTIVE_LINES..=LAST_VBLANK_LINE).contains(&line)
    }

    /// The horizontal mode.
    pub fn mode(&self) -> HMode {
        self.mode
    }

    /// Master clocks since power-on.
    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// The line the beam is on, 0 at the top of the picture.
    pub fn line(&self) -> u64 {
        self.clock / LINE_CLOCKS % LINES
    }

    /// Master clocks since the line began.
    pub fn position(&self) -> u64 {
        self.clock % LINE_CLOCKS
    }

    /// Frames that have ended: vertical interrupt points passed.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The first master clock after now where a line's pixels start going out: where its
    /// horizontal blanking ends.
    pub fn next_pixels(&self) -> u64 {
        let at = self.mode.line_positions().hblank_end;
        let position = self.position();
        let start = self.clock - position;
        if position < at {
            start + at
        } else {
            start + LINE_CLOCKS + at
        }
    }

    /// The line number the V counter shows: the beam's line, one further from the line's V
    /// step on.
    fn v_line(&self) -> u64 {
        let line = self.line();
        if self.position() >= self.mode.line_positions().v_step {
            (line + 1) % LINES
        } else {
            line
        }
    }

    /// The first point after now where something happens: the line counter's step on every
    /// line, and the vertical interrupt on line 224.
    fn next_event(&self) -> u64 {
        let at = self.mode.line_positions();
        let position = self.position();
        let start = self.clock - position;
        if self.line() == ACTIVE_LINES && position < at.vint {
            return start + at.vint;
        }
        if position < at.hblank_start {
            return start + at.hblank_start;
        }
        let next = start + LINE_CLOCKS;
        if next / LINE_CLOCKS % LINES == ACTIVE_LINES {
            next + at.vint
        } else {
            next + at.hblank_start
        }
    }

    fn apply(&mut self, event: u64) {
        let at = self.mode.line_positions();
        let line = event / LINE_CLOCKS % LINES;
        let position = event % LINE_CLOCKS;
        if position == at.vint && line == ACTIVE_LINES {
            self.vint_pending = true;
            self.frames += 1;
            self.odd = !self.odd;
        } else if position == at.hblank_start {
            if line > ACTIVE_LINES {
                self.line_counter = self.line_reload;
            } else if self.line_counter == 0 {
                self.line_counter = self.line_reload;
                self.hint_pending = true;
            } else {
                self.line_counter -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H32: Positions = HMode::H32_POSITIONS;
    const H40: Positions = HMode::H40_POSITIONS;

    /// 68000 cycles in `clocks` master clocks, rounded up.
    fn cycles(clocks: u64) -> u32 {
        clocks.div_ceil(CPU_DIVIDER) as u32
    }

    /// Advances to master clock `clock` or just past it.
    fn advance_to(timing: &mut Timing, clock: u64) {
        let cycles = cycles(clock - timing.clock());
        timing.advance(cycles);
    }

    /// Timing in H40 with the display on, as games set it up.
    fn h40() -> Timing {
        let mut timing = Timing::default();
        timing.set_register(12, 0x81);
        timing.set_register(1, 0x44);
        timing
    }

    #[test]
    fn test_vertical_interrupt_pends_once_per_frame_at_line_224() {
        let mut timing = Timing::default();
        timing.set_register(1, 0x20);
        let point = ACTIVE_LINES * LINE_CLOCKS + H32.vint;
        advance_to(&mut timing, point - CPU_DIVIDER);
        assert_eq!((timing.pending(), timing.frames()), (None, 0));
        advance_to(&mut timing, point);
        assert_eq!((timing.pending(), timing.frames()), (Some(VINT_LEVEL), 1));
        timing.acknowledge(VINT_LEVEL);
        assert_eq!(timing.pending(), None);
        advance_to(&mut timing, point + 10 * FRAME_CLOCKS);
        assert_eq!(timing.frames(), 11);
    }

    #[test]
    fn test_pending_vertical_interrupt_asserts_once_enabled() {
        let mut timing = Timing::default();
        advance_to(&mut timing, ACTIVE_LINES * LINE_CLOCKS + H32.vint);
        assert_eq!(timing.pending(), None);
        assert_ne!(timing.status() & status::VINT, 0);
        timing.set_register(1, 0x20);
        assert_eq!(timing.pending(), Some(VINT_LEVEL));
    }

    #[test]
    fn test_horizontal_interrupt_comes_every_reload_plus_one_lines() {
        let mut timing = Timing::default();
        timing.set_register(0, 0x10);
        timing.set_register(10, 3);
        let mut lines = Vec::new();
        for line in 0..LINES {
            advance_to(&mut timing, line * LINE_CLOCKS + H32.hblank_start);
            if timing.pending() == Some(HINT_LEVEL) {
                lines.push(line);
                timing.acknowledge(HINT_LEVEL);
            }
        }
        // The counter starts at 0, so line 0 fires and loads 3; then every fourth line,
        // until the picture ends and the counter reloads on every line below it.
        let expected: Vec<u64> = (0..=ACTIVE_LINES).step_by(4).collect();
        assert_eq!(lines, expected);
    }

    #[test]
    fn test_line_counter_reloads_below_the_picture_not_on_write() {
        let mut timing = Timing::default();
        timing.set_register(0, 0x10);
        timing.set_register(10, 0xFF);
        advance_to(&mut timing, H32.hblank_start);
        timing.acknowledge(HINT_LEVEL);
        // The counter now holds 0xFF; a new register value waits for the next reload.
        timing.set_register(10, 1);
        advance_to(&mut timing, FRAME_CLOCKS - 1);
        assert_eq!(timing.pending(), None);
        advance_to(&mut timing, FRAME_CLOCKS + H32.hblank_start);
        assert_eq!(timing.pending(), None);
        advance_to(&mut timing, FRAME_CLOCKS + LINE_CLOCKS + H32.hblank_start);
        assert_eq!(timing.pending(), Some(HINT_LEVEL));
    }

    #[test]
    fn test_until_event_reaches_the_next_line_counter_step() {
        let mut timing = Timing::default();
        let wait = timing.until_event();
        assert_eq!(wait, cycles(H32.hblank_start));
        timing.advance(wait - 1);
        assert_eq!(timing.until_event(), 1);
        timing.advance(1);
        assert!(timing.clock() >= H32.hblank_start);
        assert_eq!(
            u64::from(timing.until_event()),
            (LINE_CLOCKS + H32.hblank_start - timing.clock()).div_ceil(CPU_DIVIDER)
        );
    }

    #[test]
    fn test_status_blanking_flags_follow_the_beam() {
        let mut timing = h40();
        advance_to(&mut timing, 10 * LINE_CLOCKS + 0x50 * 16);
        assert_eq!(timing.status(), 0);
        advance_to(&mut timing, 10 * LINE_CLOCKS + H40.hblank_start);
        assert_eq!(timing.status(), status::HBLANK);
        advance_to(&mut timing, 230 * LINE_CLOCKS + 0x50 * 16);
        assert_eq!(timing.status(), status::VBLANK | status::VINT);
        advance_to(&mut timing, (LINES - 1) * LINE_CLOCKS + 0x50 * 16);
        assert_eq!(timing.status() & status::VBLANK, 0);
    }

    #[test]
    fn test_status_reads_vblank_while_the_display_is_off() {
        let mut timing = h40();
        advance_to(&mut timing, 10 * LINE_CLOCKS + 0x50 * 16);
        timing.set_register(1, 0x04);
        assert_eq!(timing.status(), status::VBLANK);
        assert!(timing.line_blanked(10));
    }

    #[test]
    fn test_line_positions_match_the_h_counter_transitions() {
        assert_eq!(
            H40,
            Positions {
                vint: 16,
                v_step: 0xA5 * 16,
                hblank_start: 0xB3 * 16 + 14 * 2,
                hblank_end: 0x06 * 16,
            }
        );
        assert_eq!(
            H32,
            Positions {
                vint: 20,
                v_step: 0x85 * 20,
                hblank_start: 0x93 * 20,
                hblank_end: 0x05 * 20,
            }
        );
    }

    /// Every H counter value a line shows, in order, with how long each lasts.
    fn h_sequence(mode: HMode) -> Vec<(u16, u64)> {
        let mut timing = Timing::default();
        let register = if mode == HMode::H40 { 0x81 } else { 0 };
        timing.set_register(12, register);
        let mut sequence: Vec<(u16, u64)> = Vec::new();
        for _ in 0..LINE_CLOCKS {
            let h = timing.hv() & 0xFF;
            match sequence.last_mut() {
                Some((last, clocks)) if *last == h => *clocks += 1,
                _ => sequence.push((h, 1)),
            }
            timing.clock += 1;
        }
        sequence
    }

    #[test]
    fn test_h_counter_skips_its_gap_and_fills_the_line() {
        let h32 = h_sequence(HMode::H32);
        let values: Vec<u16> = h32.iter().map(|&(h, _)| h).collect();
        let expected: Vec<u16> = (0x00..=0x93).chain(0xE9..=0xFF).collect();
        assert_eq!(values, expected);
        assert!(h32.iter().all(|&(_, clocks)| clocks == 20));

        // H40 jumps from the first pixel of a pair to the second, so 0xB6 and 0xE4 last one
        // pixel each; the slow pixels make 0xAC-0xB5 and 0xE5-0xE8 last 20 clocks.
        let h40 = h_sequence(HMode::H40);
        let values: Vec<u16> = h40.iter().map(|&(h, _)| h).collect();
        let expected: Vec<u16> = (0x00..=0xB6).chain(0xE4..=0xFF).collect();
        assert_eq!(values, expected);
        for &(h, clocks) in &h40 {
            let expected = match h {
                0xB6 | 0xE4 => 10,
                0xAC..=0xB5 | 0xE5..=0xE8 => 20,
                _ => 16,
            };
            assert_eq!(clocks, expected, "H counter 0x{h:02X}");
        }
    }

    #[test]
    fn test_v_counter_jumps_below_the_picture() {
        let mut timing = h40();
        advance_to(&mut timing, 0xEA * LINE_CLOCKS);
        assert_eq!(timing.hv() >> 8, 0xEA);
        advance_to(&mut timing, 0xEA * LINE_CLOCKS + H40.v_step);
        assert_eq!(timing.v_counter(), 0x1E5);
        assert_eq!(timing.hv() >> 8, 0xE5);
        advance_to(&mut timing, (LINES - 1) * LINE_CLOCKS);
        assert_eq!(timing.v_counter(), 0x1FF);
        advance_to(&mut timing, (LINES - 1) * LINE_CLOCKS + H40.v_step);
        assert_eq!(timing.v_counter(), 0);
    }

    #[test]
    fn test_interlaced_frames_flip_the_odd_flag() {
        let mut timing = h40();
        timing.set_register(12, 0x87);
        advance_to(&mut timing, ACTIVE_LINES * LINE_CLOCKS + H40.vint);
        assert_ne!(timing.status() & status::ODD, 0);
        advance_to(
            &mut timing,
            FRAME_CLOCKS + ACTIVE_LINES * LINE_CLOCKS + H40.vint,
        );
        assert_eq!(timing.status() & status::ODD, 0);
        // Double interlace shows the V counter shifted, with bit 8 in bit 0.
        advance_to(&mut timing, 2 * FRAME_CLOCKS + 0x90 * LINE_CLOCKS);
        assert_eq!(timing.hv() >> 8, 0x90 << 1 & 0xFF);
    }

    #[test]
    fn test_next_pixels_is_the_next_end_of_horizontal_blanking() {
        let mut timing = h40();
        assert_eq!(timing.next_pixels(), 96);
        timing.advance_to(96);
        assert_eq!(timing.next_pixels(), LINE_CLOCKS + 96);
        assert_eq!(Timing::default().next_pixels(), 100);
    }

    #[test]
    fn test_frame_is_896040_master_clocks() {
        assert_eq!(FRAME_CLOCKS, 896_040);
    }
}
