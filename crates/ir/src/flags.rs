//! Flag liveness inside one instruction body, and the pruning it allows.
//!
//! A body reads flags through [`Expr::ReadFlag`] (one flag) and [`Expr::ReadSr`] (all of
//! them), and writes them through [`Op::SetFlag`] (one) and [`Op::WriteSr`] /
//! [`Op::WriteCcr`] (all). The functions here walk one body; the caller supplies what is
//! live after it and at each of its exits, which only a control-flow analysis knows.

use std::ops::{BitAnd, BitOr, BitOrAssign, Sub};

use crate::op::{Body, Control, Expr, Flag, Op, Temp, Value};

/// A set of condition code flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FlagSet(u8);

impl FlagSet {
    pub const EMPTY: Self = Self(0);
    pub const ALL: Self = Self(0x1F);

    /// Returns the set holding only `flag`.
    pub const fn of(flag: Flag) -> Self {
        Self(1 << flag.bit())
    }

    /// Returns the set whose status register bits are set in the low byte of `bits`.
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// Returns the flags as status register bits (X at bit 4 down to C at bit 0).
    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, flag: Flag) -> bool {
        self.0 & Self::of(flag).0 != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns the number of flags in the set.
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// Returns the flags in the set, in status register order from X down to C.
    pub fn iter(self) -> impl Iterator<Item = Flag> {
        Flag::ALL
            .into_iter()
            .filter(move |&flag| self.contains(flag))
    }
}

impl BitOr for FlagSet {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOrAssign for FlagSet {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

impl BitAnd for FlagSet {
    type Output = Self;

