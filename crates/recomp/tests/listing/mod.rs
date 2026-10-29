//! Reader for the AS assembler listing of a local disassembly build (s1disasm's `sonic.lst`,
//! s2disasm's `s2.lst`).
//!
//! Only addresses, lengths, mnemonics, directive names and the presence of labels are kept.
//! The listing's text never leaves the test process, and nothing here names a label.

use std::collections::{BTreeMap, BTreeSet};

/// The byte field ends and the source text begins at this column on every line, whatever
/// the width of the address.
const SOURCE_COLUMN: usize = 40;

/// The listing body ends where the symbol table begins.
const TABLES_START: &str = "Symbol Table";

const CONDITIONS: [&str; 19] = [
    "t", "f", "ra", "hi", "ls", "cc", "cs", "hs", "lo", "ne", "eq", "vc", "vs", "pl", "mi", "ge",
    "lt", "gt", "le",
];

const PLAIN_MNEMONICS: [&str; 76] = [
    "abcd", "add", "adda", "addi", "addq", "addx", "and", "andi", "asl", "asr", "bchg", "bclr",
    "bset", "btst", "chk", "clr", "cmp", "cmpa", "cmpi", "cmpm", "divs", "divu", "eor", "eori",
    "exg", "ext", "illegal", "jmp", "jsr", "lea", "link", "lsl", "lsr", "move", "movea", "movem",
    "movep", "moveq", "muls", "mulu", "nbcd", "neg", "negx", "nop", "not", "or", "ori", "pea",
    "reset", "rol", "ror", "roxl", "roxr", "rte", "rtr", "rts", "sbcd", "stop", "sub", "suba",
    "subi", "subq", "subx", "swap", "tas", "trap", "trapv", "tst", "unlk", "bra", "bsr", "st",
    "sf", "dbt", "dbf", "dbra",
];

/// A 68000 instruction the listing shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedInstruction {
    pub length: u32,
    /// Lowercase, with its size suffix and without a leading `!`.
    pub mnemonic: String,
}

/// A run of 68000-side data bytes the listing shows (`dc.w`, `dc.b`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedData {
    pub address: u32,
    pub length: u32,
    /// Lowercase directive with its size suffix.
    pub directive: String,
}

/// What the listing says about the 68000 side of the ROM.
#[derive(Debug, Default)]
pub struct Listing {
    pub instructions: BTreeMap<u32, ListedInstruction>,
    /// Data runs by start address. Patch lines that rewrite a byte of an earlier
    /// instruction are kept too, so data runs may lie inside instructions.
    pub data: BTreeMap<u32, ListedData>,
    /// Addresses of 68000-side lines that carry a label.
    pub labels: BTreeSet<u32>,
}

/// What a byte of the ROM is, according to the listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage<'a> {
    /// The first byte of a listed instruction.
    InstructionStart,
    /// A later byte of the instruction starting at this address.
    InsideInstruction(u32),
    /// A byte of a listed data run.
    Data(&'a ListedData),
    /// A byte the listing does not print (binclude'd files).
    Unlisted,
}

impl Listing {
    /// Classifies the byte at `address`; instructions win over the patch lines inside them.
    pub fn coverage(&self, address: u32) -> Coverage<'_> {
        if let Some((&start, instruction)) = self.instructions.range(..=address).next_back()
            && address < start + instruction.length
        {
            return if start == address {
                Coverage::InstructionStart
            } else {
                Coverage::InsideInstruction(start)
            };
        }
        match self.data.range(..=address).next_back() {
            Some((_, data)) if address < data.address + data.length => Coverage::Data(data),
            _ => Coverage::Unlisted,
        }
    }
}

/// One source line that produced bytes, with its continuation lines merged in.
struct Emit {
    address: u32,
    bytes: Vec<u8>,
    mnemonic: String,
    is_68000: bool,
}

/// Parses the listing body.
///
/// # Errors
/// Returns a description of the first line that does not fit the format.
pub fn parse(text: &[u8]) -> Result<Listing, String> {
    let mut listing = Listing::default();
    let mut emits: Vec<Emit> = Vec::new();
    let mut cpu = String::from("68000");
    let mut saved_cpus = Vec::new();
    for (index, raw) in text.split(|&byte| byte == b'\n').enumerate() {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if String::from_utf8_lossy(line)
            .trim_start()
            .starts_with(TABLES_START)
        {
            break;
        }
        let Some(head) = parse_head(line) else {
            continue;
        };
        let field = if head.end < SOURCE_COLUMN {
            &line[head.end..line.len().min(SOURCE_COLUMN)]
        } else {
            &[][..]
        };
        let source = line.get(SOURCE_COLUMN..).unwrap_or_default();
        let statement = parse_statement(source);

        match statement.mnemonic.as_str() {
            "cpu" => cpu = statement.operands.to_ascii_uppercase(),
            "save" => saved_cpus.push(cpu.clone()),
            "restore" => cpu = saved_cpus.pop().unwrap_or(cpu),
            _ => {}
        }
        let tokens: Vec<&[u8]> = field
            .split(u8::is_ascii_whitespace)
            .filter(|token| !token.is_empty())
            .collect();
        let is_68000 = cpu == "68000";
        let is_assignment = tokens.first().is_some_and(|token| token.starts_with(b"="));
        if statement.has_label && is_68000 && !is_assignment {
            listing.labels.insert(head.address);
        }
        if tokens.is_empty() || !tokens.iter().all(|token| is_hex(token)) {
            continue;
        }
        let bytes = decode_hex(&tokens).map_err(|error| format!("line {}: {error}", index + 1))?;
        if head.line_number.is_none() {
            let previous = emits
                .last_mut()
                .ok_or_else(|| format!("line {}: continuation before any emit", index + 1))?;
            if head.address != previous.address + previous.bytes.len() as u32 {
                return Err(format!(
                    "line {}: continuation at {:X} does not follow {:X}",
                    index + 1,
                    head.address,
                    previous.address
                ));
            }
            previous.bytes.extend(bytes);
            continue;
        }
        emits.push(Emit {
            address: head.address,
            bytes,
            mnemonic: statement.mnemonic,
            is_68000,
        });
    }

    for emit in emits.into_iter().filter(|emit| emit.is_68000) {
        let length = emit.bytes.len() as u32;
        if is_instruction(&emit.mnemonic) {
            listing.instructions.insert(
                emit.address,
                ListedInstruction {
                    length,
                    mnemonic: emit.mnemonic,
                },
            );
        } else {
            listing.data.insert(
                emit.address,
                ListedData {
                    address: emit.address,
                    length,
                    directive: emit.mnemonic,
                },
            );
        }
    }
    Ok(listing)
}

