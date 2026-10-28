//! Validates decoding and discovery of Sonic 1 against the listing of a local s1disasm build
//! (REV00), analysed from its vectors alone as game builds do: every listed instruction must
//! decode as the listing has it and nothing found may be data, while coverage is reported,
//! not asserted (stepped code covers what discovery misses). Run with
//! `cargo test -p anygenesis-recomp --test sonic1_validation -- --ignored --nocapture`
//! with `ANYGENESIS_ROM` and `ANYGENESIS_S1_LISTING` set. The report quotes listing mnemonics,
//! so it goes to `target/tmp/s1-validation-report.md` and never into the repo.

mod listing;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use anygenesis_analysis::{Analysis, EntryKind, Suggestion, analyze};
use anygenesis_m68k::{Dialect, Ea, FormatOptions, Instruction, Labels, Mnemonic, Operand};
use anygenesis_rom::Rom;
use anygenesis_shared::{Address, ROM_MARKER, Size};

use listing::{Coverage, ListedInstruction, Listing};

/// Table sizes read from the listing stop here; no Sonic 1 table comes close.
const MAX_TABLE_ENTRIES: u32 = 1024;

/// Sega Retro, "Sonic the Hedgehog (Mega Drive)/Technical information", Revision 00.
const SONIC1_REV00_SHA1: &str = "6ddb7de1e17e7f6cdb88927bd906352030daa194";

#[test]
#[ignore = "needs ANYGENESIS_ROM and ANYGENESIS_S1_LISTING from a local s1disasm REV00 build"]
fn test_sonic1_discovery_matches_s1disasm_listing() {
    let rom_path = std::env::var_os("ANYGENESIS_ROM").expect("ANYGENESIS_ROM is not set");
    let listing_path =
        std::env::var_os("ANYGENESIS_S1_LISTING").expect("ANYGENESIS_S1_LISTING is not set");
    let rom = Rom::load(&rom_path).expect("ANYGENESIS_ROM loads");
    assert_eq!(
        rom.sha1().to_string(),
        SONIC1_REV00_SHA1,
        "this test needs Sonic 1 REV00"
    );
    let analysis = analyze(&rom);
    let listing_text = std::fs::read(&listing_path).expect("ANYGENESIS_S1_LISTING is readable");
    let listing = match listing::parse(&listing_text) {
        Ok(listing) => listing,
        Err(error) => panic!("listing: {error}"),
    };

    let validation = Validation::run(rom.bytes(), &analysis, &listing);
    let report_path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("s1-validation-report.md");
    let report = format!("{ROM_MARKER}\n\n{}", validation.report(&analysis, &listing));
    std::fs::write(&report_path, report).expect("report written");
    println!("report                  {}", report_path.display());
    for (name, value) in validation.summary() {
        println!("{name:<24}{value}");
    }

    assert!(validation.mismatches.is_empty(), "decoder mismatches");
    assert!(
        validation.precision_failures.is_empty(),
        "data decoded as code"
    );
}

/// The listing and our decoder disagree about one listed instruction.
struct Mismatch {
    address: u32,
    listed: ListedInstruction,
    /// Our AS text, or the decode error.
    ours: String,
    our_length: Option<u32>,
}

/// An instruction start the analysis found that the listing does not list.
struct Find {
    address: u32,
    what: String,
}

/// A run of listed instructions the analysis does not reach, with nothing reached between.
struct Gap {
    start: u32,
    end: u32,
    count: usize,
    labelled: bool,
    /// Decoded instructions whose operand or branch names the start.
    code_refs: Vec<u32>,
    /// Even ROM offsets holding the start as a 32-bit pointer.
    pointer_refs: Vec<u32>,
}

struct Validation {
    listed: usize,
    mismatches: Vec<Mismatch>,
    found: usize,
    precision_failures: Vec<Find>,
    explain: Vec<Find>,
    reached: usize,
    gaps: Vec<Gap>,
    unresolved: Vec<String>,
}

