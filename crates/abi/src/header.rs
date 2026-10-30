use std::fmt::Write;

use crate::{
    ENTRY, EXTERNS, EXTRA, EXTRA_FLAG, EXTRA_RAM_PATCH, GAME, GAME_ABI, STATE, Struct,
    function_typedef, interrupt_typedef,
};

/// The include guard and file name of the generated header.
const GUARD: &str = "AG_STATE_H";

/// Returns `ag_state.h`: the state struct, the address table entry, the translated function
/// and interrupt entry types, the game table the player reads and the extern functions,
/// with assertions on every offset and size.
pub fn header() -> String {
    let mut out = String::new();
    // Writing into a String never fails.
    let _ = write_header(&mut out);
    out
}

fn write_header(out: &mut String) -> std::fmt::Result {
    writeln!(
        out,
        "/* Generated from the anygenesis-abi crate. Do not edit. */"
    )?;
    writeln!(out, "#ifndef {GUARD}")?;
    writeln!(out, "#define {GUARD}")?;
    writeln!(out)?;
    writeln!(out, "#include <stddef.h>")?;
    writeln!(out, "#include <stdint.h>")?;
    writeln!(out)?;
    for item in [&STATE, &ENTRY, &EXTRA, &GAME] {
        writeln!(out, "typedef struct {} {};", item.name, item.name)?;
    }
    writeln!(out)?;
    write_comment(
        out,
        "",
        "A translated function: called with the 68000 address to start at, it returns the\n\
         address execution continues at.\n",
    )?;
    writeln!(out, "{}", function_typedef())?;
    write_comment(
        out,
        "",
        "A program's interrupt entry: takes the interrupt at `level` before the instruction\n\
         at `pc` and returns its handler's address.\n",
    )?;
    writeln!(out, "{}", interrupt_typedef())?;
    writeln!(out)?;
    write_comment(out, "", "The layout version of AgGame.\n")?;
    writeln!(out, "#define AG_GAME_ABI {GAME_ABI}u")?;
    write_comment(
        out,
        "",
        "The kinds of AgExtra, as the abi crate describes them.\n",
    )?;
    writeln!(out, "#define AG_EXTRA_RAM_PATCH {EXTRA_RAM_PATCH}u")?;
    writeln!(out, "#define AG_EXTRA_FLAG {EXTRA_FLAG}u")?;
    for item in [&STATE, &ENTRY, &EXTRA, &GAME] {
        writeln!(out)?;
        write_struct(out, item)?;
    }
    for item in EXTERNS {
        writeln!(out)?;
        write_comment(out, "", item.doc)?;
        writeln!(out, "{};", (item.prototype)())?;
    }
    writeln!(out)?;
    writeln!(out, "#endif")
}

fn write_struct(out: &mut String, item: &Struct) -> std::fmt::Result {
    write_comment(out, "", item.doc)?;
    writeln!(out, "struct {} {{", item.name)?;
    for field in item.fields {
        write_comment(out, "    ", field.doc)?;
        writeln!(out, "    {};", (field.declare)(field.name))?;
    }
    writeln!(out, "}};")?;
    for field in item.fields {
        writeln!(
            out,
            "_Static_assert(offsetof({0}, {1}) == {2}, \"{0}.{1} moved\");",
            item.name, field.name, field.offset
        )?;
    }
    writeln!(
        out,
        "_Static_assert(sizeof({0}) == {1}, \"{0} changed size\");",
        item.name, item.size
    )
}

/// Writes a doc comment (lines separated by `\n`, each with Rust's leading space) as a C
/// block comment.
fn write_comment(out: &mut String, indent: &str, doc: &str) -> std::fmt::Result {
    let lines: Vec<&str> = doc.lines().map(str::trim).collect();
    for (index, line) in lines.iter().enumerate() {
        let open = if index == 0 { "/*" } else { " *" };
        let close = if index + 1 == lines.len() { " */" } else { "" };
        writeln!(out, "{indent}{open} {line}{close}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_asserts_every_state_field() {
        let text = header();
        for field in STATE.fields {
            assert!(text.contains(&format!("offsetof(AgState, {}) ==", field.name)));
        }
        assert!(text.contains("    uint32_t d[8];\n"));
        assert!(text.contains("    AgFn *function;\n"));
    }

    #[test]
    fn test_header_declares_the_game_table() {
        let text = header();
        for field in GAME.fields {
            assert!(text.contains(&format!("offsetof(AgGame, {}) ==", field.name)));
        }
        assert!(text.contains("    AgFn *const *steps;\n"));
        assert!(text.contains("    AgInterrupt *interrupt;\n"));
        assert!(text.contains("    uint8_t sha1[20];\n"));
        assert!(text.contains(&format!("#define AG_GAME_ABI {GAME_ABI}u\n")));
        assert!(text.contains(
            "typedef uint32_t AgInterrupt(AgState *state, uint32_t level, uint32_t pc);"
        ));
    }

    #[test]
    fn test_multi_line_docs_become_one_block_comment() {
        let mut out = String::new();
        write_comment(&mut out, "", " First.\n Second.\n").unwrap();
        assert_eq!(out, "/* First.\n * Second. */\n");
    }
}
