//! Flag liveness: which of X, N, Z, V and C something reads at every instruction, so the
//! flag computations nothing reads can be dropped.
//!
//! The pass works on the lifted IR of every analysed instruction. Functions are summarised
//! first (the flags each reads on entry and the flags each writes on every path back to its
//! caller), then one backward fixpoint over all blocks gives the flags live before and after
//! each instruction, with a function's returns seeing what its return points need. Every
//! flag counts as live wherever control goes somewhere the analysis cannot follow:
//! unresolved jumps and calls, targets without a block, RTE and RTR, exception entries and
//! STOP (which push the status register), the returns of functions with callers the
//! analysis does not know, and RTS instructions and exits into other functions reached with
//! an unbalanced stack.

mod program;
mod stack;
mod summary;

use std::collections::{BTreeMap, BTreeSet};

use anygenesis_ir::{Body, Flag, FlagSet, Lifted, Op, flags};
use anygenesis_shared::Address;

use crate::{Analysis, Block, Function, Origin};
use program::{Context, Program};
use summary::Summaries;

/// Choices the pass leaves to its caller.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LivenessOptions {
    /// Treats every block start as a read of the whole status register, as an interrupt
    /// check point there would push it. Off by default: an interrupt frame may then hold
    /// stale values for flags nothing reads (PROGRESS.md, M2.3).
    pub check_points_read_sr: bool,
    /// Addresses whose instruction is treated as reading every flag first, as a code patch
    /// run before it may (M6.2).
    pub read_flags_at: BTreeSet<Address>,
}

/// What a function does with the flags, as seen from its callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlagSummary {
    /// The flags it may read before writing them, its callees included.
    pub reads: FlagSet,
    /// The flags it writes on every path that returns to its caller.
    pub defines: FlagSet,
    /// The flags live after its returns: what its return points need.
    pub return_live: FlagSet,
    /// Whether it can be entered from somewhere the analysis does not know (a vector, a
    /// hint, a table without a site, or any function while sites stay unresolved).
    pub unknown_callers: bool,
}

/// Flag op counts, by flag in [`Flag::ALL`] order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FlagStats {
    /// `SetFlag` ops before pruning.
    pub ops: [u64; 5],
    /// `SetFlag` ops pruning removed.
    pub removed: [u64; 5],
}

impl FlagStats {
    /// Counts the `SetFlag` ops of `original` and the ones `pruned` no longer has.
    pub fn add(&mut self, original: &Body, pruned: &Body) {
        let before = set_flag_counts(original);
        let after = set_flag_counts(pruned);
        for index in 0..Flag::ALL.len() {
            self.ops[index] += before[index];
            self.removed[index] += before[index] - after[index];
        }
    }

    /// Adds the counts of `other`.
    pub fn merge(&mut self, other: &Self) {
        for index in 0..Flag::ALL.len() {
            self.ops[index] += other.ops[index];
            self.removed[index] += other.removed[index];
        }
    }

    pub fn total_ops(&self) -> u64 {
        self.ops.iter().sum()
    }

    pub fn total_removed(&self) -> u64 {
        self.removed.iter().sum()
    }
}

fn set_flag_counts(body: &Body) -> [u64; 5] {
    let mut counts = [0; 5];
    for op in &body.ops {
        if let Op::SetFlag { flag, .. } = op {
            // Flag::ALL lists every flag.
            let index = Flag::ALL
                .iter()
                .position(|known| known == flag)
                .unwrap_or(0);
            counts[index] += 1;
        }
    }
    counts
}

/// The flag liveness of an analysed program.
#[derive(Debug)]
pub struct FlagLiveness<'a> {
    program: Program<'a>,
    facts: Facts,
    options: LivenessOptions,
}

/// The solved state the final liveness queries read.
#[derive(Debug)]
struct Facts {
    summaries: Summaries,
    unbalanced: BTreeSet<Address>,
    unknown_callers: BTreeSet<Address>,
    live: BTreeMap<Address, FlagSet>,
    return_live: BTreeMap<Address, FlagSet>,
    check_points_read_sr: bool,
    read_flags_at: BTreeSet<Address>,
}

impl<'a> FlagLiveness<'a> {
    /// Lifts every instruction of `analysis` and solves flag liveness over it.
    pub fn new(analysis: &'a Analysis, options: LivenessOptions) -> Self {
        let program = Program::new(analysis);
        let unbalanced = stack::unbalanced_exits(&program);
        let summaries = summary::summaries(&program, &unbalanced, &options);
        let unknown_callers: BTreeSet<Address> = analysis
            .functions
            .values()
            .filter(|function| has_unknown_callers(&program, function))
            .map(|function| function.entry)
            .collect();
        let mut facts = Facts {
            summaries,
            unbalanced,
            live: program
                .sites
                .keys()
                .map(|&address| (address, FlagSet::EMPTY))
                .collect(),
            return_live: analysis
                .functions
                .keys()
                .map(|&entry| {
                    let live = if unknown_callers.contains(&entry) {
                        FlagSet::ALL
                    } else {
                        FlagSet::EMPTY
                    };
                    (entry, live)
                })
                .collect(),
            unknown_callers,
            check_points_read_sr: options.check_points_read_sr,
            read_flags_at: options.read_flags_at.clone(),
        };
        while facts.update(&program) {}
        Self {
            program,
            facts,
            options,
        }
    }

