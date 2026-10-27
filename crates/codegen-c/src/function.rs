//! One analysed function as one C function.
//!
//! Blocks become goto labels and every instruction a braced group of statements. Control
//! leaving the function is a guaranteed tail call (jumps) or a native call whose returned
//! address is checked (calls, exceptions, STOP): a mismatch returns that address to the
//! caller, which checks it in turn, so a popped caller resumes natively one level up and
//! anything no level expects reaches the runtime's top-level loop. Registers and flags live
//! in the state struct, so continuing at whichever level matches is exact.

use std::collections::{BTreeMap, BTreeSet};

use anygenesis_analysis::{Block, Function};
use anygenesis_ir::{Control, Lifted, Op, ReturnKind, Value, exception_entry};
use anygenesis_m68k::{Dialect, FormatOptions};
use anygenesis_shared::Address;

use crate::{
    CheckPoints,
    printer::{ADDRESS_MASK, Printer, constant, used_temps},
    program::Context,
};

/// The C text of one function and the addresses it can resume at besides its entry.
#[derive(Debug)]
pub struct Emitted {
    pub text: String,
    pub resume: BTreeSet<Address>,
}

/// Returns the C name of the function entered at `entry`, after the symbol prefix.
pub fn function_name(prefix: &str, entry: Address) -> String {
    format!("{prefix}f_{:06X}", entry.get())
}

/// Returns the C prototype of the function entered at `entry`, without a semicolon.
pub fn prototype(prefix: &str, entry: Address) -> String {
    format!(
        "uint32_t {}(AgState *s, uint32_t pc)",
        function_name(prefix, entry)
    )
}

fn label(address: Address) -> String {
    format!("L_{:06X}", address.get())
}

/// Emits `function`.
pub fn emit_function(context: &Context<'_>, function: &Function) -> Emitted {
    let mut emitter = Emitter::new(context, function);
    emitter.body();
    emitter.finish()
}

/// One instruction in emission order.
#[derive(Debug)]
struct Slot<'a> {
    lifted: Lifted,
    block: &'a Block,
}

/// A piece of the function body: label positions are only decided once every jump is known.
#[derive(Debug)]
enum Part {
    Anchor(Address),
    Line(String),
}

#[derive(Debug)]
struct Emitter<'a> {
    context: &'a Context<'a>,
    function: &'a Function,
    slots: Vec<Slot<'a>>,
    owned: BTreeMap<Address, usize>,
    check_points: BTreeSet<Address>,
    labels: BTreeSet<Address>,
    resume: BTreeSet<Address>,
    parts: Vec<Part>,
    /// [`Printer::ahead`] for an exception frame taken by the current instruction: its own
    /// cycles on the path that leaves, data-dependent ones included.
    exception_ahead: String,
}

impl<'a> Emitter<'a> {
    fn new(context: &'a Context<'a>, function: &'a Function) -> Self {
        let analysis = context.analysis;
        let mut slots = Vec::new();
        for block in function
            .blocks
            .iter()
            .filter_map(|start| analysis.blocks.get(start))
        {
            let mut address = block.start;
            loop {
                let lifted = context.pruned(address);
                let next = lifted.next();
                slots.push(Slot { lifted, block });
                if address == block.last {
                    break;
                }
                address = next;
            }
        }
        let owned = slots
            .iter()
            .enumerate()
            .map(|(index, slot)| (slot.lifted.address, index))
            .collect();
        let check_points = check_points(context, function);
        Self {
            context,
            function,
            slots,
            owned,
            check_points,
            labels: BTreeSet::new(),
            resume: BTreeSet::new(),
            parts: Vec::new(),
            exception_ahead: NOTHING_AHEAD.to_owned(),
        }
    }

    fn line(&mut self, indent: usize, text: impl AsRef<str>) {
        self.parts
            .push(Part::Line(format!("{:indent$}{}", "", text.as_ref())));
    }

    fn lines(&mut self, indent: usize, lines: Vec<String>) {
        for text in lines {
            self.line(indent, text);
        }
    }

    fn body(&mut self) {
        let first = self.slots.first().map(|slot| slot.lifted.address);
        if first != Some(self.function.entry) {
            let lines = self.transfer(self.function.entry);
            self.lines(4, lines);
        }
        for index in 0..self.slots.len() {
            self.instruction(index);
        }
    }

