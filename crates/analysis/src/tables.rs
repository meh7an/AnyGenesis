use anygenesis_gamecfg::EntryKind;
use anygenesis_shared::{Address, read_u16_be, read_u32_be};

/// The opcode word of `bra.w`; its displacement follows in the next word.
pub(crate) const BRA_W: u16 = 0x6000;

/// The most entries a count guess proposes; real tables are far smaller.
const MAX_GUESS: u32 = 256;

/// Returns the target of entry `index` of a table at `table`, or `None` when the entry
/// lies past the end of `bytes`.
pub(crate) fn entry_target(
    bytes: &[u8],
    table: Address,
    kind: EntryKind,
    index: u32,
) -> Option<Address> {
    let entry = table.wrapping_add(index * kind.entry_bytes());
    let offset = usize::try_from(entry.get()).ok()?;
    match kind {
        EntryKind::WordOffset => {
            let word = read_u16_be(bytes, offset).ok()?;
            // Offsets are signed: `jmp Tbl(pc,dn.w)` sign-extends the word index.
            Some(table.offset(i32::from(word as i16)))
        }
        EntryKind::Long => read_u32_be(bytes, offset).ok().map(Address::new),
        EntryKind::BraW => read_u32_be(bytes, offset).ok().map(|_| entry),
        _ => None,
    }
}

/// Guesses how many entries a table at `table` holds, for the report only.
///
/// Entries are read while their targets are even and inside the ROM; the table is assumed
/// to end where the nearest target after it begins, since tables usually sit just before
/// the code they index. `bra_w` entries must also be `bra.w` instructions.
pub(crate) fn guess_count(bytes: &[u8], table: Address, kind: EntryKind) -> Option<u32> {
    let rom_end = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    let mut limit = rom_end;
    let mut count = 0;
    while count < MAX_GUESS {
        let entry = table.wrapping_add(count * kind.entry_bytes());
        if entry.get() >= limit {
            break;
        }
        if kind == EntryKind::BraW
            && usize::try_from(entry.get())
                .ok()
                .and_then(|offset| read_u16_be(bytes, offset).ok())
                != Some(BRA_W)
        {
            break;
        }
        let Some(target) = entry_target(bytes, table, kind, count) else {
            break;
        };
        let target = if kind == EntryKind::BraW {
            bra_w_target(bytes, target)
        } else {
            Some(target)
        };
        match target {
            Some(target) if target.is_even() && target.get() < rom_end && target != table => {
                if target > table {
                    limit = limit.min(target.get());
                }
            }
            _ => break,
        }
        count += 1;
    }
    (count > 0).then_some(count)
}

/// Returns where the `bra.w` at `entry` leads.
fn bra_w_target(bytes: &[u8], entry: Address) -> Option<Address> {
    let offset = usize::try_from(entry.get()).ok()?.checked_add(2)?;
    let displacement = read_u16_be(bytes, offset).ok()? as i16;
    Some(entry.wrapping_add(2).offset(i32::from(displacement)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_at(base: usize, words: &[u16]) -> Vec<u8> {
        let mut bytes = vec![0; base];
        bytes.extend(words.iter().flat_map(|word| word.to_be_bytes()));
        bytes
    }

    #[test]
    fn test_entry_target_word_offset_is_signed_from_table() {
        let bytes = bytes_at(0x10, &[0x0004, 0xFFFC]);
        let table = Address::new(0x10);
        assert_eq!(
            entry_target(&bytes, table, EntryKind::WordOffset, 0),
            Some(Address::new(0x14))
        );
        assert_eq!(
            entry_target(&bytes, table, EntryKind::WordOffset, 1),
            Some(Address::new(0x0C))
        );
        assert_eq!(entry_target(&bytes, table, EntryKind::WordOffset, 2), None);
    }

    #[test]
    fn test_entry_target_long_masks_to_24_bits() {
        let bytes = bytes_at(0x10, &[0xFF00, 0x1234]);
        assert_eq!(
            entry_target(&bytes, Address::new(0x10), EntryKind::Long, 0),
            Some(Address::new(0x1234))
        );
    }

    #[test]
    fn test_entry_target_bra_w_is_the_entry_itself() {
        let bytes = bytes_at(0x10, &[BRA_W, 0x0010, BRA_W, 0x0020]);
        assert_eq!(
            entry_target(&bytes, Address::new(0x10), EntryKind::BraW, 1),
            Some(Address::new(0x14))
        );
    }

    #[test]
    fn test_guess_count_word_table_stops_at_first_target() {
        // Two offsets, then the code they lead to.
        let bytes = bytes_at(0x10, &[0x0004, 0x0006, 0x4E75, 0x4E75]);
        assert_eq!(
            guess_count(&bytes, Address::new(0x10), EntryKind::WordOffset),
            Some(2)
        );
    }

    #[test]
    fn test_guess_count_bra_w_table_stops_at_other_opcode() {
        let bytes = bytes_at(0x10, &[BRA_W, 0x0006, BRA_W, 0x0004, 0x4E75, 0x4E75]);
        assert_eq!(
            guess_count(&bytes, Address::new(0x10), EntryKind::BraW),
            Some(2)
        );
    }

    #[test]
    fn test_guess_count_odd_first_target_gives_none() {
        let bytes = bytes_at(0x10, &[0x0003, 0x4E75]);
        assert_eq!(
            guess_count(&bytes, Address::new(0x10), EntryKind::WordOffset),
            None
        );
    }
}