/// The fixed part of a body line: `[(depth)] [line/] address : `.
struct Head {
    line_number: Option<u32>,
    address: u32,
    /// Byte offset just past the ` : ` separator.
    end: usize,
}

fn parse_head(line: &[u8]) -> Option<Head> {
    let mut position = 0;
    if line.first() == Some(&b'(') {
        position = line.iter().position(|&byte| byte == b')')? + 1;
    }
    position += count_spaces(&line[position..]);
    let first = take_while(&line[position..], |byte| byte.is_ascii_hexdigit());
    let mut line_number = None;
    if line.get(position + first.len()) == Some(&b'/') {
        line_number = Some(std::str::from_utf8(first).ok()?.parse().ok()?);
        position += first.len() + 1;
        position += count_spaces(&line[position..]);
    }
    let address_text = take_while(&line[position..], |byte| {
        byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte)
    });
    if address_text.is_empty() || !line[position + address_text.len()..].starts_with(b" : ") {
        return None;
    }
    let wide = u64::from_str_radix(std::str::from_utf8(address_text).ok()?, 16).ok()?;
    Some(Head {
        line_number,
        // Phased RAM structures print 64-bit values; only the low bits are an address.
        address: wide as u32,
        end: position + address_text.len() + 3,
    })
}

/// The parts of a source statement the validation needs.
struct Statement {
    mnemonic: String,
    operands: String,
    has_label: bool,
}

fn parse_statement(source: &[u8]) -> Statement {
    let code = String::from_utf8_lossy(strip_comment(source));
    let code = code.trim_end();
    let mut rest = code;
    let mut has_label = false;
    // A token in column 0 is a label, and may run straight into the mnemonic after its ':'.
    if rest.starts_with(|c: char| !c.is_whitespace()) {
        has_label = true;
        let label_end = rest
            .find(|c: char| c.is_whitespace() || c == ':')
            .unwrap_or(rest.len());
        rest = &rest[label_end..];
        rest = rest.strip_prefix(':').unwrap_or(rest);
    }
    let mut parts = rest.split_whitespace();
    let mut mnemonic = parts.next().unwrap_or_default();
    if mnemonic.ends_with(':') {
        has_label = true;
        mnemonic = parts.next().unwrap_or_default();
    }
    let operands = rest
        .trim_start()
        .strip_prefix(mnemonic)
        .map(str::trim)
        .unwrap_or_default();
    Statement {
        mnemonic: mnemonic.trim_start_matches('!').to_ascii_lowercase(),
        operands: operands.to_owned(),
        has_label,
    }
}

fn strip_comment(source: &[u8]) -> &[u8] {
    let mut quote = None;
    for (index, &byte) in source.iter().enumerate() {
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None if byte == b'\'' || byte == b'"' => quote = Some(byte),
            None if byte == b';' => return &source[..index],
            None => {}
        }
    }
    source
}

/// Whether a mnemonic (size suffix included) is a 68000 instruction rather than a directive.
pub fn is_instruction(mnemonic: &str) -> bool {
    let base = mnemonic.split('.').next().unwrap_or_default();
    if PLAIN_MNEMONICS.contains(&base) {
        return true;
    }
    let conditional = |prefix: &str| {
        base.strip_prefix(prefix)
            .is_some_and(|condition| CONDITIONS.contains(&condition))
    };
    conditional("b") || conditional("db") || conditional("s")
}

fn is_hex(token: &[u8]) -> bool {
    token.len().is_multiple_of(2)
        && token
            .iter()
            .all(|&byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
}

fn decode_hex(tokens: &[&[u8]]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    for token in tokens {
        for pair in token.chunks(2) {
            let text = std::str::from_utf8(pair).map_err(|error| error.to_string())?;
            bytes.push(u8::from_str_radix(text, 16).map_err(|error| error.to_string())?);
        }
    }
    Ok(bytes)
}

fn count_spaces(bytes: &[u8]) -> usize {
    take_while(bytes, |byte| byte == b' ').len()
}

fn take_while(bytes: &[u8], keep: impl Fn(u8) -> bool) -> &[u8] {
    let end = bytes
        .iter()
        .position(|&byte| !keep(byte))
        .unwrap_or(bytes.len());
    &bytes[..end]
}