    fn bitand(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
}

impl Sub for FlagSet {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl FromIterator<Flag> for FlagSet {
    fn from_iter<I: IntoIterator<Item = Flag>>(flags: I) -> Self {
        flags
            .into_iter()
            .fold(Self::EMPTY, |set, flag| set | Self::of(flag))
    }
}

/// Returns the flags live before `body`, given `live_out`, the flags live when it falls
/// through, and `exit`, the flags live where each [`Op::Branch`] or [`Op::Exit`] leads.
pub fn live_in(body: &Body, live_out: FlagSet, exit: impl FnMut(&Control) -> FlagSet) -> FlagSet {
    walk_backward(body, live_out, exit, |_, _| {})
}

/// Returns the flags `body` writes on the way to its fall-through end, and calls `exit` at
/// each [`Op::Branch`] and [`Op::Exit`] with the flags written before it.
pub fn written(body: &Body, mut exit: impl FnMut(&Control, FlagSet)) -> FlagSet {
    let mut written = FlagSet::EMPTY;
    for op in &body.ops {
        match *op {
            Op::SetFlag { flag, .. } => written |= FlagSet::of(flag),
            Op::WriteSr { .. } | Op::WriteCcr { .. } => written = FlagSet::ALL,
            Op::Branch { control, .. } | Op::Exit(control) => exit(&control, written),
            Op::Let { .. }
            | Op::WriteReg { .. }
            | Op::Store { .. }
            | Op::ResetDevices
            | Op::Cycles { .. } => {}
        }
    }
    written
}

/// Returns `body` without the [`Op::SetFlag`] ops whose flag is dead, given the same
/// liveness as [`live_in`], and without the pure `Let`s only they used. Loads stay, since
/// they reach the bus. The temporaries are renumbered, so the result validates.
pub fn prune(body: &Body, live_out: FlagSet, exit: impl FnMut(&Control) -> FlagSet) -> Body {
    let mut dead_flags = vec![false; body.ops.len()];
    walk_backward(body, live_out, exit, |index, live| {
        if let Op::SetFlag { flag, .. } = body.ops[index] {
            dead_flags[index] = !live.contains(flag);
        }
    });
    rebuild(body, &kept_ops(body, &dead_flags))
}

/// Returns `body` without the ops `dropped` marks and without the pure `Let`s only they
/// used, the temporaries renumbered.
pub(crate) fn without(body: &Body, dropped: &[bool]) -> Body {
    rebuild(body, &kept_ops(body, dropped))
}

/// The ops of `body` that `keep` marks, the temporaries renumbered in order.
fn rebuild(body: &Body, keep: &[bool]) -> Body {
    let mut renamed = vec![None; body.temps.len()];
    let mut pruned = Body::default();
    for (op, _) in body.ops.iter().zip(keep).filter(|(_, kept)| **kept) {
        let mut op = *op;
        rename_op(&mut op, &renamed);
        if let Op::Let { dst, .. } = &mut op {
            // A body never holds more than u16::MAX temporaries (see the builder).
            let fresh = Temp(pruned.temps.len() as u16);
            renamed[dst.index()] = Some(fresh);
            pruned.temps.push(body.temps[dst.index()]);
            *dst = fresh;
        }
        pruned.ops.push(op);
    }
    pruned
}

/// Walks `body` backward and returns the flags live before it. `visit` sees each op's index
/// with the flags live just after that op.
fn walk_backward(
    body: &Body,
    live_out: FlagSet,
    mut exit: impl FnMut(&Control) -> FlagSet,
    mut visit: impl FnMut(usize, FlagSet),
) -> FlagSet {
    let mut live = if body.falls_through() {
        live_out
    } else {
        FlagSet::EMPTY
    };
    for (index, op) in body.ops.iter().enumerate().rev() {
        visit(index, live);
        match *op {
            Op::Exit(control) => live = exit(&control),
            Op::Branch { control, .. } => live |= exit(&control),
            Op::SetFlag { flag, .. } => live = live - FlagSet::of(flag),
            Op::WriteSr { .. } | Op::WriteCcr { .. } => live = FlagSet::EMPTY,
            Op::Let {
                expr: Expr::ReadFlag(flag),
                ..
            } => live |= FlagSet::of(flag),
            Op::Let {
                expr: Expr::ReadSr, ..
            } => live = FlagSet::ALL,
            Op::Let { .. }
            | Op::WriteReg { .. }
            | Op::Store { .. }
            | Op::ResetDevices
            | Op::Cycles { .. } => {}
        }
    }
    live
}

/// Marks the ops to keep: everything but the `SetFlag` and `Cycles` ops `dropped` marks and
/// the side-effect-free `Let`s whose temporaries nothing kept uses.
fn kept_ops(body: &Body, dropped: &[bool]) -> Vec<bool> {
    let mut used = vec![false; body.temps.len()];
    let mut keep = vec![false; body.ops.len()];
    for (index, op) in body.ops.iter().enumerate().rev() {
        keep[index] = match op {
            Op::Let { dst, expr } => used[dst.index()] || matches!(expr, Expr::Load { .. }),
            Op::SetFlag { .. } | Op::Cycles { .. } => !dropped[index],
            _ => true,
        };
        if keep[index] {
            for_each_operand(op, |value| {
                if let Value::Temp(temp) = value {
                    used[temp.index()] = true;
                }
            });
        }
    }
    keep
}

/// Calls `visit` with every value `op` reads.
fn for_each_operand(op: &Op, mut visit: impl FnMut(Value)) {
    let mut op = *op;
    for_each_operand_mut(&mut op, |value| visit(*value));
}

fn rename_op(op: &mut Op, renamed: &[Option<Temp>]) {
    for_each_operand_mut(op, |value| {
        if let Value::Temp(temp) = value {
            // Operands are defined before use, and a kept op's operands are kept.
            *temp = renamed[temp.index()].expect("kept operands are defined first");
        }
    });
}

fn for_each_operand_mut(op: &mut Op, mut visit: impl FnMut(&mut Value)) {
    match op {
        Op::Let { expr, .. } => match expr {
            Expr::ReadReg { .. } | Expr::ReadFlag(_) | Expr::ReadSr | Expr::Supervisor => {}
            Expr::Load { address, .. } => visit(address),
            Expr::Unary { value, .. } | Expr::Convert { value, .. } => visit(value),
            Expr::Binary { left, right, .. } => {
                visit(left);
                visit(right);
            }
            Expr::Shift { value, count, .. } => {
                visit(value);
                visit(count);
            }
            Expr::Select {
                condition,
                if_true,
                if_false,
            } => {
                visit(condition);
                visit(if_true);
                visit(if_false);
            }
            Expr::Flag {
                left,
                right,
                result,
                ..
            } => {
                visit(left);
                visit(right);
                visit(result);
            }
        },
        Op::WriteReg { value, .. }
        | Op::SetFlag { value, .. }
        | Op::WriteSr { value }
        | Op::WriteCcr { value }
        | Op::Cycles { value } => visit(value),
        Op::Store { address, value, .. } => {
            visit(address);
            visit(value);
        }
        Op::ResetDevices => {}
        Op::Branch { condition, control } => {
            visit(condition);
            control_operand(control, visit);
        }
        Op::Exit(control) => control_operand(control, visit),
    }
}

fn control_operand(control: &mut Control, mut visit: impl FnMut(&mut Value)) {
    match control {
        Control::JumpIndirect(target)
        | Control::CallIndirect { target, .. }
        | Control::Return { target, .. } => visit(target),
        Control::Jump(_)
        | Control::Call { .. }
        | Control::Exception { .. }
        | Control::Stop { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use anygenesis_m68k::decode;
    use anygenesis_shared::Address;

    use super::*;
    use crate::{lift, validate};

    fn body(words: &[u16]) -> Body {
        let instruction = decode(words).expect("valid instruction");
        lift(&instruction, Address::new(0x1000))
            .expect("lifts")
            .body
    }

    fn set_flags(body: &Body) -> FlagSet {
        body.ops
            .iter()
            .filter_map(|op| match op {
                Op::SetFlag { flag, .. } => Some(*flag),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn test_flag_set_operations_follow_status_register_bits() {
        let zc = FlagSet::of(Flag::Z) | FlagSet::of(Flag::C);

        assert_eq!(zc.bits(), 0b0_0101);
        assert_eq!(zc.len(), 2);
        assert_eq!(zc.iter().collect::<Vec<_>>(), [Flag::Z, Flag::C]);
        assert_eq!(FlagSet::ALL - zc, FlagSet::from_bits(0b1_1010));
        assert_eq!(FlagSet::ALL & zc, zc);
        assert!(FlagSet::EMPTY.is_empty());
    }

    #[test]
    fn test_live_in_add_kills_every_flag_it_sets() {
        // add.w d1,d0
        let add = body(&[0xD041]);

        assert_eq!(
            live_in(&add, FlagSet::ALL, |_| FlagSet::ALL),
            FlagSet::EMPTY
        );
    }

    #[test]
    fn test_live_in_addx_reads_x_and_z() {
        // addx.w d1,d0
        let addx = body(&[0xD141]);

        let live = live_in(&addx, FlagSet::EMPTY, |_| FlagSet::EMPTY);

        assert_eq!(live, FlagSet::of(Flag::X) | FlagSet::of(Flag::Z));
    }

    #[test]
    fn test_live_in_move_from_sr_reads_every_flag() {
        // move.w sr,d0
        let move_sr = body(&[0x40C0]);

        assert_eq!(
            live_in(&move_sr, FlagSet::EMPTY, |_| FlagSet::EMPTY),
            FlagSet::ALL
        );
    }

    #[test]
    fn test_live_in_branch_takes_exit_liveness_and_reads_condition() {
        // beq.s *+4
        let beq = body(&[0x6702]);

        let live = live_in(&beq, FlagSet::of(Flag::C), |_| FlagSet::of(Flag::N));

        assert_eq!(
            live,
            FlagSet::of(Flag::Z) | FlagSet::of(Flag::C) | FlagSet::of(Flag::N)
        );
    }

    #[test]
    fn test_written_reports_flags_before_each_exit() {
        // chk.w d1,d0 sets its flags before the trap branch.
        let chk = body(&[0x4181]);
        let mut at_exit = Vec::new();

        let fallthrough = written(&chk, |_, flags| at_exit.push(flags));

        let nzvc = FlagSet::ALL - FlagSet::of(Flag::X);
        assert_eq!(fallthrough, nzvc);
        assert_eq!(at_exit, [nzvc]);
    }

    #[test]
    fn test_prune_drops_dead_flags_and_their_computations() {
        // add.w d1,d0 with only Z live afterwards
        let add = body(&[0xD041]);

        let pruned = prune(&add, FlagSet::of(Flag::Z), |_| FlagSet::ALL);

        assert_eq!(validate(&pruned), Ok(()));
        assert_eq!(set_flags(&pruned), FlagSet::of(Flag::Z));
        assert!(pruned.ops.len() < add.ops.len());
        assert!(!pruned.ops.iter().any(|op| matches!(
            op,
            Op::Let {
                expr: Expr::Flag { .. },
                ..
            }
        )));
    }

    #[test]
    fn test_prune_keeps_loads_of_dropped_flag_sources() {
        // tst.w (a0): the load stays even when no flag is live.
        let tst = body(&[0x4A50]);

        let pruned = prune(&tst, FlagSet::EMPTY, |_| FlagSet::EMPTY);

        assert_eq!(validate(&pruned), Ok(()));
        assert_eq!(set_flags(&pruned), FlagSet::EMPTY);
        assert!(pruned.ops.iter().any(|op| matches!(
            op,
            Op::Let {
                expr: Expr::Load { .. },
                ..
            }
        )));
    }

    #[test]
    fn test_prune_with_all_live_keeps_the_body() {
        // addx.w d1,d0
        let addx = body(&[0xD141]);

        assert_eq!(prune(&addx, FlagSet::ALL, |_| FlagSet::ALL), addx);
    }
}
