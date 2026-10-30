use std::collections::BTreeMap;

use anygenesis_ir::{Control, FlagSet, Lifted, Op, ReturnKind, flags, lift};
use anygenesis_shared::Address;

use crate::{Analysis, Block, Function};

/// One analysed instruction, lifted once, with the block that holds it.
#[derive(Debug)]
pub(super) struct Site {
    pub lifted: Lifted,
    pub block: Address,
    /// Whether the instruction starts its block (an interrupt check point).
    pub block_start: bool,
}

/// A call made by one instruction.
#[derive(Debug)]
pub(super) struct CallSite {
    pub targets: Vec<Address>,
    pub return_address: Address,
}

/// The analysed program as liveness sees it: every instruction lifted, and the call sites.
#[derive(Debug)]
pub(super) struct Program<'a> {
    pub analysis: &'a Analysis,
    pub sites: BTreeMap<Address, Site>,
    pub calls: BTreeMap<Address, CallSite>,
}

/// What a liveness pass knows beyond the instruction itself.
pub(super) trait Context {
    /// Returns the flags live on arrival at `target`.
    fn target(&self, target: Address) -> FlagSet;

    /// Returns the flags live after a balanced RTS that ends `block`.
    fn returning(&self, block: &Block) -> FlagSet;

    /// Returns `(reads, defines)` of the function at `entry`, `None` if there is none.
    fn summary(&self, entry: Address) -> Option<(FlagSet, FlagSet)>;

    /// Returns whether the RTS at `site`, or the instruction there that leads into another
    /// function, is reached with an unbalanced stack.
    fn unbalanced(&self, site: Address) -> bool;

    /// Returns whether every block start reads the whole status register.
    fn check_points_read_sr(&self) -> bool;
}

impl<'a> Program<'a> {
    pub fn new(analysis: &'a Analysis) -> Self {
        let mut sites = BTreeMap::new();
        let mut calls = BTreeMap::new();
        for block in analysis.blocks.values() {
            for (&address, instruction) in analysis.instructions.range(block.start..block.end) {
                // Every decoded instruction lifts: the exhaustive lifting test covers them.
                let lifted = lift(instruction, address).expect("decoded instructions always lift");
                if let Some(call) = call_site(analysis, block, address, &lifted) {
                    calls.insert(address, call);
                }
                sites.insert(
                    address,
                    Site {
                        lifted,
                        block: block.start,
                        block_start: address == block.start,
                    },
                );
            }
        }
        Self {
            analysis,
            sites,
            calls,
        }
    }

    pub fn block(&self, site: &Site) -> &'a Block {
        &self.analysis.blocks[&site.block]
    }

    /// Returns the addresses of the instructions in `function`'s blocks, in ROM order.
    pub fn function_sites(&self, function: &Function) -> Vec<Address> {
        function
            .blocks
            .iter()
            .flat_map(|start| {
                let block = &self.analysis.blocks[start];
                self.sites
                    .range(block.start..block.end)
                    .map(|(&address, _)| address)
            })
            .collect()
    }

    /// Returns the flags live before the instruction at `address`.
    pub fn live_in(&self, address: Address, site: &Site, context: &impl Context) -> FlagSet {
        let live_out = self.live_out(site, context);
        let live = flags::live_in(&site.lifted.body, live_out, |control| {
            self.exit(address, site, control, context)
        });
        if site.block_start && context.check_points_read_sr() {
            FlagSet::ALL
        } else {
            live
        }
    }

    /// Returns the flags live when the instruction falls through. After a write to SR an
    /// interrupt may be taken at once, so with check points reading SR its frame needs all;
    /// falling into another function after a popped caller returns past the return point.
    pub fn live_out(&self, site: &Site, context: &impl Context) -> FlagSet {
        let body = &site.lifted.body;
        let unbalanced = context.unbalanced(site.lifted.address);
        if body.falls_through()
            && (unbalanced || (body.writes_sr() && context.check_points_read_sr()))
        {
            FlagSet::ALL
        } else if body.falls_through() {
            context.target(site.lifted.next())
        } else {
            FlagSet::EMPTY
        }
    }

    /// Returns the flags live where `control` leads from the instruction at `address`.
    pub fn exit(
        &self,
        address: Address,
        site: &Site,
        control: &Control,
        context: &impl Context,
    ) -> FlagSet {
        let block = self.block(site);
        match *control {
            // Another function entered after a popped caller returns past the return point.
            Control::Jump(_) | Control::JumpIndirect(_) if context.unbalanced(address) => {
                FlagSet::ALL
            }
            Control::Jump(target) => context.target(target),
            Control::JumpIndirect(_) => {
                if block.successors.is_empty() {
                    return FlagSet::ALL;
                }
                block.successors.iter().fold(FlagSet::EMPTY, |live, edge| {
                    live | context.target(edge.target)
                })
            }
            Control::Call { .. } | Control::CallIndirect { .. } => self
                .calls
                .get(&address)
                .map_or(FlagSet::ALL, |call| self.call(call, context)),
            Control::Return {
                kind: ReturnKind::Rts,
                ..
            } if !context.unbalanced(address) => context.returning(block),
            // RTE and RTR continue wherever the stack says, an exception entry and STOP's
            // interrupt push the whole status register.
            Control::Return { .. } | Control::Exception { .. } | Control::Stop { .. } => {
                FlagSet::ALL
            }
        }
    }

    /// Returns the flags live before a call: what the callees read, and what is live after
    /// it that they may leave alone.
    fn call(&self, call: &CallSite, context: &impl Context) -> FlagSet {
        if call.targets.is_empty() {
            return FlagSet::ALL;
        }
        let mut reads = FlagSet::EMPTY;
        let mut defines = FlagSet::ALL;
        let mut returns = false;
        for &target in &call.targets {
            let Some((callee_reads, callee_defines)) = context.summary(target) else {
                return FlagSet::ALL;
            };
            reads |= callee_reads;
            defines = defines & callee_defines;
            returns |= !self.analysis.functions[&target].noreturn;
        }
        let after = if returns {
            context.target(call.return_address)
        } else {
            FlagSet::EMPTY
        };
        reads | (after - defines)
    }
}

/// Returns the call the instruction at `address` makes, with the targets the analysis
/// found for it.
fn call_site(
    analysis: &Analysis,
    block: &Block,
    address: Address,
    lifted: &Lifted,
) -> Option<CallSite> {
    let (direct, return_address) = match lifted.body.ops.last() {
        Some(Op::Exit(Control::Call {
            target,
            return_address,
        })) => (Some(*target), *return_address),
        Some(Op::Exit(Control::CallIndirect { return_address, .. })) => (None, *return_address),
        _ => return None,
    };
    let targets = match direct {
        Some(target) => vec![target],
        None => block
            .calls
            .iter()
            .find(|call| call.site == address)
            .map(|call| call.targets.clone())
            .unwrap_or_default(),
    };
    // A call target the analysis has no function for (RAM, bad addresses) leaves the
    // callee unknown.
    let targets = if targets
        .iter()
        .all(|target| analysis.functions.contains_key(target))
    {
        targets
    } else {
        Vec::new()
    };
    Some(CallSite {
        targets,
        return_address,
    })
}