    fn instruction(&mut self, index: usize) {
        let slot = &self.slots[index];
        let address = slot.lifted.address;
        let block = slot.block;
        let block_start = address == block.start;
        self.parts.push(Part::Anchor(address));
        if block_start && self.check_points.contains(&address) {
            // The entry needs no resume label: a call with the entry address starts there.
            if address != self.function.entry {
                self.resume.insert(address);
                self.labels.insert(address);
            }
            let pc = constant(address.get());
            if self.context.options.cycles {
                let bound = self.block_bound(index);
                self.line(4, format!("AG_HCHECK(s, {pc}, {bound});"));
            } else {
                self.line(4, format!("AG_CHECK(s, {pc});"));
            }
        }
        if self.context.options.cycles && self.starts_segment(index) {
            let cost = self.segment_cost(index);
            self.line(4, format!("AG_CYCLES(s, {cost});"));
        }
        self.comment(index);
        let next = self.slots.get(index + 1).map(|slot| slot.lifted.address);
        let slot = &self.slots[index];
        let base = static_cost(&slot.lifted);
        let cycles = slot.lifted.cycles;
        let charging = self.context.options.cycles;
        let body = if charging {
            slot.lifted.body.clone()
        } else {
            slot.lifted.body.without_cycles()
        };
        // The segment's cycles were charged at its start; from this instruction on they lie
        // past the instruction's start.
        let ahead = if charging {
            self.segment_cost(index).to_string()
        } else {
            NOTHING_AHEAD.to_owned()
        };
        let printer = Printer {
            body: &body,
            prefix: "t",
            rom_size: self.context.analysis.rom_size,
            ahead: &ahead,
        };
        let used = used_temps(&body);
        let mut charged = Vec::new();
        if !body.ops.is_empty() {
            self.line(4, "{");
        }
        for op in &body.ops {
            match *op {
                Op::Branch { condition, control } => {
                    self.line(8, format!("if ({}) {{", printer.value(condition)));
                    // The segment charged the instruction's own path; this one may differ.
                    let path = cycles.leaving(&control);
                    if charging && path != base {
                        let extra = i32::from(path) - i32::from(base);
                        self.line(12, format!("AG_CYCLES(s, {extra});"));
                    }
                    self.exception_ahead = leaving_ahead(charging, path, &charged);
                    let lines = self.control(control, &printer, index, None);
                    self.lines(12, lines);
                    self.line(8, "}");
                }
                Op::Exit(control) => {
                    let path = cycles.leaving(&control);
                    self.exception_ahead = leaving_ahead(charging, path, &charged);
                    let lines = self.control(control, &printer, index, next);
                    self.lines(8, lines);
                }
                Op::Cycles { value } => {
                    charged.push(printer.value(value));
                    if let Some(statement) = printer.op(op, &used) {
                        self.line(8, statement);
                    }
                }
                _ => {
                    if let Some(statement) = printer.op(op, &used) {
                        self.line(8, statement);
                    }
                }
            }
        }
        if !body.ops.is_empty() {
            self.line(4, "}");
        }
        if body.falls_through() && body.writes_sr() {
            let after = constant(self.slots[index].lifted.next().get());
            self.line(4, format!("AG_DELIVER(s, {after});"));
        }
        if address == block.last && body.falls_through() {
            let lines = self.continue_at(block.end, next);
            self.lines(4, lines);
        }
    }

    /// Writes the address and disassembly, and the `#line` directive when enabled.
    fn comment(&mut self, index: usize) {
        let address = self.slots[index].lifted.address;
        let text = self
            .context
            .analysis
            .instructions
            .get(&address)
            .map(|instruction| {
                instruction
                    .format(address, FormatOptions::new(Dialect::Motorola))
                    .to_string()
            })
            .unwrap_or_default();
        self.line(4, format!("/* {:06X}  {text} */", address.get()));
        if let Some(lines) = &self.context.options.lines
            && let Some(line) = lines.lines.get(&address)
        {
            let directive = format!("#line {line} \"{}\"", escape(&lines.file));
            self.parts.push(Part::Line(directive));
        }
    }

    /// The most cycles the block can take from its start at `index` to its end, for
    /// `AG_HCHECK`: a budget holding more cannot end inside it.
    fn block_bound(&self, index: usize) -> u32 {
        let block = self.slots[index].block;
        self.slots[index..]
            .iter()
            .take_while(|slot| std::ptr::eq(slot.block, block))
            .map(|slot| slot.lifted.max_cycles())
            .sum()
    }

