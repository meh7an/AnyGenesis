//! Finds the exits that may not return to the function's caller: RTS instructions, and
//! instructions leading into another function, reached with the stack pointer away from where
//! it was on entry (a popped caller, `pea` + `rts`) or at an unknown place.

use std::collections::{BTreeMap, BTreeSet};

use anygenesis_ir::{BinaryOp, Body, Control, Expr, Op, Reg, ReturnKind, Value, Width};
use anygenesis_shared::{Address, Size};

use super::program::{Program, Site};
use crate::Function;

/// A0-A7 as offsets from the stack pointer on function entry, `None` when unknown.
type Frame = [Option<i64>; 8];

const SP: usize = 7;

/// Returns every RTS, and every instruction leading into another function, that some
/// function containing it reaches with an unbalanced stack. Another function entered after a
/// popped caller ends in an RTS that returns past this function's return point.
pub(super) fn unbalanced_exits(program: &Program) -> BTreeSet<Address> {
    let mut unbalanced = BTreeSet::new();
    for function in program.analysis.functions.values() {
        let frames = frames(program, function);
        for address in program.function_sites(function) {
            let site = &program.sites[&address];
            let returns = matches!(
                site.lifted.body.ops.last(),
                Some(Op::Exit(Control::Return {
                    kind: ReturnKind::Rts,
                    ..
                }))
            );
            let balanced = |frame: &Frame| frame[SP] == Some(0);
            let unbalanced_here = match frames.get(&address) {
                Some(frame) if returns => !balanced(frame),
                Some(frame) => leaves(program, function, site, *frame)
                    .iter()
                    .any(|frame| !balanced(frame)),
                // Not reached from the entry: any exit may be unbalanced.
                None => returns || !leaves(program, function, site, [None; 8]).is_empty(),
            };
            if unbalanced_here {
                unbalanced.insert(address);
            }
        }
    }
    unbalanced
}

/// Returns the frame at every point where the instruction at `site`, entered with `frame`,
/// goes on to another function's entry: by a jump, a table, or falling through into it.
fn leaves(program: &Program, function: &Function, site: &Site, frame: Frame) -> Vec<Frame> {
    let other = |target: Address| program.analysis.is_tail_jump(function.entry, target);
    let mut found = Vec::new();
    let body = &site.lifted.body;
    let end = evaluate(body, frame, |control, frame| {
        let leaves = match *control {
            Control::Jump(target) => other(target),
            Control::JumpIndirect(_) => program
                .block(site)
                .successors
                .iter()
                .any(|edge| other(edge.target)),
            _ => false,
        };
        if leaves {
            found.push(frame);
        }
    });
    if body.falls_through() && other(site.lifted.next()) {
        found.push(end);
    }
    found
}

/// Returns the frame on arrival at each instruction of `function` reached from its entry.
fn frames(program: &Program, function: &Function) -> BTreeMap<Address, Frame> {
    let local: BTreeSet<Address> = program.function_sites(function).into_iter().collect();
    let mut entry = [None; 8];
    entry[SP] = Some(0);
    let mut frames = BTreeMap::from([(function.entry, entry)]);
    let mut pending = vec![function.entry];
    while let Some(address) = pending.pop() {
        let site = &program.sites[&address];
        let block = program.block(site);
        let mut flows = Vec::new();
        let end = evaluate(&site.lifted.body, frames[&address], |control, frame| {
            match *control {
                Control::Jump(target) => flows.push((target, frame)),
                Control::JumpIndirect(_) => {
                    flows.extend(block.successors.iter().map(|edge| (edge.target, frame)))
                }
                // The callee pops the return address the call pushed and may change any
                // other address register.
                Control::Call { return_address, .. }
                | Control::CallIndirect { return_address, .. } => {
                    let mut after = clobbered(frame);
                    after[SP] = frame[SP].map(|offset| offset + 4);
                    flows.push((return_address, after));
                }
                // The handler's RTE comes back to `pc` with the stack as it was.
                Control::Exception { pc, .. } => flows.push((pc, clobbered(frame))),
                Control::Stop { resume } => flows.push((resume, clobbered(frame))),
                Control::Return { .. } => {}
            }
        });
        if site.lifted.body.falls_through() {
            flows.push((site.lifted.next(), end));
        }
        for (target, frame) in flows {
            if !local.contains(&target) || program.analysis.is_tail_jump(function.entry, target) {
                continue;
            }
            let merged = frames
                .get(&target)
                .map_or(frame, |known| meet(known, &frame));
            if frames.get(&target) != Some(&merged) {
                frames.insert(target, merged);
                pending.push(target);
            }
        }
    }
    frames
}

/// Runs `body` over symbolic address registers, calling `exit` at each branch and exit
/// with the frame there, and returns the frame at its fall-through end.
fn evaluate(body: &Body, mut frame: Frame, mut exit: impl FnMut(&Control, Frame)) -> Frame {
    let mut temps: Vec<Option<i64>> = vec![None; body.temps.len()];
    for op in &body.ops {
        let offset = |value: Value| match value {
            Value::Temp(temp) => temps[temp.index()],
            Value::Const { .. } => None,
        };
        match *op {
            Op::Let { dst, expr } => {
                let value = match expr {
                    Expr::ReadReg {
                        reg: Reg::Address(n),
                        size: Size::Long,
                    } => frame[usize::from(n & 7)],
                    Expr::Binary {
                        op: BinaryOp::Add,
                        left,
                        right,
                    } => offset(left)
                        .zip(constant(right))
                        .or_else(|| offset(right).zip(constant(left)))
                        .map(|(offset, constant)| offset + constant),
                    Expr::Binary {
                        op: BinaryOp::Sub,
                        left,
                        right,
                    } => offset(left)
                        .zip(constant(right))
                        .map(|(offset, constant)| offset - constant),
                    _ => None,
                };
                temps[dst.index()] = value;
            }
            Op::WriteReg {
                reg: Reg::Address(n),
                value,
                ..
            } => frame[usize::from(n & 7)] = offset(value),
            // A change of S swaps A7 with the other stack pointer.
            Op::WriteSr { .. } => frame[SP] = None,
            Op::Branch { control, .. } | Op::Exit(control) => exit(&control, frame),
            Op::WriteReg { .. }
            | Op::SetFlag { .. }
            | Op::WriteCcr { .. }
            | Op::Store { .. }
            | Op::ResetDevices
            | Op::Cycles { .. } => {}
        }
    }
    frame
}

/// Returns a `Long` constant as a signed offset.
fn constant(value: Value) -> Option<i64> {
    match value {
        Value::Const {
            width: Width::Long,
            value,
        } => Some(i64::from(value as i32)),
        _ => None,
    }
}

/// Forgets every address register but the stack pointer.
fn clobbered(frame: Frame) -> Frame {
    let mut after = [None; 8];
    after[SP] = frame[SP];
    after
}

fn meet(left: &Frame, right: &Frame) -> Frame {
    std::array::from_fn(|n| left[n].filter(|&offset| right[n] == Some(offset)))
}
