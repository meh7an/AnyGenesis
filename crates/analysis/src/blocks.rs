use std::collections::{BTreeMap, BTreeSet};

use anygenesis_m68k::Instruction;
use anygenesis_shared::Address;

use crate::{
    Block, Diagnostic, Edge, EdgeKind, Function, Hints, TailJump, Terminator,
    discover::{Effect, Entry},
};

/// Cuts the decoded instructions into blocks at every leader and block-ending instruction.
pub(crate) fn build_blocks(
    instructions: &BTreeMap<Address, Instruction>,
    effects: &BTreeMap<Address, Effect>,
    leaders: &BTreeSet<Address>,
) -> BTreeMap<Address, Block> {
    leaders
        .iter()
        .filter(|leader| instructions.contains_key(leader))
        .map(|&start| (start, build_block(instructions, effects, leaders, start)))
        .collect()
}

fn build_block(
    instructions: &BTreeMap<Address, Instruction>,
    effects: &BTreeMap<Address, Effect>,
    leaders: &BTreeSet<Address>,
    start: Address,
) -> Block {
    let mut address = start;
    let mut calls = Vec::new();
    loop {
        let length = instructions.get(&address).map_or(0, Instruction::length);
        let next = address.wrapping_add(length);
        let effect = effects.get(&address);
        calls.extend(effect.and_then(|effect| effect.call.clone()));
        let ends_here = effect.and_then(|effect| effect.terminator);
        if ends_here.is_some() || leaders.contains(&next) || !instructions.contains_key(&next) {
            // Without a recorded terminator the run stops only at a leader; the descent
            // marks every other stop as invalid.
            let terminator = ends_here.unwrap_or(Terminator::Fallthrough);
            let mut successors = effect
                .map(|effect| effect.edges.clone())
                .unwrap_or_default();
            if terminator.falls_through() {
                successors.push(Edge {
                    target: next,
                    kind: EdgeKind::Fallthrough,
                });
            }
            return Block {
                start,
                end: next,
                last: address,
                terminator,
                successors,
                calls,
                functions: BTreeSet::new(),
            };
        }
        address = next;
    }
}

/// Assigns blocks to the functions that reach them and records tail jumps.
pub(crate) fn build_functions(
    blocks: &mut BTreeMap<Address, Block>,
    entries: BTreeMap<Address, Entry>,
    hints: &Hints,
) -> BTreeMap<Address, Function> {
    let entry_set: BTreeSet<Address> = entries
        .keys()
        .copied()
        .filter(|entry| blocks.contains_key(entry))
        .collect();
    entries
        .into_iter()
        .filter(|(entry, _)| entry_set.contains(entry))
        .map(|(entry, found)| {
            let mut function = Function {
                entry,
                origins: found.origins,
                callers: found.callers,
                blocks: BTreeSet::new(),
                tail_jumps: Vec::new(),
                noreturn: hints.function(entry).is_some_and(|hint| hint.noreturn),
            };
            let mut pending = vec![entry];
            while let Some(start) = pending.pop() {
                let Some(block) = blocks.get_mut(&start) else {
                    continue;
                };
                if !function.blocks.insert(start) {
                    continue;
                }
                block.functions.insert(entry);
                for edge in &block.successors {
                    if edge.target != entry && entry_set.contains(&edge.target) {
                        function.tail_jumps.push(TailJump {
                            site: block.last,
                            target: edge.target,
                        });
                    } else {
                        pending.push(edge.target);
                    }
                }
            }
            function
                .tail_jumps
                .sort_by_key(|tail| (tail.site, tail.target));
            function.tail_jumps.dedup();
            (entry, function)
        })
        .collect()
}

/// Lists the blocks no function reaches.
pub(crate) fn unowned_blocks(blocks: &BTreeMap<Address, Block>) -> Vec<Diagnostic> {
    blocks
        .values()
        .filter(|block| block.functions.is_empty())
        .map(|block| Diagnostic::UnownedBlock { start: block.start })
        .collect()
}