    pub fn options(&self) -> &LivenessOptions {
        &self.options
    }

    /// Returns the flags live before the instruction at `address`, `None` when it is not
    /// an analysed instruction.
    pub fn live_in(&self, address: Address) -> Option<FlagSet> {
        self.facts.live.get(&address).copied()
    }

    /// Returns the flags live when the instruction at `address` falls through (empty when
    /// it never does).
    pub fn live_out(&self, address: Address) -> Option<FlagSet> {
        let site = self.program.sites.get(&address)?;
        Some(self.program.live_out(site, &self.facts))
    }

    /// Returns the summary of the function at `entry`.
    pub fn summary(&self, entry: Address) -> Option<FlagSummary> {
        let (reads, defines) = self.facts.summaries.get(entry)?;
        Some(FlagSummary {
            reads,
            defines,
            return_live: self.facts.return_live[&entry],
            unknown_callers: self.facts.unknown_callers.contains(&entry),
        })
    }

    /// Returns the RTS instructions and the exits into other functions reached with an
    /// unbalanced stack, where every flag is live.
    pub fn unbalanced_exits(&self) -> impl Iterator<Item = Address> + '_ {
        self.facts.unbalanced.iter().copied()
    }

    /// Returns the instruction at `address` as lifted, before pruning.
    pub fn lifted(&self, address: Address) -> Option<&Lifted> {
        self.program.sites.get(&address).map(|site| &site.lifted)
    }

    /// Returns `lifted` without the flag computations nothing reads. An instruction that
    /// is not analysed comes back unchanged.
    pub fn prune(&self, lifted: &Lifted) -> Lifted {
        let Some(site) = self.program.sites.get(&lifted.address) else {
            return lifted.clone();
        };
        let live_out = self.program.live_out(site, &self.facts);
        let body = flags::prune(&lifted.body, live_out, |control| {
            self.program
                .exit(lifted.address, site, control, &self.facts)
        });
        Lifted {
            body,
            ..lifted.clone()
        }
    }

    /// Counts the flag ops of every analysed instruction and how many pruning removes.
    pub fn stats(&self) -> FlagStats {
        let mut stats = FlagStats::default();
        for site in self.program.sites.values() {
            stats.add(&site.lifted.body, &self.prune(&site.lifted).body);
        }
        stats
    }
}

impl Facts {
    /// Recomputes every instruction's live flags and every function's return flags once,
    /// and returns whether anything changed.
    fn update(&mut self, program: &Program) -> bool {
        let mut changed = false;
        for (&address, site) in program.sites.iter().rev() {
            let live = program.live_in(address, site, self);
            changed |= self.live.insert(address, live) != Some(live);
        }
        let functions = &program.analysis.functions;
        for call in program.calls.values() {
            let after = self.target(call.return_address);
            for target in &call.targets {
                if !functions[target].noreturn {
                    changed |= self.add_return_live(*target, after);
                }
            }
        }
        for function in functions.values() {
            let inherited = self.return_live[&function.entry];
            for tail in &function.tail_jumps {
                changed |= self.add_return_live(tail.target, inherited);
            }
        }
        changed
    }

    fn add_return_live(&mut self, entry: Address, flags: FlagSet) -> bool {
        let live = self.return_live[&entry] | flags;
        self.return_live.insert(entry, live) != Some(live)
    }
}

impl Context for Facts {
    fn target(&self, target: Address) -> FlagSet {
        self.live.get(&target).copied().unwrap_or(FlagSet::ALL)
    }

    fn returning(&self, block: &Block) -> FlagSet {
        if block.functions.is_empty() {
            return FlagSet::ALL;
        }
        block
            .functions
            .iter()
            .fold(FlagSet::EMPTY, |live, entry| live | self.return_live[entry])
    }

    fn summary(&self, entry: Address) -> Option<(FlagSet, FlagSet)> {
        self.summaries.get(entry)
    }

    fn unbalanced(&self, site: Address) -> bool {
        self.unbalanced.contains(&site)
    }

    fn check_points_read_sr(&self) -> bool {
        self.check_points_read_sr
    }

    fn reads_flags(&self, address: Address) -> bool {
        self.read_flags_at.contains(&address)
    }
}

/// Returns whether `function` can be entered from somewhere whose needs the analysis does
/// not see. A table or indirect hint counts as known only when its site was analysed and
/// leads to the function as a call or a tail jump.
fn has_unknown_callers(program: &Program, function: &Function) -> bool {
    let analysis = program.analysis;
    let known_site = |site: Address| {
        let called = program
            .calls
            .get(&site)
            .is_some_and(|call| call.targets.contains(&function.entry));
        let jumped = analysis.functions.values().any(|caller| {
            caller
                .tail_jumps
                .iter()
                .any(|tail| tail.site == site && tail.target == function.entry)
        });
        called || jumped
    };
    !analysis.unresolved.is_empty()
        || function.origins.iter().any(|origin| match *origin {
            Origin::Reset | Origin::Vector(_) | Origin::Hint => true,
            Origin::Table { table } => !analysis
                .jump_tables
                .iter()
                .find(|resolved| resolved.address == table)
                .and_then(|resolved| resolved.site)
                .is_some_and(known_site),
            Origin::Indirect { site } => !known_site(site),
        })
}