impl Validation {
    fn run(rom: &[u8], analysis: &Analysis, listing: &Listing) -> Self {
        let rom_size = rom.len() as u32;
        let in_rom: Vec<(u32, &ListedInstruction)> = listing
            .instructions
            .iter()
            .filter(|(address, instruction)| **address + instruction.length <= rom_size)
            .map(|(address, instruction)| (*address, instruction))
            .collect();

        let mismatches = in_rom
            .iter()
            .filter_map(|&(address, listed)| compare(rom, address, listed))
            .collect();

        let mut precision_failures = Vec::new();
        let mut explain = Vec::new();
        for address in analysis.instructions.keys().map(|address| address.get()) {
            match listing.coverage(address) {
                Coverage::InstructionStart => {}
                Coverage::InsideInstruction(start) => precision_failures.push(Find {
                    address,
                    what: format!("inside the listed instruction at {start:06X}"),
                }),
                Coverage::Data(data) if data.directive.starts_with("dc.w") => explain.push(Find {
                    address,
                    what: format!("{} run at {:06X}", data.directive, data.address),
                }),
                Coverage::Data(data) => precision_failures.push(Find {
                    address,
                    what: format!("{} run at {:06X}", data.directive, data.address),
                }),
                Coverage::Unlisted => explain.push(Find {
                    address,
                    what: String::from("bytes the listing does not print"),
                }),
            }
        }

        let reached = in_rom
            .iter()
            .filter(|(address, _)| analysis.instructions.contains_key(&Address::new(*address)))
            .count();
        let references = References::new(rom, analysis);
        let gaps = gaps(&in_rom, analysis)
            .into_iter()
            .map(|(start, end, count)| Gap {
                start,
                end,
                count,
                labelled: listing.labels.contains(&start),
                code_refs: references.code.get(&start).cloned().unwrap_or_default(),
                pointer_refs: references.pointers.get(&start).cloned().unwrap_or_default(),
            })
            .collect();

        Self {
            listed: in_rom.len(),
            mismatches,
            found: analysis.instructions.len(),
            precision_failures,
            explain,
            reached,
            gaps,
            unresolved: unresolved_sites(analysis, listing),
        }
    }

    fn summary(&self) -> Vec<(&'static str, String)> {
        let unreached: usize = self.gaps.iter().map(|gap| gap.count).sum();
        vec![
            ("listing instructions", self.listed.to_string()),
            ("decoder mismatches", self.mismatches.len().to_string()),
            ("found instructions", self.found.to_string()),
            (
                "precision failures",
                self.precision_failures.len().to_string(),
            ),
            ("explain items", self.explain.len().to_string()),
            (
                "reached",
                format!(
                    "{} ({:.2}%)",
                    self.reached,
                    100.0 * self.reached as f64 / self.listed.max(1) as f64
                ),
            ),
            ("unreached", unreached.to_string()),
            ("gap runs", self.gaps.len().to_string()),
            ("unresolved sites", self.unresolved.len().to_string()),
        ]
    }

    fn report(&self, analysis: &Analysis, listing: &Listing) -> String {
        let mut out = String::from("# Sonic 1 against the s1disasm listing\n\n");
        out.push_str("| Check | Count |\n|-------|------:|\n");
        for (name, value) in self.summary() {
            let _ = writeln!(out, "| {name} | {value} |");
        }

        out.push_str("\n## Decoder mismatches\n\n");
        for mismatch in &self.mismatches {
            let our_length = mismatch
                .our_length
                .map_or_else(|| String::from("-"), |length| length.to_string());
            let _ = writeln!(
                out,
                "- {:06X}: listing `{}` ({} bytes), ours `{}` ({our_length} bytes)",
                mismatch.address, mismatch.listed.mnemonic, mismatch.listed.length, mismatch.ours
            );
        }

        out.push_str("\n## Precision failures\n\n");
        write_finds(&mut out, &self.precision_failures);
        out.push_str("\n## Explain items\n\n");
        write_finds(&mut out, &self.explain);

        out.push_str("\n## Unresolved sites\n\n");
        for site in &self.unresolved {
            let _ = writeln!(out, "{site}");
        }

        out.push_str("\n## Coverage gaps\n\n");
        out.push_str(
            "Referenced starts are likely reached through pointers; the rest look dead.\n\n",
        );
        out.push_str("| Start | End | Instructions | Label | Code refs | Pointers | Looks |\n");
        out.push_str("|-------|-----|-------------:|-------|-----------|----------|-------|\n");
        for gap in &self.gaps {
            let referenced = !gap.code_refs.is_empty() || !gap.pointer_refs.is_empty();
            let _ = writeln!(
                out,
                "| {:06X} | {:06X} | {} | {} | {} | {} | {} |",
                gap.start,
                gap.end,
                gap.count,
                if gap.labelled { "yes" } else { "" },
                hex_list(&gap.code_refs),
                hex_list(&gap.pointer_refs),
                if referenced { "referenced" } else { "dead" }
            );
        }
        let _ = writeln!(
            out,
            "\n{} functions, {} labelled addresses in the listing.",
            analysis.functions.len(),
            listing.labels.len()
        );
        out
    }
}

