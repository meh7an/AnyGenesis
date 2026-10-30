//! Code patches (M6.2): C functions run before the instructions at their addresses, RAM
//! patches written at reset or at every VBlank, and runtime enhancement flags. Unlike ROM
//! patches they do not change the image; the build compiles the C with the game and embeds
//! the rest for the player.

use std::{collections::BTreeMap, fmt};

use anygenesis_rom::{MAX_ROM_SIZE, Sha1Digest};
use serde::Deserialize;

use crate::{Patch, PatchError};

/// Where work RAM starts in the 68000 map; RAM patches stay inside its mirror at the top.
pub const RAM_START: u32 = 0xFF_0000;
const RAM_END: u32 = 0x100_0000;

/// One C file of a pack, with the name the pack gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub text: Vec<u8>,
}

/// A C function `uint32_t function(AgState *s, uint32_t pc)` run before each execution of
/// the instruction at `address`: a `[[hook]]` (0 cycles) or a `[[replace]]`, whose cycles
/// are charged when it returns another address than its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodePatch {
    pub address: u32,
    pub function: String,
    /// Its file, an index into [`CodePatches::sources`].
    pub source: usize,
    pub cycles: u32,
}

/// When a RAM patch is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum When {
    /// Once, before the first instruction.
    Reset,
    /// At every frame's VBlank point, whether the game enabled the interrupt or not.
    Vblank,
}

impl fmt::Display for When {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Reset => "reset",
            Self::Vblank => "vblank",
        })
    }
}

/// Bytes written to work RAM at `address` (0xFF0000-0xFFFFFF).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RamPatch {
    pub address: u32,
    pub bytes: Vec<u8>,
    pub when: When,
}

/// The widest `widescreen` margin, the runtime's limit: wider, sprites left of the picture
/// would wrap round into the right margin.
pub const MAX_WIDESCREEN: u8 = 62;

/// The runtime enhancement flags; `None` leaves the hardware behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flags {
    /// `false` draws every sprite on a line; the overflow status bit keeps the hardware rule.
    pub sprite_limit: Option<bool>,
    /// Pixels drawn left and right of the console's picture (M6.3), even, at most
    /// [`MAX_WIDESCREEN`].
    pub widescreen: Option<u8>,
}

/// A flag's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagValue {
    Switch(bool),
    Pixels(u8),
}

impl FlagValue {
    /// The one byte the program embeds.
    pub fn byte(self) -> u8 {
        match self {
            Self::Switch(on) => u8::from(on),
            Self::Pixels(pixels) => pixels,
        }
    }
}

impl fmt::Display for FlagValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Switch(on) => write!(f, "{on}"),
            Self::Pixels(pixels) => write!(f, "{pixels}"),
        }
    }
}

impl Flags {
    /// Each flag that is set, by name.
    pub fn set(&self) -> Vec<(&'static str, FlagValue)> {
        let sprite_limit = self
            .sprite_limit
            .map(|on| ("sprite_limit", FlagValue::Switch(on)));
        let widescreen = self
            .widescreen
            .map(|pixels| ("widescreen", FlagValue::Pixels(pixels)));
        sprite_limit.into_iter().chain(widescreen).collect()
    }

    /// Fails unless every flag set has a value the runtime takes.
    pub(crate) fn check(&self) -> Result<(), String> {
        match self.widescreen {
            Some(pixels) if pixels > MAX_WIDESCREEN || !pixels.is_multiple_of(2) => Err(format!(
                "`widescreen = {pixels}` is not an even number of pixels from 0 to \
                 {MAX_WIDESCREEN}"
            )),
            _ => Ok(()),
        }
    }

    /// Adds `other`'s flags to these.
    fn add(&mut self, other: Self) -> Result<(), PatchError> {
        let clash = |name: &str| PatchError::Clash {
            what: format!("two packs set `{name}`"),
        };
        if let Some(value) = other.sprite_limit
            && self.sprite_limit.replace(value).is_some()
        {
            return Err(clash("sprite_limit"));
        }
        if let Some(value) = other.widescreen
            && self.widescreen.replace(value).is_some()
        {
            return Err(clash("widescreen"));
        }
        Ok(())
    }
}