    /// Cycles of the instructions from `index` up to the next one that can leave early, as
    /// they cost when nothing leaves.
    fn segment_cost(&self, index: usize) -> i32 {
        let mut cost = 0;
        for at in index..self.slots.len() {
            if at != index && self.starts_segment(at) {
                break;
            }
            cost += i32::from(static_cost(&self.slots[at].lifted));
        }
        cost
    }

    /// Whether the instruction at `index` starts a segment, which charges the static cycles
    /// up to the next one: at a block start, after an instruction that can leave early, and
    /// where an exception taken by the instruction itself resumes (its frame holds the
    /// instruction's own address: a privilege violation, ILLEGAL, line A and F).
    fn starts_segment(&self, index: usize) -> bool {
        let slot = &self.slots[index];
        index == 0
            || slot.lifted.address == slot.block.start
            || ends_segment(&self.slots[index - 1])
            || resumes_at_itself(&slot.lifted)
    }

    /// Returns the statements for `control`; `next` is the address emitted right after this
    /// instruction when `control` is the instruction's last op, `None` inside a branch.
    fn control(
        &mut self,
        control: Control,
        printer: &Printer<'_>,
        index: usize,
        next: Option<Address>,
    ) -> Vec<String> {
        match control {
            Control::Jump(target) => self.continue_at(target, next),
            Control::JumpIndirect(value) => self.indirect_jump(printer.value(value), index),
            Control::Call {
                target,
                return_address,
            } => self.call(target, return_address, next),
            Control::CallIndirect {
                target,
                return_address,
            } => self.indirect_call(printer.value(target), index, return_address, next),
            // RTE may lower the mask: an interrupt it lets through is taken at its target.
            Control::Return {
                kind: ReturnKind::Rte,
                target,
            } => {
                vec![format!(
                    "return ag_deliver(s, {} & {});",
                    printer.value(target),
                    constant(ADDRESS_MASK)
                )]
            }
            Control::Return { target, .. } => {
                vec![format!(
                    "return {} & {};",
                    printer.value(target),
                    constant(ADDRESS_MASK)
                )]
            }
            Control::Exception { vector, pc } => self.exception(vector, pc, next),
            Control::Stop { resume } => {
                let call = format!("ag_stop(s, {})", constant(resume.get()));
                self.native(Vec::new(), call, resume, next)
            }
        }
    }

    /// Continues at `target`, emitting nothing when it is the next instruction anyway.
    fn continue_at(&mut self, target: Address, next: Option<Address>) -> Vec<String> {
        if next == Some(target) {
            Vec::new()
        } else {
            self.transfer(target)
        }
    }

    /// Jumps to `target`: a goto inside the function, a tail call to another function's
    /// entry, otherwise a tail call through the dispatcher.
    fn transfer(&mut self, target: Address) -> Vec<String> {
        if self.owned.contains_key(&target) {
            self.labels.insert(target);
            vec![format!("goto {};", label(target))]
        } else if self.context.function(target).is_some() {
            vec![self.tail_call(target)]
        } else {
            dispatch(&constant(target.get()))
        }
    }

    fn indirect_jump(&mut self, value: String, index: usize) -> Vec<String> {
        let targets: BTreeSet<Address> = self.slots[index]
            .block
            .successors
            .iter()
            .map(|edge| edge.target)
            .collect();
        let mut lines = vec![
            "{".to_owned(),
            format!(
                "    uint32_t target = {value} & {};",
                constant(ADDRESS_MASK)
            ),
        ];
        let reachable: Vec<Address> = targets
            .into_iter()
            .filter(|target| {
                self.owned.contains_key(target) || self.context.function(*target).is_some()
            })
            .collect();
        let cases: Vec<(Address, Vec<String>)> = reachable
            .into_iter()
            .map(|target| (target, self.transfer(target)))
            .collect();
        if cases.is_empty() {
            lines.extend(indented(4, dispatch("target")));
        } else {
            lines.push("    switch (target) {".to_owned());
            for (target, body) in cases {
                lines.push(format!("    case {}:", constant(target.get())));
                lines.extend(indented(8, body));
            }
            lines.push("    default:".to_owned());
            lines.extend(indented(8, dispatch("target")));
            lines.push("    }".to_owned());
        }
        lines.push("}".to_owned());
        lines
    }

    fn call(
        &mut self,
        target: Address,
        return_address: Address,
        next: Option<Address>,
    ) -> Vec<String> {
        match self.context.function(target) {
            // Whatever a callee hinted as never returning does return to reaches this
            // function's caller, which checks it, so no native frame needs to stay.
            Some(callee) if callee.noreturn => vec![self.tail_call(target)],
            Some(_) => {
                let call = self.direct_call(target);
                self.native(Vec::new(), call, return_address, next)
            }
            None => {
                let target = constant(target.get());
                let call = format!("ag_lookup(s, {target})(s, {target})");
                self.native(Vec::new(), call, return_address, next)
            }
        }
    }

