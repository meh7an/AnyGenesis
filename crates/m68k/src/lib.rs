//! 68000 instruction model and decoder.

mod decode;
mod ea;
mod instruction;

pub use decode::{DecodeError, decode, decode_at};
pub use ea::{Ea, EaMode, IndexRegister, IndexSize, Register};
pub use instruction::{Condition, Instruction, MAX_WORDS, Mnemonic, Operand, RegisterList};
