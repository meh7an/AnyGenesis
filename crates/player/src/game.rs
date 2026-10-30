//! The game a program carries: what its `ag_game` table says, checked.

use std::{ffi::CStr, slice};

use anygenesis_abi::{
    AgEntry, AgExtra, AgFn, AgGame, AgInterrupt, EXTRA_FLAG, EXTRA_RAM_PATCH, GAME_ABI,
};
use anygenesis_rom::{Rom, Sha1Digest};
use anygenesis_runtime::{RamPatch, RamWhen};
use anyhow::{Context, Result, bail};

/// A translated game linked into the program, with the ROM image it was translated from.
#[derive(Debug, Clone, Copy)]
pub struct Game {
    /// The title from the ROM's header, for display.
    pub name: &'static str,
    /// The SHA-1 of the image, checked against the one the build recorded.
    pub sha1: Sha1Digest,
    /// The whole ROM image.
    pub image: &'static [u8],
    /// Every function entry and resume point with the function that runs it.
    pub table: &'static [AgEntry],
    /// The stepped function of every even ROM address, index address / 2.
    pub steps: &'static [Option<AgFn>],
    /// The checked entry into the stepped functions, `ag_step_entry`.
    pub step_entry: AgFn,
    /// The program's interrupt entry, `ag_interrupt`.
    pub interrupt: AgInterrupt,
    /// Entries of the functions reset and the exception vectors start, ascending.
    pub vector_handlers: &'static [u32],
    /// Entries of every analysed function, ascending.
    pub function_entries: &'static [u32],
    /// The RAM patches the program's patch packs write, in order (M6.2).
    pub ram_patches: &'static [RamPatch],
    /// Whether lines keep the hardware's sprite limits: off by the `sprite_limit` flag.
    pub sprite_limit: bool,
}

impl Game {
    /// Reads the game from the program's table, checking its layout version and that its
    /// image still has the SHA-1 the build recorded.
    ///
    /// # Errors
    /// A table from another layout version, a title that is not UTF-8, or an image whose
    /// SHA-1 differs from the recorded one.
    pub fn from_table(table: &'static AgGame) -> Result<Self> {
        if table.abi != GAME_ABI {
            bail!(
                "the game table has layout version {}, this player reads {GAME_ABI}: rebuild \
                 the game with this player",
                table.abi
            );
        }
        // SAFETY: the build wrote the table with each pointer to its count of elements (or
        // null with a count of 0) and the title NUL-terminated, all in the program's
        // read-only data, which lives as long as the program.
        let game = unsafe {
            Self {
                name: CStr::from_ptr(table.title)
                    .to_str()
                    .context("the game's title is not UTF-8")?,
                sha1: Sha1Digest::of(elements(table.image, table.image_size)),
                image: elements(table.image, table.image_size),
                table: elements(table.table, table.table_size),
                steps: elements(table.steps, table.steps_size),
                step_entry: table.step_entry,
                interrupt: table.interrupt,
                vector_handlers: elements(table.vector_handlers, table.vector_handler_count),
                function_entries: elements(table.function_entries, table.function_entry_count),
                ram_patches: &[],
                sprite_limit: true,
            }
        };
        // SAFETY: as above.
        let extras = unsafe { elements(table.extras, table.extra_count) };
        let (ram_patches, sprite_limit) = read_extras(extras)?;
        // Read once per program and kept for its life, as the rest of the table is.
        let game = Self {
            ram_patches: Box::leak(ram_patches.into_boxed_slice()),
            sprite_limit,
            ..game
        };
        if game.sha1.bytes() != &table.sha1 {
            let recorded: String = table
                .sha1
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            bail!(
                "the embedded ROM image has SHA-1 {}, but {} was built from {recorded}",
                game.sha1,
                game.name
            );
        }
        Ok(game)
    }

    /// The ROM the game was translated from, its image parsed, with the table checked
    /// against its reset PC.
    ///
    /// # Errors
    /// An image the ROM loader refuses, or a table that is not sorted or lacks the reset PC
    /// or a vector handler.
    pub fn rom(&self) -> Result<Rom> {
        let rom = Rom::from_bytes(self.image.to_vec()).context("loading the embedded image")?;
        self.check_table(rom.vectors().reset_pc() & 0xFF_FFFF)?;
        Ok(rom)
    }

    /// The entry of the function nearest below or at `address`: the one that most likely
    /// holds it, as functions are not ranges.
    pub fn function_at(&self, address: u32) -> Option<u32> {
        let after = self
            .function_entries
            .partition_point(|&entry| entry <= address);
        after
            .checked_sub(1)
            .map(|index| self.function_entries[index])
    }

    /// Returns the table row for `address`.
    pub fn row(&self, address: u32) -> Option<&AgEntry> {
        self.table
            .binary_search_by_key(&address, |entry| entry.address)
            .ok()
            .map(|index| &self.table[index])
    }

    /// Checks that the table is sorted, as the dispatcher's binary search needs, and holds
    /// `reset_pc` and every vector handler.
    ///
    /// # Errors
    /// Names the first row out of order or the first address without a row.
    pub fn check_table(&self, reset_pc: u32) -> Result<()> {
        if let Some(pair) = self
            .table
            .windows(2)
            .find(|pair| pair[0].address >= pair[1].address)
        {
            bail!(
                "ag_table is not sorted: ${:06X} before ${:06X}",
                pair[0].address,
                pair[1].address
            );
        }
        let handlers = self
            .vector_handlers
            .iter()
            .map(|&entry| ("vector handler", entry));
        for (what, address) in std::iter::once(("reset PC", reset_pc)).chain(handlers) {
            if self.row(address).is_none() {
                bail!("ag_table has no row for the {what} ${address:06X}");
            }
        }
        Ok(())
    }
}

/// The `count` elements from `start` on, none when `count` is 0 (`start` may then be null).
///
/// # Safety
/// Unless `count` is 0, `start` points to `count` initialised elements that stay unchanged
/// for the rest of the program.
/// The RAM patches and the sprite limit `extras` give; kinds this player does not know are
/// skipped.
fn read_extras(extras: &'static [AgExtra]) -> Result<(Vec<RamPatch>, bool)> {
    let mut patches = Vec::new();
    let mut sprite_limit = true;
    for extra in extras {
        // SAFETY: the build wrote each extra's name NUL-terminated and its data with its
        // size, in the program's read-only data.
        let (name, data) = unsafe {
            (
                CStr::from_ptr(extra.name).to_str().unwrap_or_default(),
                elements(extra.data, extra.size),
            )
        };
        match (extra.kind, name, data) {
            (EXTRA_RAM_PATCH, "reset" | "vblank", [high, middle, low, bytes @ ..]) => {
                patches.push(RamPatch {
                    address: u32::from_be_bytes([0, *high, *middle, *low]),
                    bytes: bytes.to_vec(),
                    when: if name == "reset" {
                        RamWhen::Reset
                    } else {
                        RamWhen::Vblank
                    },
                });
            }
            (EXTRA_FLAG, "sprite_limit", [value @ (0 | 1)]) => sprite_limit = *value == 1,
            (EXTRA_RAM_PATCH | EXTRA_FLAG, _, _) => {
                bail!(
                    "the game carries a malformed extra of kind {} named {name:?}: rebuild it                      with this player",
                    extra.kind
                )
            }
            _ => {}
        }
    }
    Ok((patches, sprite_limit))
}

unsafe fn elements<T>(start: *const T, count: u32) -> &'static [T] {
    if count == 0 {
        return &[];
    }
    // SAFETY: the caller guarantees `count` elements at `start`.
    unsafe { slice::from_raw_parts(start, count as usize) }
}
