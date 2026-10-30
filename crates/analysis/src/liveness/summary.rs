//! Per-function summaries: the flags a function reads before writing them, and the flags it
//! writes on every path that returns to its caller. Both are solved to a fixpoint over the
//! call graph, so recursion and tail jumps are covered.

use std::collections::{BTreeMap, BTreeSet};

use anygenesis_ir::{Control, FlagSet, ReturnKind, flags};
use anygenesis_shared::Address;

use super::LivenessOptions;
use super::program::{Context, Program};
use crate::{Block, Function};

/// The summary of every function, by entry.
#[derive(Debug)]
pub(super) struct Summaries {
    pub reads: BTreeMap<Address, FlagSet>,
    pub defines: BTreeMap<Address, FlagSet>,
}

impl Summaries {
    pub fn get(&self, entry: Address) -> Option<(FlagSet, FlagSet)> {
        self.reads
            .get(&entry)
            .copied()
            .zip(self.defines.get(&entry).copied())
    }
}

/// Solves the summaries. `defines` is a must property, so it starts from every flag and
/// shrinks (the greatest fixpoint); `reads` starts empty and grows.
pub(super) fn summaries(
    program: &Program,
    unbalanced: &BTreeSet<Address>,
    options: &LivenessOptions,
) -> Summaries {
    let functions = &program.analysis.functions;
    let mut summaries = Summaries {
        reads: functions
            .keys()
            .map(|&entry| (entry, FlagSet::EMPTY))
            .collect(),
        defines: functions
            .keys()
            .map(|&entry| (entry, FlagSet::ALL))
            .collect(),
    };
    let sites: BTreeMap<Address, Vec<Address>> = functions
        .values()
        .map(|function| (function.entry, program.function_sites(function)))
        .collect();
    solve(
        functions.values(),
        &mut summaries.defines,
        |function, defines| {
            function_defines(
                program,
                function,
                &sites[&function.entry],
                defines,
                unbalanced,
            )
        },
    );
    let defines = summaries.defines.clone();
    solve(
        functions.values(),
        &mut summaries.reads,
        |function, reads| {
            let mut context = Local {
                program,
                function: function.entry,
                live: BTreeMap::new(),
                reads,
                defines: &defines,
                unbalanced,
                options,
            };
            context.solve(&sites[&function.entry])
        },
    );
    summaries
}

/// Recomputes every function's value until none changes.
fn solve<'f>(
    functions: impl Iterator<Item = &'f Function> + Clone,
    values: &mut BTreeMap<Address, FlagSet>,
    mut compute: impl FnMut(&Function, &BTreeMap<Address, FlagSet>) -> FlagSet,
) {
    loop {
        let mut changed = false;
        for function in functions.clone() {
            let value = compute(function, values);
            if values.insert(function.entry, value) != Some(value) {
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// Returns the flags `function` writes on every path to a return into its caller, given
/// the callees' current values.
fn function_defines(
    program: &Program,
    function: &Function,
    sites: &[Address],
    defines: &BTreeMap<Address, FlagSet>,
    unbalanced: &BTreeSet<Address>,
) -> FlagSet {
    let analysis = program.analysis;
    let local: BTreeSet<Address> = sites.iter().copied().collect();
    let mut written = BTreeMap::from([(function.entry, FlagSet::EMPTY)]);
    let mut pending = vec![function.entry];
    let mut returned = FlagSet::ALL;
    while let Some(address) = pending.pop() {
        let site = &program.sites[&address];
        let block = program.block(site);
        let before = written[&address];
        let mut flows = Vec::new();
        let through = flags::written(&site.lifted.body, |control, flags| {
            let flags = before | flags;
            match *control {
                Control::Jump(target) => flows.push((target, flags)),
                Control::JumpIndirect(_) => {
                    flows.extend(block.successors.iter().map(|edge| (edge.target, flags)));
                }
                Control::Call { .. } | Control::CallIndirect { .. } => {
                    let call = &program.calls[&address];
                    if let Some(callee) = callee_defines(program, &call.targets, defines) {
                        flows.push((call.return_address, flags | callee));
                    }
                }
                Control::Return {
                    kind: ReturnKind::Rts,
                    ..
                } if !unbalanced.contains(&address) => returned = returned & flags,
                Control::Return {
                    kind: ReturnKind::Rtr,
                    ..
                } => returned = returned & flags,
                Control::Exception { pc, .. } => flows.push((pc, flags)),
                Control::Stop { resume } => flows.push((resume, flags)),
                Control::Return { .. } => {}
            }
        });
        if site.lifted.body.falls_through() {
            flows.push((site.lifted.next(), before | through));
        }
        for (target, flags) in flows {
            if analysis.is_tail_jump(function.entry, target) {
                if !analysis.functions[&target].noreturn {
                    returned = returned & (flags | defines[&target]);
                }
            } else if local.contains(&target) {
                let merged = written.get(&target).map_or(flags, |&known| known & flags);
                if written.insert(target, merged) != Some(merged) {
                    pending.push(target);
                }
            }
        }
    }
    returned
}

/// Returns the flags every callee writes before returning, or `None` when none of them
/// returns. Unknown callees write nothing for sure.
fn callee_defines(
    program: &Program,
    targets: &[Address],
    defines: &BTreeMap<Address, FlagSet>,
) -> Option<FlagSet> {
    if targets.is_empty() {
        return Some(FlagSet::EMPTY);
    }
    let functions = &program.analysis.functions;
    targets
        .iter()
        .filter(|target| !functions[target].noreturn)
        .map(|target| defines[target])
        .reduce(|left, right| left & right)
}

/// The context of one function's local liveness, with every return treated as returning
/// to a caller that needs nothing: the caller's needs are added at the call site.
struct Local<'p> {
    program: &'p Program<'p>,
    function: Address,
    live: BTreeMap<Address, FlagSet>,
    reads: &'p BTreeMap<Address, FlagSet>,
    defines: &'p BTreeMap<Address, FlagSet>,
    unbalanced: &'p BTreeSet<Address>,
    options: &'p LivenessOptions,
}

impl Local<'_> {
    /// Returns the flags live on entry to the function.
    fn solve(&mut self, sites: &[Address]) -> FlagSet {
        self.live = sites.iter().map(|&site| (site, FlagSet::EMPTY)).collect();
        loop {
            let mut changed = false;
            for &address in sites.iter().rev() {
                let live = self
                    .program
                    .live_in(address, &self.program.sites[&address], self);
                if self.live.insert(address, live) != Some(live) {
                    changed = true;
                }
            }
            if !changed {
                return self.live[&self.function];
            }
        }
    }
}

impl Context for Local<'_> {
    fn target(&self, target: Address) -> FlagSet {
        if self.program.analysis.is_tail_jump(self.function, target) {
            self.reads[&target]
        } else {
            self.live.get(&target).copied().unwrap_or(FlagSet::ALL)
        }
    }

    fn returning(&self, _block: &Block) -> FlagSet {
        FlagSet::EMPTY
    }

    fn summary(&self, entry: Address) -> Option<(FlagSet, FlagSet)> {
        self.reads
            .get(&entry)
            .copied()
            .zip(self.defines.get(&entry).copied())
    }

    fn unbalanced(&self, site: Address) -> bool {
        self.unbalanced.contains(&site)
    }

    fn check_points_read_sr(&self) -> bool {
        self.options.check_points_read_sr
    }

    fn reads_flags(&self, address: Address) -> bool {
        self.options.read_flags_at.contains(&address)
    }
}
