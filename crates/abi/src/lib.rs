//! The C ABI between generated code and the runtime, defined once.
//!
//! The state struct, the function table entry and the extern functions generated code calls
//! are declared here in Rust, and [`header`] prints the matching C header from the same
//! declarations. The header asserts every field offset and struct size Rust computed, so a
//! header from a different definition fails to compile instead of drifting.

mod header;

use std::ffi::c_void;

pub use header::header;

/// A Rust type with a C spelling.
pub trait CType {
    /// Returns the C declaration of `name` with this type, as `uint32_t name`.
    fn declare(name: &str) -> String;
}

macro_rules! scalar_ctype {
    ($($rust:ty => $c:literal),* $(,)?) => {
        $(
            impl CType for $rust {
                fn declare(name: &str) -> String {
                    format!(concat!($c, " {}"), name)
                }
            }
        )*
    };
}

scalar_ctype! {
    u8 => "uint8_t",
    u16 => "uint16_t",
    u32 => "uint32_t",
    i32 => "int32_t",
}

impl CType for () {
    fn declare(name: &str) -> String {
        format!("void {name}")
    }
}

impl CType for *const u8 {
    fn declare(name: &str) -> String {
        format!("const uint8_t *{name}")
    }
}

impl CType for *mut c_void {
    fn declare(name: &str) -> String {
        format!("void *{name}")
    }
}

impl CType for *mut AgState {
    fn declare(name: &str) -> String {
        format!("AgState *{name}")
    }
}

impl CType for AgFn {
    fn declare(name: &str) -> String {
        format!("AgFn *{name}")
    }
}

impl<T: CType, const N: usize> CType for [T; N] {
    fn declare(name: &str) -> String {
        T::declare(&format!("{name}[{N}]"))
    }
}

/// One field of a struct shared with C.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    /// The byte offset Rust gave the field.
    pub offset: usize,
    pub declare: fn(&str) -> String,
    /// The doc comment, one line per `\n`.
    pub doc: &'static str,
}

/// A struct shared with C, as the header prints it.
#[derive(Debug, Clone, Copy)]
pub struct Struct {
    pub name: &'static str,
    pub size: usize,
    pub fields: &'static [Field],
    /// The doc comment, one line per `\n`.
    pub doc: &'static str,
}

