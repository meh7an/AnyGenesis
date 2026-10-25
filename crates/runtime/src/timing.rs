//! NTSC video timing: where the beam is, and the two interrupts the VDP derives from it.
//!
//! Time is kept in master clocks, of which the 68000 gets one in seven, so lines and frames
//! stay exact (a line is 488 4/7 68000 cycles). A line starts where the H counter reads 0.
//! The vertical interrupt becomes pending on line 224, the first line below a 224-line
//! picture; that is where a frame ends. The horizontal interrupt comes from a line counter
//! that steps once per line while the picture is drawn and reloads from register 10 below
//! it.
//!
//! Sources (behaviour only): Kabuto's hardware notes as mirrored on Plutiedev (line length,
//! line count, H/V counter transitions, blanking flags, where the vertical interrupt flag
//! sets) and Charles MacDonald's VDP document and Sega Retro's interrupt page (line counter
//! reload). Positions inside a line are taken at the H40 rate, where the H counter steps
//! every 16 master clocks; H32 differs by a few master clocks.

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
    /// Vertical blanking.
    pub const VBLANK: u16 = 0x0008;
    /// Horizontal blanking.
    pub const HBLANK: u16 = 0x0004;
}

/// Master clocks per H counter step in H40.
const H_STEP: u64 = 16;
/// The vertical interrupt flag sets as the H counter goes from 0x00 to 0x01.
const VINT_OFFSET: u64 = H_STEP;
/// The V counter steps as the H counter goes from 0xA4 to 0xA5.
const V_STEP_OFFSET: u64 = 0xA5 * H_STEP;
/// Horizontal blanking starts as the H counter goes from 0xB2 to 0xB3...
const HBLANK_START: u64 = 0xB3 * H_STEP;
/// ...and ends as it goes from 0x05 to 0x06.
const HBLANK_END: u64 = 0x06 * H_STEP;
/// The line counter steps as horizontal blanking starts.
const LINE_COUNTER_OFFSET: u64 = HBLANK_START;
/// The last line (by the V counter's numbering) of vertical blanking: the flag clears as
/// the V counter reaches 0x1FF, the last line of the frame.
const LAST_VBLANK_LINE: u64 = LINES - 2;

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
    hint_pending: bool,
    vint_pending: bool,
}

impl Timing {
    /// Takes note of a VDP register write; only registers 0, 1 and 10 matter here.
    pub fn set_register(&mut self, register: usize, value: u8) {
        match register {
            0 => self.hint_enabled = value & 0x10 != 0,
            1 => self.vint_enabled = value & 0x20 != 0,
            // Not loaded into the counter now: the counter takes it when it next reloads.
            10 => self.line_reload = value,
            _ => {}
        }
    }