/// Decodes the ROM at a listed instruction and compares length, mnemonic and size.
fn compare(rom: &[u8], address: u32, listed: &ListedInstruction) -> Option<Mismatch> {
    match anygenesis_m68k::decode_at(rom, address as usize) {
        Ok(instruction) => {
            let text = instruction
                .format(Address::new(address), FormatOptions::new(Dialect::As))
                .to_string();
            let matches = instruction.length() == listed.length
                && mnemonic_matches(&listed.mnemonic, &instruction, &text);
            (!matches).then(|| Mismatch {
                address,
                listed: listed.clone(),
                ours: text,
                our_length: Some(instruction.length()),
            })
        }
        Err(error) => Some(Mismatch {
            address,
            listed: listed.clone(),
            ours: error.to_string(),
            our_length: None,
        }),
    }
}

/// The listing spells some mnemonics differently and leaves sizes out where AS implies them.
fn mnemonic_matches(listed: &str, instruction: &Instruction, our_text: &str) -> bool {
    let (listed_base, listed_suffix) = match listed.split_once('.') {
        Some((base, suffix)) => (base, Some(suffix)),
        None => (listed, None),
    };
    let our_base = our_text
        .split(['\t', '.'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if alias(listed_base) != alias(&our_base) {
        return false;
    }
    let is_branch = matches!(
        instruction.mnemonic,
        Mnemonic::Bra | Mnemonic::Bsr | Mnemonic::Bcc(_)
    );
    match listed_suffix {
        Some("b") => instruction.size == Some(Size::Byte),
        Some("s") => is_branch && instruction.size == Some(Size::Byte),
        Some("w") => instruction.size == Some(Size::Word),
        Some("l") => instruction.size == Some(Size::Long),
        Some(_) => false,
        // AS picks the branch size itself; the length check settles it.
        None => {
            is_branch
                || instruction.size.is_none()
                || instruction.size == Some(Size::Word)
                || has_one_size(instruction.mnemonic)
        }
    }
}

fn alias(base: &str) -> &str {
    match base {
        "bhs" => "bcc",
        "blo" => "bcs",
        "shs" => "scc",
        "slo" => "scs",
        "dbra" => "dbf",
        "dbhs" => "dbcc",
        "dblo" => "dbcs",
        other => other,
    }
}

/// Operations whose size follows from the mnemonic and operands alone.
fn has_one_size(mnemonic: Mnemonic) -> bool {
    matches!(
        mnemonic,
        Mnemonic::Abcd
            | Mnemonic::Sbcd
            | Mnemonic::Nbcd
            | Mnemonic::Tas
            | Mnemonic::Scc(_)
            | Mnemonic::Dbcc(_)
            | Mnemonic::Lea
            | Mnemonic::Pea
            | Mnemonic::Moveq
            | Mnemonic::Exg
            | Mnemonic::Swap
            | Mnemonic::Link
            | Mnemonic::Chk
            | Mnemonic::Muls
            | Mnemonic::Mulu
            | Mnemonic::Divs
            | Mnemonic::Divu
            | Mnemonic::Bchg
            | Mnemonic::Bclr
            | Mnemonic::Bset
            | Mnemonic::Btst
    )
}

/// Splits the listed instructions into runs the analysis does not reach.
fn gaps(in_rom: &[(u32, &ListedInstruction)], analysis: &Analysis) -> Vec<(u32, u32, usize)> {
    let mut runs: Vec<(u32, u32, usize)> = Vec::new();
    let mut open = false;
    for &(address, instruction) in in_rom {
        if analysis.instructions.contains_key(&Address::new(address)) {
            open = false;
            continue;
        }
        let end = address + instruction.length;
        match runs.last_mut() {
            Some(run) if open => {
                run.1 = end;
                run.2 += 1;
            }
            _ => runs.push((address, end, 1)),
        }
        open = true;
    }
    runs
}

/// Who names which address: decoded operands and branches, and 32-bit words in the ROM.
struct References {
    code: HashMap<u32, Vec<u32>>,
    pointers: HashMap<u32, Vec<u32>>,
}

/// Collects the addresses the formatter asks labels for: branch and PC-relative targets.
struct Recorder(RefCell<Vec<Address>>);

impl Labels for Recorder {
    fn label(&self, address: Address) -> Option<String> {
        self.0.borrow_mut().push(address);
        None
    }
}

impl References {
    fn new(rom: &[u8], analysis: &Analysis) -> Self {
        let mut code: HashMap<u32, Vec<u32>> = HashMap::new();
        let recorder = Recorder(RefCell::new(Vec::new()));
        for (address, instruction) in &analysis.instructions {
            let options = FormatOptions::new(Dialect::As).with_labels(&recorder);
            let _ = instruction.format(*address, options).to_string();
            let mut targets: Vec<u32> = recorder
                .0
                .borrow_mut()
                .drain(..)
                .map(Address::get)
                .collect();
            targets.extend(instruction.operands.iter().flatten().filter_map(
                |operand| match operand {
                    Operand::Ea(Ea::Immediate(value) | Ea::AbsoluteLong(value)) => {
                        Some(value & Address::MASK)
                    }
                    _ => None,
                },
            ));
            for target in targets {
                code.entry(target).or_default().push(address.get());
            }
        }
        let mut pointers: HashMap<u32, Vec<u32>> = HashMap::new();
        for offset in (0..rom.len().saturating_sub(3)).step_by(2) {
            let value = u32::from_be_bytes([
                rom[offset],
                rom[offset + 1],
                rom[offset + 2],
                rom[offset + 3],
            ]);
            if value < rom.len() as u32 && value.is_multiple_of(2) && value >= 0x200 {
                pointers.entry(value).or_default().push(offset as u32);
            }
        }
        Self { code, pointers }
    }
}

/// One line per unresolved site, table sizes read from the listing.
fn unresolved_sites(analysis: &Analysis, listing: &Listing) -> Vec<String> {
    analysis
        .unresolved
        .iter()
        .map(|site| match &site.suggestion {
            Suggestion::JumpTable { address, kind, .. } => {
                let count = listed_table_count(listing, address.get(), *kind);
                format!(
                    "- {:06X}: table at {:06X}, {}, {} entries in the listing",
                    site.site.get(),
                    address.get(),
                    kind.name(),
                    count.map_or_else(|| String::from("?"), |count| count.to_string())
                )
            }
            _ => format!(
                "- {:06X}: {} through a register, targets by hand",
                site.site.get(),
                if site.is_call { "call" } else { "jump" }
            ),
        })
        .collect()
}

/// Counts table entries in the listing: data up to the next label, or `bra.w` rows up to the
/// first other line (rows often carry labels of their own).
fn listed_table_count(listing: &Listing, address: u32, kind: EntryKind) -> Option<u32> {
    let step = kind.entry_bytes();
    let mut count = 0;
    let mut position = address;
    while count < MAX_TABLE_ENTRIES {
        if kind != EntryKind::BraW && position != address && listing.labels.contains(&position) {
            break;
        }
        let entry = match kind {
            EntryKind::BraW => listing
                .instructions
                .get(&position)
                .is_some_and(|row| row.length == 4 && row.mnemonic.starts_with("bra")),
            _ => (0..step)
                .all(|offset| matches!(listing.coverage(position + offset), Coverage::Data(_))),
        };
        if !entry {
            break;
        }
        count += 1;
        position += step;
    }
    (count > 0).then_some(count)
}

fn write_finds(out: &mut String, finds: &[Find]) {
    for find in finds {
        let _ = writeln!(out, "- {:06X}: {}", find.address, find.what);
    }
}

fn hex_list(values: &[u32]) -> String {
    let mut text = values
        .iter()
        .take(4)
        .map(|value| format!("{value:06X}"))
        .collect::<Vec<_>>()
        .join(" ");
    if values.len() > 4 {
        let _ = write!(text, " +{}", values.len() - 4);
    }
    text
}
