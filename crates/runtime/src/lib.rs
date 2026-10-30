//! The runtime recompiled code links against: the 68000 memory map, the I/O chip and pad,
//! the Z80 side with its own Z80, TMSS, the `ag_*` externs, and the loop that runs translated code,
//! with the program's stepped code for addresses no function covers. With the default
//! `interpreter` feature the IR interpreter is the last fallback, and it can keep a profile
//! of what it runs ([`profile`], the build's profile run); without it (games) a fetch from
//! an address with no native code ends the run.
//!
//! The VDP and everything that keeps time sit behind [`Hardware`]: [`Vdp`] with its ports,
//! memories and DMA, keeping NTSC time through [`timing`]. The scheduler delivers its
//! interrupts at the check points translated code calls. The translated program's
//! `ag_table`, `ag_steps` and `ag_interrupt` come in from the caller: only the crate that
//! links the generated C names its symbols.

mod bus;
mod externs;
pub mod hardware;
pub mod io;
pub mod log;
mod machine;
#[cfg(feature = "interpreter")]
pub mod profile;
pub mod refresh;
pub mod sound;
pub mod timing;
pub mod vdp;
pub mod z80;

pub use externs::{
    ag_check, ag_deliver, ag_lookup, ag_read8, ag_read16, ag_read32, ag_reset_devices,
    ag_step_begin, ag_step_check, ag_step_end, ag_step_enter, ag_step_late, ag_step_lookup,
    ag_stop, ag_write8, ag_write16, ag_write32,
};
pub use hardware::{Hardware, Memory};
pub use io::Buttons;
pub use log::{Access, AccessKind, Log, Miss};
pub use machine::{Exit, InterruptStop, Options, RUN_STACK, Runtime, UNWIND};
pub use sound::{Chip, Cpu, SoundCounts, SoundWrite};
pub use vdp::Vdp;

/// Whether this build of the runtime has the IR interpreter (the `interpreter` feature).
pub fn has_interpreter() -> bool {
    cfg!(feature = "interpreter")
}
