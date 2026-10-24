//! Primitives shared by every AnyGenesis crate: addresses, sizes and the bus trait.

mod address;
mod big_endian;
mod bus;
mod size;

pub use address::Address;
pub use big_endian::{BoundsError, read_sized_be, read_u8, read_u16_be, read_u32_be};
pub use bus::Bus;
pub use size::Size;
