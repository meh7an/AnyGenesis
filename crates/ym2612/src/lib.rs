//! The YM2612 (OPN2), the Genesis's FM sound chip, in two halves:
//! - [`Control`], what either CPU sees: the address latches, the busy flag, timers A and B
//!   and the status register. The emulation keeps it, as timers and status change what a
//!   game does.
//! - [`Ym2612`], the sound: the register file, the DAC and panning, one stereo [`Output`]
//!   per output period of 144 input clocks. It plays outside the emulation from the writes
//!   the game made, so sound never changes a run.
//!
//! FM synthesis (operators, envelopes, LFO) is not built yet: FM channels put out 0.
//!
//! Written from documentation, no other emulator's code; ymfm (BSD-3, pinned in
//! vendor/ymfm/UPSTREAM.md) settles what the documents leave open and is the tests'
//! oracle, read as a reference only.

mod chip;
mod control;
mod registers;
mod timer;

pub use chip::{CHANNELS, Output, Ym2612};
pub use control::{BUSY_CLOCKS, Control, STATUS_BUSY, Written};
pub use registers::{
    DAC_DATA, DAC_ENABLE, PAN, TEST, TIMER_A_HIGH, TIMER_A_LOW, TIMER_B, TIMER_CONTROL,
};
pub use timer::{STATUS_TIMER_A, STATUS_TIMER_B};

/// Master clocks per input clock: the chip runs at the 68000's clock.
pub const MASTER_DIVIDER: u64 = 7;

/// Input clocks per output period: 6 input clocks per internal cycle, 24 operator slots.
pub const PERIOD_CLOCKS: u64 = 144;

/// Master clocks per output period, the grid the timers count on from power-on.
pub const SAMPLE_CLOCKS: u64 = PERIOD_CLOCKS * MASTER_DIVIDER;
