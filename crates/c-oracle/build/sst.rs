//! The SingleStepTests sample: each chosen case becomes a one-instruction function at its
//! own address, emitted through the normal emitter from a one-function analysis, and the
//! instruction's stepped function. Every exit leaves the function, so it goes to the
//! dispatcher or returns, and every flag stays live.
//!
//! Per file the sample takes up to [`SAMPLE`] cases, distinct opcode words first, in file
//! order. Left out and counted: address-error cases (as in M2.2), opcodes that do not
//! decode, instructions that wrap past 0xFFFFFF, static transfers to the instruction itself
//! (a goto or call that would loop in C, where the suite runs one instruction), and cases
//! whose RAM addresses alias in the 64 KiB mirror the inline fast path implements (the
//! suite's memory is flat).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anygenesis_analysis::{
    Analysis, Block, FlagLiveness, Function, LivenessOptions, Origin, Terminator,
};
use anygenesis_codegen_c::{EmitOptions, Output, emit, emit_stepped_instruction, step_name};
use anygenesis_ir::{Control, Lifted, Op, lift};
use anygenesis_m68k::{Instruction, MAX_WORDS, decode};
use anygenesis_oracle::sst::{self, TestCase, Transaction, TransactionKind};
use anygenesis_shared::Address;

use crate::compile;

/// Cases translated per suite file.
pub const SAMPLE: usize = 256;

/// SingleStepTests stores PC as MAME's prefetch address, this far past the opcode.
const PC_OFFSET: u32 = 4;
const RAM_START: u32 = 0xE0_0000;
const RAM_MASK: u32 = 0xFFFF;
/// Cases per generated C file.
const CASES_PER_FILE: usize = 512;

/// A case ready to translate.
struct Prepared {
    index: usize,
    address: Address,
    instruction: Instruction,
}

/// Emits the sample into `out`/sst, compiles it and returns the Rust items naming it.
pub fn translate_sample(workspace: &Path, out: &Path) -> String {
    let suite = workspace.join("testdata/m68000/v1");
    println!("cargo::rerun-if-changed={}", suite.display());
    let Ok(listing) = fs::read_dir(&suite) else {
        return items(None, &[], &[]);
    };
    let mut files: Vec<PathBuf> = listing
        .map(|entry| entry.expect("listing the suite directory").path())
        .filter(|path| path.to_string_lossy().ends_with(".json.bin"))
        .collect();
    files.sort();

    let directory = out.join("sst");
    fs::create_dir_all(&directory).expect("sst directory created");
    let mut cases = Vec::new();
    let mut exclusions = Vec::new();
    let mut chunks: Vec<String> = Vec::new();
    let mut headers_written = false;
    for path in &files {
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let suite_cases = sst::load_file(path).unwrap_or_else(|error| panic!("{error}"));
        let mut excluded: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut eligible = Vec::new();
        for (index, case) in suite_cases.iter().enumerate() {
            match prepare(index, case) {
                Ok(prepared) => eligible.push(prepared),
                Err(reason) => *excluded.entry(reason).or_default() += 1,
            }
        }
        for (reason, count) in excluded {
            exclusions.push((file.clone(), reason, count));
        }
        for prepared in sample(eligible) {
            let number = cases.len();
            let prefix = format!("s{number:05}_");
            let output = translate(&prepared, &prefix);
            if !headers_written {
                for header in ["ag_state.h", "ag_runtime.h"] {
                    let text = &file_text(&output, header);
                    fs::write(directory.join(header), text).expect("header written");
                }
                headers_written = true;
            }
            if number % CASES_PER_FILE == 0 {
                chunks.push(String::new());
            }
            let chunk = chunks.last_mut().expect("a chunk was just opened");
            chunk.push_str(&inline(&output));
            let lifted =
                lift(&prepared.instruction, prepared.address).expect("prepared cases lift");
            // No ROM, as for the function.
            chunk.push_str(&emit_stepped_instruction(&lifted, 0, &prefix));
            let address = prepared.address.get();
            let name = format!("{prefix}f_{address:06X}");
            cases.push((
                file.clone(),
                prepared.index,
                name,
                step_name(&prefix, address),
            ));
        }
    }
    let sources: Vec<PathBuf> = chunks
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let path = directory.join(format!("sst_{index:03}.c"));
            fs::write(&path, text).expect("sample file written");
            path
        })
        .collect();
    if !sources.is_empty() {
        compile::library("ag_sst", &directory, &sources);
    }
    items(Some(SAMPLE), &cases, &exclusions)
}