/// Declares a `#[repr(C)]` struct together with its [`Struct`] description.
macro_rules! c_struct {
    (
        $(#[doc = $doc:literal])+
        pub struct $name:ident as $table:ident {
            $($(#[doc = $field_doc:literal])+ pub $field:ident: $ty:ty,)*
        }
    ) => {
        $(#[doc = $doc])+
        #[repr(C)]
        #[derive(Debug)]
        pub struct $name {
            $($(#[doc = $field_doc])+ pub $field: $ty,)*
        }

        #[doc = concat!("The C description of [`", stringify!($name), "`].")]
        pub const $table: Struct = Struct {
            name: stringify!($name),
            size: std::mem::size_of::<$name>(),
            fields: &[$(Field {
                name: stringify!($field),
                offset: std::mem::offset_of!($name, $field),
                declare: <$ty as CType>::declare,
                doc: concat!($($field_doc, "\n"),+),
            },)*],
            doc: concat!($($doc, "\n"),+),
        };
    };
}

/// Bytes of work RAM, mirrored over 0xE00000-0xFFFFFF.
pub const RAM_SIZE: usize = 0x1_0000;

/// The lowest work RAM address; RAM and its mirrors run to the top of the address space.
pub const RAM_START: u32 = 0xE0_0000;

const ADDRESS_MASK: u32 = 0xFF_FFFF;

/// Where a whole access goes: the classification ag_runtime.h's inline fast path makes,
/// defined once for the emitter and every Rust bus that must agree with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// Every byte lies in 0xE00000-0xFFFFFF; byte indexes wrap at 64 KiB.
    Ram,
    /// Every byte lies inside the ROM image.
    Rom,
    /// Anything else, including accesses that straddle a boundary: the runtime's bus.
    Bus,
}

impl Region {
    /// Classifies an access of `bytes` bytes at the 24-bit `address`, given the ROM size.
    pub fn of(address: u32, bytes: u32, rom_size: u32) -> Self {
        if address >= RAM_START && address <= ADDRESS_MASK - (bytes - 1) {
            Self::Ram
        } else if address + bytes <= rom_size {
            Self::Rom
        } else {
            Self::Bus
        }
    }
}

/// Returns the index into [`AgState::ram`] of a work RAM address.
pub fn ram_index(address: u32) -> usize {
    (address as usize) & (RAM_SIZE - 1)
}

c_struct! {
    /// The machine state translated code runs on.
    pub struct AgState as STATE {
        /// The ROM image, read-only.
        pub rom: *const u8,
        /// The runtime's own context, opaque to generated code.
        pub runtime: *mut c_void,
        /// D0-D7.
        pub d: [u32; 8],
        /// A0-A7; A7 is the active stack pointer.
        pub a: [u32; 8],
        /// USP in supervisor mode, SSP in user mode.
        pub inactive_sp: u32,
        /// The next instruction, for the runtime; generated code passes addresses instead.
        pub pc: u32,
        /// Cycles left until a check point calls the hook (at 0 or below).
        pub cycles: i32,
        /// Bytes in `rom`.
        pub rom_size: u32,
        /// T, S and the interrupt mask in their status register bits; the flag bits stay 0.
        pub sr: u16,
        /// The X flag, 0 or 1.
        pub x: u8,
        /// The N flag, 0 or 1.
        pub n: u8,
        /// The Z flag, 0 or 1.
        pub z: u8,
        /// The V flag, 0 or 1.
        pub v: u8,
        /// The C flag, 0 or 1.
        pub c: u8,
        /// Work RAM in 68000 byte order.
        pub ram: [u8; RAM_SIZE],
    }
}

/// A translated function: called with the 68000 address to start at (its entry or one of
/// its resume points), it returns the address execution continues at.
pub type AgFn = unsafe extern "C" fn(state: *mut AgState, pc: u32) -> u32;

/// Returns the C typedef of [`AgFn`].
pub fn function_typedef() -> String {
    let params = [<*mut AgState>::declare("state"), u32::declare("pc")];
    format!(
        "typedef {};",
        u32::declare(&format!("AgFn({})", params.join(", ")))
    )
}

c_struct! {
    /// One row of the address table: the function that runs `address`.
    pub struct AgEntry as ENTRY {
        /// A function entry or a resume point inside the function.
        pub address: u32,
        /// The function to call with `address`.
        pub function: AgFn,
    }
}

/// An extern function the runtime implements for generated code.
#[derive(Debug, Clone, Copy)]
pub struct Extern {
    pub name: &'static str,
    /// Returns the C prototype, without the final semicolon.
    pub prototype: fn() -> String,
    /// The doc comment, one line per `\n`.
    pub doc: &'static str,
}

/// Expands to the return type of an extern, `()` when none is written.
macro_rules! return_type {
    () => {
        ()
    };
    ($ret:ty) => {
        $ret
    };
}

/// Declares the extern functions: a Rust signature type per function in [`signatures`] and
/// the [`EXTERNS`] table the header prints.
macro_rules! c_externs {
    ($(
        $(#[doc = $doc:literal])+
        fn $name:ident($($param:ident: $ty:ty),*) $(-> $ret:ty)?;
    )*) => {
        /// The Rust type of each extern function, for the runtime's implementations.
        #[allow(non_camel_case_types)]
        pub mod signatures {
            use super::*;

            $(
                $(#[doc = $doc])+
                pub type $name = unsafe extern "C" fn($($param: $ty),*) $(-> $ret)?;
            )*
        }

        /// Every extern function generated code may call, in header order.
        pub const EXTERNS: &[Extern] = &[$(Extern {
            name: stringify!($name),
            prototype: || {
                let params = [$(<$ty as CType>::declare(stringify!($param))),*];
                <return_type!($($ret)?) as CType>::declare(&format!(
                    "{}({})",
                    stringify!($name),
                    params.join(", ")
                ))
            },
            doc: concat!($($doc, "\n"),+),
        },)*];
    };
}

c_externs! {
    /// Reads a byte outside the inline fast path; `address` has 24 bits.
    fn ag_read8(state: *mut AgState, address: u32) -> u32;
    /// Reads a word outside the inline fast path.
    fn ag_read16(state: *mut AgState, address: u32) -> u32;
    /// Reads a long outside the inline fast path, as two words, high word first.
    fn ag_read32(state: *mut AgState, address: u32) -> u32;
    /// Writes the low byte of `value` outside the inline fast path.
    fn ag_write8(state: *mut AgState, address: u32, value: u32);
    /// Writes the low word of `value` outside the inline fast path.
    fn ag_write16(state: *mut AgState, address: u32, value: u32);
    /// Writes a long outside the inline fast path, as two words, high word first.
    fn ag_write32(state: *mut AgState, address: u32, value: u32);
    /// Returns the function that runs `address`: a translated function, or an interpreter
    /// entry for addresses no function covers. Never null.
    fn ag_lookup(state: *mut AgState, address: u32) -> AgFn;
    /// STOP: waits for an interrupt, runs it and returns where execution continues
    /// (`resume` unless the handler changed the stacked PC).
    fn ag_stop(state: *mut AgState, resume: u32) -> u32;
    /// The check-point hook, called when `cycles` ran out at `pc`: delivers due interrupts
    /// and returns where execution continues (`pc` unless a handler changed the stacked PC).
    fn ag_check(state: *mut AgState, pc: u32) -> u32;
    /// The slow path of a check point whose block an event falls inside: interprets from the
    /// block start `pc` to the next table row and returns it with `AG_HYBRID_ROW` set, or
    /// returns any other address execution goes to instead.
    fn ag_hybrid(state: *mut AgState, pc: u32) -> u32;
    /// The slow path of a check after an instruction that may reach a device, once its
    /// access left the budget short of the rest of the block: gives back the `ahead` cycles
    /// charged for instructions not yet run, applies the latch rule to the instruction with
    /// its `latch` point, interprets from `pc` to the next table row and returns it with
    /// `AG_HYBRID_ROW` set, or returns any other address execution goes to instead.
    fn ag_resume(state: *mut AgState, pc: u32, ahead: u32, latch: u32) -> u32;
    /// After a write to SR, at the instruction boundary `pc`: delivers the interrupts the new
    /// mask lets through and returns where execution continues (`pc` unless a handler
    /// changed the stacked PC). Never ends the run.
    fn ag_deliver(state: *mut AgState, pc: u32) -> u32;
    /// RESET: asserts the reset line of the external devices.
    fn ag_reset_devices(state: *mut AgState);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offset(name: &str) -> Option<usize> {
        STATE
            .fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.offset)
    }

    #[test]
    fn test_state_layout_packs_flags_before_ram() {
        assert_eq!(offset("c").map(|c| c + 1), offset("ram"));
        assert_eq!(STATE.size % std::mem::align_of::<AgState>(), 0);
    }

    #[test]
    fn test_regions_keep_long_accesses_from_wrapping_out_of_ram() {
        assert_eq!(Region::of(0xFF_FFFC, 4, 0x1000), Region::Ram);
        assert_eq!(Region::of(0xFF_FFFE, 4, 0x1000), Region::Bus);
        assert_eq!(Region::of(0xE0_FFFE, 4, 0x1000), Region::Ram);
        assert_eq!(Region::of(0x00_0FFE, 2, 0x1000), Region::Rom);
        assert_eq!(Region::of(0x00_0FFE, 4, 0x1000), Region::Bus);
        assert_eq!(Region::of(0xA1_0000, 1, 0x1000), Region::Bus);
        assert_eq!(ram_index(0xE1_2345), 0x2345);
    }

    #[test]
    fn test_extern_prototypes_spell_c_types() {
        let prototypes: Vec<String> = EXTERNS.iter().map(|item| (item.prototype)()).collect();
        let expected = [
            "uint32_t ag_read8(AgState *state, uint32_t address)",
            "AgFn *ag_lookup(AgState *state, uint32_t address)",
            "void ag_reset_devices(AgState *state)",
        ];
        for prototype in expected {
            assert!(
                prototypes.iter().any(|item| item == prototype),
                "{prototype}"
            );
        }
    }

    #[test]
    fn test_declarations_put_array_lengths_after_the_name() {
        assert_eq!(<[u32; 8]>::declare("d"), "uint32_t d[8]");
        assert_eq!(
            function_typedef(),
            "typedef uint32_t AgFn(AgState *state, uint32_t pc);"
        );
    }
}
