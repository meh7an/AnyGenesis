//! Primitives shared by every AnyGenesis crate: addresses, sizes, the bus trait and the
//! marker of ROM-derived files.

mod address;
mod big_endian;
mod bus;
mod marker;
mod size;

pub use address::Address;
pub use big_endian::{BoundsError, read_sized_be, read_u8, read_u16_be, read_u32_be};
pub use bus::Bus;
pub use marker::{ROM_MARKER, c_marker_line};
pub use size::Size;