    /// Moves time on by `cycles` 68000 cycles, raising whatever comes due on the way.
    pub fn advance(&mut self, cycles: u32) {
        let target = self.clock + u64::from(cycles) * CPU_DIVIDER;
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
        let mut v_line = self.line();
        if position >= V_STEP_OFFSET {
            v_line = (v_line + 1) % LINES;
        }
        let mut bits = 0;
        if self.vint_pending {
            bits |= status::VINT;
        }
        if (ACTIVE_LINES..=LAST_VBLANK_LINE).contains(&v_line) {
            bits |= status::VBLANK;
        }
        if !(HBLANK_END..HBLANK_START).contains(&position) {
            bits |= status::HBLANK;
        }
        bits
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

    /// The first point after now where something happens: the line counter's step on every
    /// line, and the vertical interrupt on line 224.
    fn next_event(&self) -> u64 {
        let start = self.clock - self.position();
        let position = self.position();
        if self.line() == ACTIVE_LINES && position < VINT_OFFSET {
            return start + VINT_OFFSET;
        }
        if position < LINE_COUNTER_OFFSET {
            return start + LINE_COUNTER_OFFSET;
        }
        let next = start + LINE_CLOCKS;
        if next / LINE_CLOCKS % LINES == ACTIVE_LINES {
            next + VINT_OFFSET
        } else {
            next + LINE_COUNTER_OFFSET
        }
    }

    fn apply(&mut self, event: u64) {
        let line = event / LINE_CLOCKS % LINES;
        match event % LINE_CLOCKS {
            VINT_OFFSET => {
                self.vint_pending = true;
                self.frames += 1;
            }
            LINE_COUNTER_OFFSET if line <= ACTIVE_LINES => {
                if self.line_counter == 0 {
                    self.line_counter = self.line_reload;
                    self.hint_pending = true;
                } else {
                    self.line_counter -= 1;
                }
            }
            LINE_COUNTER_OFFSET => self.line_counter = self.line_reload,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 68000 cycles in `clocks` master clocks, rounded up.
    fn cycles(clocks: u64) -> u32 {
        clocks.div_ceil(CPU_DIVIDER) as u32
    }

    /// Advances to master clock `clock` or just past it.
    fn advance_to(timing: &mut Timing, clock: u64) {
        let cycles = cycles(clock - timing.clock());
        timing.advance(cycles);
    }

    #[test]
    fn test_vertical_interrupt_pends_once_per_frame_at_line_224() {
        let mut timing = Timing::default();
        timing.set_register(1, 0x20);
        let point = ACTIVE_LINES * LINE_CLOCKS + VINT_OFFSET;
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
        advance_to(&mut timing, ACTIVE_LINES * LINE_CLOCKS + VINT_OFFSET);
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
            advance_to(&mut timing, line * LINE_CLOCKS + LINE_COUNTER_OFFSET);
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
        advance_to(&mut timing, LINE_COUNTER_OFFSET);
        timing.acknowledge(HINT_LEVEL);
        // The counter now holds 0xFF; a new register value waits for the next reload.
        timing.set_register(10, 1);
        advance_to(&mut timing, FRAME_CLOCKS - 1);
        assert_eq!(timing.pending(), None);
        advance_to(&mut timing, FRAME_CLOCKS + LINE_COUNTER_OFFSET);
        assert_eq!(timing.pending(), None);
        advance_to(
            &mut timing,
            FRAME_CLOCKS + LINE_CLOCKS + LINE_COUNTER_OFFSET,
        );
        assert_eq!(timing.pending(), Some(HINT_LEVEL));
    }

    #[test]
    fn test_until_event_reaches_the_next_line_counter_step() {
        let mut timing = Timing::default();
        let wait = timing.until_event();
        assert_eq!(wait, cycles(LINE_COUNTER_OFFSET));
        timing.advance(wait - 1);
        assert_eq!(timing.until_event(), 1);
        timing.advance(1);
        assert!(timing.clock() >= LINE_COUNTER_OFFSET);
        assert_eq!(
            u64::from(timing.until_event()),
            (LINE_CLOCKS + LINE_COUNTER_OFFSET - timing.clock()).div_ceil(CPU_DIVIDER)
        );
    }

    #[test]
    fn test_status_blanking_flags_follow_the_beam() {
        let mut timing = Timing::default();
        advance_to(&mut timing, 10 * LINE_CLOCKS + 0x50 * H_STEP);
        assert_eq!(timing.status(), 0);
        advance_to(&mut timing, 10 * LINE_CLOCKS + HBLANK_START);
        assert_eq!(timing.status(), status::HBLANK);
        advance_to(&mut timing, 230 * LINE_CLOCKS + 0x50 * H_STEP);
        assert_eq!(timing.status(), status::VBLANK | status::VINT);
        advance_to(&mut timing, (LINES - 1) * LINE_CLOCKS + 0x50 * H_STEP);
        assert_eq!(timing.status() & status::VBLANK, 0);
    }

    #[test]
    fn test_frame_is_896040_master_clocks() {
        assert_eq!(FRAME_CLOCKS, 896_040);
    }
}
