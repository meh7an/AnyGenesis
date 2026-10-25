//! The game the build script translated and linked into this binary, if any.

use anygenesis_abi::AgEntry;
use anyhow::{Result, bail};

/// A translated game linked into the binary.
#[derive(Debug)]
pub struct CompiledGame {
    /// The name of its config in games/.
    pub name: &'static str,
    /// The SHA-1 of the ROM it was translated from, lowercase hex.
    pub sha1: &'static str,
    /// Analysed 68000 functions.
    pub functions: usize,
    /// Every function entry and resume point with the function that runs it.
    pub table: &'static [AgEntry],
    /// Entries of the functions reset and the exception vectors start, ascending.
    pub vector_handlers: &'static [u32],
}

impl CompiledGame {
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
