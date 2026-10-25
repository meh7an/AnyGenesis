//! The runtime recompiled code links against: the 68000 memory map, the I/O chip and pad,
//! the Z80 and TMSS stand-ins, the `ag_*` externs, and the loop that runs translated code
//! with the IR interpreter as the fallback for addresses no function covers.
//!
//! The VDP and everything that keeps time sit behind [`Hardware`]; [`StubVdp`] answers until
//! the real one exists, keeping NTSC time through [`timing`]. The scheduler delivers its
//! interrupts at the check points translated code calls. The translated program's
//! `ag_table` comes in as a slice: only the crate that links the generated C names its
//! symbols.

mod bus;
mod externs;
pub mod hardware;
pub mod io;
pub mod log;
mod machine;
pub mod timing;
pub mod z80;

pub use externs::{
    ag_check, ag_lookup, ag_read8, ag_read16, ag_read32, ag_reset_devices, ag_stop, ag_write8,
    ag_write16, ag_write32,
};
pub use hardware::{Hardware, Memory, StubVdp};
pub use io::Buttons;
pub use log::{Access, AccessKind, Log, Miss};
pub use machine::{Exit, Options, RUN_STACK, Runtime, UNWIND};
