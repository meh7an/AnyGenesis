//! What the runtime noticed while running: dispatcher misses, which are hint candidates, and
//! accesses the memory map does not expect. Entries are deduplicated with counts and kept in
//! memory, for tests to assert on and for the binary to print.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
};

use anygenesis_shared::Size;

/// Why an access was logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum AccessKind {
    /// Nothing answers at the address; reads return the open-bus value.
    Unmapped,
    /// A word access at an odd address, an address error on a real 68000.
    OddWord,
    /// A write to the cartridge ROM.
    RomWrite,
    /// Z80 RAM touched while the 68000 does not hold the Z80 bus.
    Z80WithoutBus,
    /// A word access to the Z80 window, which only takes bytes.
    Z80Word,
    /// An access that hangs a real console between the two CPUs: the 68000 reaching the
    /// VDP through the Z80's space, or the Z80's bank window pointing back at its own.
    Z80Hang,
    /// The VDP touched before TMSS was unlocked, which locks up a console that has TMSS.
    VdpLocked,
    /// A VDP block address that hangs a real console.
    VdpHang,
    /// A VDP port access with no defined effect: a data access whose command code has no
    /// target or points the other way (the 8-bit VRAM read among them), a write to the HV
    /// counter, a PSG read, or the ports past the PSG.
    VdpCommand,
    /// A VDP register past 23, or a mode the VDP does not build (mode 4, 128 KiB VRAM, a
    /// mixed horizontal resolution, shadow/highlight, double-resolution interlace, a
    /// prohibited plane size).
    VdpRegister,
    /// A DMA that reads outside ROM and work RAM, fills CRAM or VSRAM, or transfers through
    /// a code with no write target.
    VdpDma,
}

/// One kind of logged access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Access {
    pub kind: AccessKind,
    pub write: bool,
    pub size: Size,
    pub address: u32,
}

impl fmt::Display for AccessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unmapped => "unmapped",
            Self::OddWord => "odd word",
            Self::RomWrite => "ROM write",
            Self::Z80WithoutBus => "Z80 RAM without the bus",
            Self::Z80Word => "Z80 word access",
            Self::Z80Hang => "Z80 window hang",
            Self::VdpLocked => "VDP before TMSS",
            Self::VdpHang => "VDP hang address",
            Self::VdpCommand => "VDP access without effect",
            Self::VdpRegister => "VDP register or mode not built",
            Self::VdpDma => "VDP DMA not built",
        })
    }
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let direction = if self.write { "write" } else { "read" };
        let size = match self.size {
            Size::Byte => "b",
            Size::Word => "w",
            Size::Long => "l",
        };
        write!(f, "{}: {direction}.{size} ${:06X}", self.kind, self.address)
    }
}

/// How often an address without translated code reached the runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Miss {
    /// `ag_lookup` calls for the address.
    pub lookups: u64,
    /// Times the top-level loop continued at it after a return reached the top.
    pub resumes: u64,
    /// Instructions the interpreter ran from it before handing control back.
    pub instructions: u64,
}

/// Everything logged so far.
#[derive(Debug, Default)]
pub struct Log {
    accesses: HashMap<Access, u64>,
    misses: BTreeMap<u32, Miss>,
    unwinds: u64,
}

impl Log {
    pub(crate) fn access(&mut self, access: Access) {
        *self.accesses.entry(access).or_default() += 1;
    }

    /// Only the interpreter runs code a lookup missed; without it a miss ends the run.
    #[cfg(feature = "interpreter")]
    pub(crate) fn miss(&mut self, address: u32) -> &mut Miss {
        self.misses.entry(address).or_default()
    }

    pub(crate) fn unwind(&mut self) {
        self.unwinds += 1;
    }

    /// Logged accesses with their counts, by kind, then address, then reads before writes.
    pub fn accesses(&self) -> Vec<(Access, u64)> {
        let mut accesses: Vec<(Access, u64)> = self
            .accesses
            .iter()
            .map(|(&access, &count)| (access, count))
            .collect();
        accesses.sort_by_key(|(access, _)| {
            (
                access.kind,
                access.address,
                access.write,
                access.size.bytes(),
            )
        });
        accesses
    }

    /// Addresses without translated code that execution reached, ascending.
    pub fn misses(&self) -> &BTreeMap<u32, Miss> {
        &self.misses
    }

    /// Times the native-depth guard unwound the native stack to the top level.
    pub fn unwinds(&self) -> u64 {
        self.unwinds
    }
}
