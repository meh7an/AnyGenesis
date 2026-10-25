//! 68000 instruction model, decoder and formatter.

mod decode;
mod ea;
mod format;
mod instruction;

pub use decode::{DecodeError, decode, decode_at};
pub use ea::{Ea, EaMode, IndexRegister, IndexSize, Register};
pub use format::{Dialect, FormatOptions, Formatted, Labels, LocationLabels};
pub use instruction::{Condition, Instruction, MAX_WORDS, Mnemonic, Operand, RegisterList};