    fn indirect_call(
        &mut self,
        value: String,
        index: usize,
        return_address: Address,
        next: Option<Address>,
    ) -> Vec<String> {
        let site = self.slots[index].lifted.address;
        let targets: BTreeSet<Address> = self.slots[index]
            .block
            .calls
            .iter()
            .filter(|call| call.site == site)
            .flat_map(|call| call.targets.iter().copied())
            .filter(|&target| self.context.function(target).is_some())
            .collect();
        let mut setup = vec![format!(
            "uint32_t target = {value} & {};",
            constant(ADDRESS_MASK)
        )];
        let fallback = "ag_lookup(s, target)(s, target)".to_owned();
        if targets.is_empty() {
            return self.native(setup, fallback, return_address, next);
        }
        setup.push("uint32_t next;".to_owned());
        setup.push("switch (target) {".to_owned());
        for target in targets {
            setup.push(format!("case {}:", constant(target.get())));
            setup.push(format!("    next = {};", self.direct_call(target)));
            setup.push("    break;".to_owned());
        }
        setup.push("default:".to_owned());
        setup.push(format!("    next = {fallback};"));
        setup.push("    break;".to_owned());
        setup.push("}".to_owned());
        self.checked(setup, return_address, next)
    }

    /// Takes exception `vector` with `pc` in the frame: the entry sequence inline, then a
    /// native call to the handler, whose RTE returns the address to check.
    fn exception(&mut self, vector: u8, pc: Address, next: Option<Address>) -> Vec<String> {
        let entry = exception_entry(vector, pc);
        let handler_temp = match entry.ops.last() {
            Some(Op::Exit(Control::JumpIndirect(Value::Temp(temp)))) => Some(*temp),
            _ => None,
        };
        let handler = self.context.handlers.get(&vector).copied();
        let ahead = self.exception_ahead.clone();
        let printer = Printer {
            body: &entry,
            prefix: "e",
            rom_size: self.context.analysis.rom_size,
            ahead: &ahead,
        };
        let used = used_temps(&entry);
        let mut setup = Vec::new();
        for op in &entry.ops {
            // The vector table is ROM, so a known handler needs no read of it.
            let is_handler_read = matches!(op, Op::Let { dst, .. } if Some(*dst) == handler_temp);
            if handler.is_some() && is_handler_read {
                continue;
            }
            if let Some(statement) = printer.op(op, &used) {
                setup.push(statement);
            }
        }
        let call = if let Some(function) = handler {
            self.direct_call(function)
        } else {
            let address = match handler_temp {
                Some(temp) => printer.value(Value::Temp(temp)),
                None => format!("ag_load32(s, {}, {ahead})", constant(u32::from(vector) * 4)),
            };
            setup.push(format!(
                "uint32_t handler = {address} & {};",
                constant(ADDRESS_MASK)
            ));
            "ag_lookup(s, handler)(s, handler)".to_owned()
        };
        self.native(setup, call, pc, next)
    }

    /// Runs `setup`, then the native call `call`, and continues at `expected` when it
    /// returns that address.
    fn native(
        &mut self,
        mut setup: Vec<String>,
        call: String,
        expected: Address,
        next: Option<Address>,
    ) -> Vec<String> {
        setup.push(format!("uint32_t next = {call};"));
        self.checked(setup, expected, next)
    }

    /// Wraps `setup`, which leaves the returned address in `next`, with the check against
    /// `expected`.
    fn checked(
        &mut self,
        setup: Vec<String>,
        expected: Address,
        next: Option<Address>,
    ) -> Vec<String> {
        let mut lines = vec!["{".to_owned()];
        lines.extend(indented(4, setup));
        if self.owned.contains_key(&expected) {
            self.resume.insert(expected);
            self.labels.insert(expected);
            lines.push(format!("    if (next != {})", constant(expected.get())));
            lines.push("        return next;".to_owned());
            lines.push("}".to_owned());
            lines.extend(self.continue_at(expected, next));
        } else {
            lines.push("    return next;".to_owned());
            lines.push("}".to_owned());
        }
        lines
    }

    fn prefix(&self) -> &str {
        &self.context.options.symbol_prefix
    }