/// The code patches of the packs a build uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodePatches {
    pub sources: Vec<Source>,
    /// Ascending by address, one per address.
    pub patches: Vec<CodePatch>,
    /// In pack order, applied in that order.
    pub ram: Vec<RamPatch>,
    pub flags: Flags,
}

impl CodePatches {
    /// The code patches of `patches` in order; patch files have none.
    ///
    /// # Errors
    /// [`PatchError::Clash`] when two packs patch one address or set one flag.
    pub fn of(patches: &[Patch]) -> Result<Self, PatchError> {
        let mut all = Self::default();
        for patch in patches {
            if let Patch::Pack(pack) = patch {
                all.add(&pack.code)?;
            }
        }
        Ok(all)
    }

    pub fn is_empty(&self) -> bool {
        self.patches.is_empty() && self.ram.is_empty() && self.flags == Flags::default()
    }

    /// Adds `other`'s patches after these.
    pub(crate) fn add(&mut self, other: &Self) -> Result<(), PatchError> {
        let base = self.sources.len();
        self.sources.extend(other.sources.iter().cloned());
        let mut patches: BTreeMap<u32, CodePatch> = self
            .patches
            .drain(..)
            .map(|patch| (patch.address, patch))
            .collect();
        for patch in &other.patches {
            let patch = CodePatch {
                source: patch.source + base,
                ..patch.clone()
            };
            if patches.insert(patch.address, patch.clone()).is_some() {
                return Err(PatchError::Clash {
                    what: format!("two code patches at {:#08x}", patch.address),
                });
            }
        }
        self.patches = patches.into_values().collect();
        self.ram.extend(other.ram.iter().cloned());
        self.flags.add(other.flags)
    }

    /// The SHA-1 of everything here, in a fixed text form: what a build with these patches
    /// is cached under.
    pub fn hash(&self) -> Sha1Digest {
        let mut text = Vec::new();
        for (index, source) in self.sources.iter().enumerate() {
            text.extend(format!("source {index} {}\n", source.text.len()).bytes());
            text.extend(&source.text);
            text.push(b'\n');
        }
        for patch in &self.patches {
            text.extend(
                format!(
                    "code {:06x} {} {} {}\n",
                    patch.address, patch.function, patch.source, patch.cycles
                )
                .bytes(),
            );
        }
        for ram in &self.ram {
            text.extend(format!("ram {} {:06x} ", ram.when, ram.address).bytes());
            for byte in &ram.bytes {
                text.extend(format!("{byte:02x}").bytes());
            }
            text.push(b'\n');
        }
        for (name, value) in self.flags.set() {
            text.extend(format!("flag {name} {value}\n").bytes());
        }
        Sha1Digest::of(&text)
    }
}

/// Fails unless `address` can hold an instruction of a ROM.
pub(crate) fn check_code_address(address: u32) -> Result<(), String> {
    if !address.is_multiple_of(2) || address as usize >= MAX_ROM_SIZE {
        return Err(format!(
            "address {address:#x} is not an even ROM address below {MAX_ROM_SIZE:#x}"
        ));
    }
    Ok(())
}

/// Fails unless `len` bytes at `address` lie in work RAM.
pub(crate) fn check_ram_range(address: u32, len: usize) -> Result<(), String> {
    let end = u64::from(address) + len as u64;
    if address < RAM_START || end > u64::from(RAM_END) {
        return Err(format!(
            "{len} bytes at {address:#x} do not lie in work RAM ({RAM_START:#x}-{:#x})",
            RAM_END - 1
        ));
    }
    Ok(())
}

/// Fails unless `name` is a C identifier that cannot clash with a generated symbol: the
/// `ag_` names and the functions `f_XXXXXX` and `s_XXXXXX`.
pub(crate) fn check_function(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    let first = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_');
    let identifier = first && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    let generated = matches!(name.as_bytes(), [b'f' | b's', b'_', hex @ ..]
        if hex.len() == 6 && hex.iter().all(u8::is_ascii_hexdigit));
    let reserved = name.to_ascii_lowercase().starts_with("ag_") || generated;
    if !identifier || reserved {
        return Err(format!(
            "`function = {name:?}` is not a C identifier outside the generated names"
        ));
    }
    Ok(())
}