/// Decodes and lifts a case, or says why it stays out.
fn prepare(index: usize, case: &TestCase) -> Result<Prepared, &'static str> {
    if raises_address_error(case) {
        return Err("address error");
    }
    let initial = &case.initial;
    let address = Address::new(initial.registers.pc.wrapping_sub(PC_OFFSET));
    let mut memory: HashMap<u32, u8> = initial
        .ram
        .iter()
        .map(|&(address, value)| (address & Address::MASK, value))
        .collect();
    // The prefetch queue goes back into memory in front of the prefetch address, as the
    // M2.2 harness does.
    for (offset, word) in [0, 2].into_iter().zip(initial.prefetch) {
        let [high, low] = (word as u16).to_be_bytes();
        memory.insert(address.wrapping_add(offset).get(), high);
        memory.insert(address.wrapping_add(offset + 1).get(), low);
    }
    let byte = |offset: u32| {
        let at = address.wrapping_add(offset).get();
        memory.get(&at).copied().unwrap_or(0)
    };
    let words: Vec<u16> = (0..MAX_WORDS as u32)
        .map(|word| u16::from_be_bytes([byte(word * 2), byte(word * 2 + 1)]))
        .collect();
    let instruction = decode(&words).map_err(|_| "undecodable opcode")?;
    // A block ending at 0x1000000 would end at address 0, before its start.
    if address.get() + instruction.length() > Address::MASK {
        return Err("wraps past 0xFFFFFF");
    }
    let lifted = lift(&instruction, address).map_err(|_| "does not lift")?;
    if transfers_to_itself(&lifted) {
        return Err("static transfer to itself");
    }
    let addresses = case
        .initial
        .ram
        .iter()
        .chain(&case.expected.ram)
        .map(|&(address, _)| address & Address::MASK)
        .chain((0..4).map(|offset| address.wrapping_add(offset).get()));
    if aliases(addresses) {
        return Err("RAM mirror alias");
    }
    Ok(Prepared {
        index,
        address,
        instruction,
    })
}

fn raises_address_error(case: &TestCase) -> bool {
    case.transactions.iter().any(|transaction| {
        matches!(
            transaction,
            Transaction::Access {
                kind: TransactionKind::ReadAddressError | TransactionKind::WriteAddressError,
                ..
            }
        )
    })
}

fn transfers_to_itself(lifted: &Lifted) -> bool {
    lifted.body.ops.iter().any(|op| {
        let control = match op {
            Op::Branch { control, .. } | Op::Exit(control) => control,
            _ => return false,
        };
        matches!(
            *control,
            Control::Jump(target) | Control::Call { target, .. } if target == lifted.address
        )
    })
}

/// Whether two of `addresses` share a byte of the mirrored RAM.
fn aliases(addresses: impl Iterator<Item = u32>) -> bool {
    let mut seen: HashMap<u32, u32> = HashMap::new();
    addresses
        .filter(|&address| address >= RAM_START)
        .any(|address| *seen.entry(address & RAM_MASK).or_insert(address) != address)
}

/// Up to [`SAMPLE`] cases, every distinct opcode word first, in file order.
fn sample(eligible: Vec<Prepared>) -> Vec<Prepared> {
    let mut seen = HashSet::new();
    let (mut chosen, rest): (Vec<Prepared>, Vec<Prepared>) = eligible
        .into_iter()
        .partition(|prepared| seen.insert(prepared.instruction.opcode()));
    chosen.truncate(SAMPLE);
    let missing = SAMPLE - chosen.len();
    chosen.extend(rest.into_iter().take(missing));
    chosen.sort_by_key(|prepared| prepared.index);
    chosen
}

