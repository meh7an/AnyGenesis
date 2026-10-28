//! The game the build script translated and linked into this binary, if any.

use anygenesis_abi::{AgEntry, AgFn, AgInterrupt};
use anyhow::{Result, bail};

/// A translated game linked into the binary.
#[derive(Debug, Clone, Copy)]
pub struct CompiledGame {
    /// The title from the ROM's header, for display.
    pub name: &'static str,
    /// The SHA-1 of the ROM it was translated from, lowercase hex.
    pub sha1: &'static str,
    /// Analysed 68000 functions.
    pub functions: usize,
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
}

impl CompiledGame {
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

include!(concat!(env!("OUT_DIR"), "/game.rs"));