    fn direct_call(&self, target: Address) -> String {
        format!(
            "{}(s, {})",
            function_name(self.prefix(), target),
            constant(target.get())
        )
    }

    fn tail_call(&self, target: Address) -> String {
        format!("AG_TAIL return {};", self.direct_call(target))
    }

    fn finish(self) -> Emitted {
        let entry = self.function.entry;
        let mut text = String::new();
        text.push_str(&prototype(self.prefix(), entry));
        text.push_str("\n{\n");
        // Every state access is printed as `s->` or as an argument `(s, `; a function of
        // gotos only (an endless `bra.s *`) has neither.
        let uses_state = self.parts.iter().any(|part| {
            matches!(part, Part::Line(line) if line.contains("s->") || line.contains("(s, "))
        });
        if !uses_state {
            text.push_str("    (void)s;\n");
        }
        let resume: Vec<Address> = self
            .resume
            .iter()
            .copied()
            .filter(|&a| a != entry)
            .collect();
        if resume.is_empty() {
            text.push_str("    (void)pc;\n");
        } else {
            text.push_str(&format!("    if (pc != {}) {{\n", constant(entry.get())));
            text.push_str("        switch (pc) {\n");
            for address in &resume {
                text.push_str(&format!(
                    "        case {}:\n            goto {};\n",
                    constant(address.get()),
                    label(*address)
                ));
            }
            text.push_str("        }\n    }\n");
        }
        for part in &self.parts {
            match part {
                Part::Anchor(address) if self.labels.contains(address) => {
                    text.push_str(&format!("{}:;\n", label(*address)));
                }
                Part::Anchor(_) => {}
                Part::Line(line) => {
                    text.push_str(line);
                    text.push('\n');
                }
            }
        }
        text.push_str("}\n");
        Emitted {
            text,
            resume: resume.into_iter().collect(),
        }
    }
}

/// Whether control can leave the function body at this instruction.
/// The static cycles of `lifted` when no branch leaves it: through its `Exit`, or falling
/// through.
fn static_cost(lifted: &Lifted) -> u16 {
    match lifted.body.ops.last() {
        Some(Op::Exit(control)) => lifted.cycles.leaving(control),
        _ => lifted.cycles.not_taken,
    }
}

/// [`Printer::ahead`] when nothing is charged past the instruction's start.
const NOTHING_AHEAD: &str = "0";

/// [`Printer::ahead`] where an instruction leaves: its `taken` cycles were charged by now,
/// with the data-dependent values in `charged` (C expressions) on top.
fn leaving_ahead(charging: bool, taken: u16, charged: &[String]) -> String {
    if !charging {
        return NOTHING_AHEAD.to_owned();
    }
    let mut ahead = taken.to_string();
    for value in charged {
        ahead.push_str(&format!(" + (int32_t){value}"));
    }
    ahead
}

/// Whether an exception `lifted` takes pushes its own address, so its handler returns to it.
fn resumes_at_itself(lifted: &Lifted) -> bool {
    lifted.body.ops.iter().any(|op| {
        matches!(
            op,
            Op::Branch {
                control: Control::Exception { pc, .. },
                ..
            } | Op::Exit(Control::Exception { pc, .. }) if *pc == lifted.address
        )
    })
}

fn ends_segment(slot: &Slot<'_>) -> bool {
    slot.lifted
        .body
        .ops
        .iter()
        .any(|op| matches!(op, Op::Branch { .. } | Op::Exit(_)))
}

/// The block starts of `function` that get a check point under the options.
fn check_points(context: &Context<'_>, function: &Function) -> BTreeSet<Address> {
    let blocks = &function.blocks;
    match context.options.check_points {
        CheckPoints::None => BTreeSet::new(),
        CheckPoints::BlockStarts => blocks.clone(),
        CheckPoints::LoopTargets => blocks
            .iter()
            .filter_map(|start| context.analysis.blocks.get(start))
            .flat_map(|block| {
                block
                    .successors
                    .iter()
                    .filter(move |edge| edge.target <= block.start)
                    .map(|edge| edge.target)
            })
            .filter(|target| blocks.contains(target))
            .collect(),
    }
}

/// A tail call through the dispatcher to the address `target` (a C expression).
fn dispatch(target: &str) -> Vec<String> {
    vec![
        "{".to_owned(),
        format!("    AgFn *next = ag_lookup(s, {target});"),
        format!("    AG_TAIL return next(s, {target});"),
        "}".to_owned(),
    ]
}

fn indented(indent: usize, lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| format!("{:indent$}{line}", ""))
        .collect()
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}