/// Emits the one-instruction function of a case.
fn translate(prepared: &Prepared, prefix: &str) -> Output {
    let analysis = single(prepared);
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());
    // Nothing follows the instruction inside the function, so every flag stays live and
    // the comparison can check all of them.
    assert_eq!(
        liveness.stats().total_removed(),
        0,
        "a flag was pruned at {:06x}",
        prepared.address.get()
    );
    // Cycle accounting without check points: the state's count is the instruction's time.
    let options = EmitOptions {
        cycles: true,
        symbol_prefix: prefix.to_owned(),
        ..EmitOptions::default()
    };
    emit(&analysis, &liveness, &options, &[])
}

/// An analysis holding one function of one instruction.
fn single(prepared: &Prepared) -> Analysis {
    let address = prepared.address;
    let block = Block {
        start: address,
        end: address.wrapping_add(prepared.instruction.length()),
        last: address,
        terminator: Terminator::Fallthrough,
        successors: Vec::new(),
        calls: Vec::new(),
        functions: BTreeSet::from([address]),
    };
    let function = Function {
        entry: address,
        origins: vec![Origin::Hint],
        callers: BTreeSet::new(),
        blocks: BTreeSet::from([address]),
        tail_jumps: Vec::new(),
        noreturn: false,
    };
    Analysis {
        sha1: String::new(),
        // No ROM: every access outside RAM goes to the bus.
        rom_size: 0,
        instructions: BTreeMap::from([(address, prepared.instruction)]),
        blocks: BTreeMap::from([(address, block)]),
        functions: BTreeMap::from([(address, function)]),
        jump_tables: Vec::new(),
        unresolved: Vec::new(),
        diagnostics: Vec::new(),
    }
}

fn file_text(output: &Output, name: &str) -> String {
    output
        .files
        .iter()
        .find(|file| file.name == name)
        .map(|file| file.text.clone())
        .unwrap_or_else(|| panic!("the emitter writes {name}"))
}

/// A case's prototypes, function and table as one piece of a shared C file.
fn inline(output: &Output) -> String {
    let mut text = file_text(output, "ag_functions.h");
    for file in output.sources() {
        text.push_str(&file.text.replace("#include \"ag_functions.h\"\n", ""));
    }
    text
}

/// The Rust items naming the sample.
fn items(
    sample: Option<usize>,
    cases: &[(String, usize, String, String)],
    exclusions: &[(String, &str, usize)],
) -> String {
    let mut text = String::from(
        "/// Cases per suite file in the translated sample; `None` without the suite.\n",
    );
    let _ = writeln!(text, "pub const SST_SAMPLE: Option<usize> = {sample:?};\n");
    if !cases.is_empty() {
        text.push_str("unsafe extern \"C\" {\n");
        for name in cases
            .iter()
            .flat_map(|(_, _, function, stepped)| [function, stepped])
        {
            let _ = writeln!(
                text,
                "    fn {name}(state: *mut anygenesis_abi::AgState, pc: u32) -> u32;"
            );
        }
        text.push_str("}\n\n");
    }
    text.push_str(
        "/// The translated cases, in suite order.\npub static SST_CASES: &[SstCase] = &[\n",
    );
    for (file, index, function, stepped) in cases {
        let _ = writeln!(
            text,
            "    SstCase {{ file: {file:?}, index: {index}, function: {function}, \
             stepped: {stepped} }},"
        );
    }
    text.push_str("];\n\n/// The cases left out.\npub static SST_EXCLUDED: &[SstExclusion] = &[\n");
    for (file, reason, count) in exclusions {
        let _ = writeln!(
            text,
            "    SstExclusion {{ file: {file:?}, reason: {reason:?}, count: {count} }},"
        );
    }
    text.push_str("];\n");
    text
}
