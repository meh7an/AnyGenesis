//! IR shape snapshots: representative instructions of every group, lifted at 0x001000.
//!
//! Semantics are proven against SingleStepTests in M2.2; these pin the IR's form (op order,
//! which flags are set and read, how edge cases are expressed). `print_shapes` (ignored)
//! prints every case's text for review.

use anygenesis_ir::{exception_entry, lift};
use anygenesis_m68k::decode;
use anygenesis_shared::Address;

const ADDRESS: u32 = 0x1000;

/// (description, instruction words, expected text).
#[rustfmt::skip]
const CASES: &[(&str, &[u16], &str)] = &[
    (
        "move.w d1,(a0)+",
        &[0x30C1],
        "$001000 (2 bytes)
t0:w = d1.w
t1:l = a0.l
t2:l = add t1, $2.l
a0.l = t2
store.w t1, t0
t3:bool = lts t0, $0.w
flag n = t3
t4:bool = eq t0, $0.w
flag z = t4
flag v = false
flag c = false
",
    ),
    (
        "move.b d0,-(a7)",
        &[0x1F00],
        "$001000 (2 bytes)
t0:b = d0.b
t1:l = a7.l
t2:l = sub t1, $2.l
a7.l = t2
store.b t2, t0
t3:bool = lts t0, $0.b
flag n = t3
t4:bool = eq t0, $0.b
flag z = t4
flag v = false
flag c = false
",
    ),
    (
        "movea.w (a1),a2",
        &[0x3451],
        "$001000 (2 bytes)
t0:l = a1.l
t1:w = load.w t0
t2:l = sext t1
a2.l = t2
",
    ),
    (
        "moveq #-1,d0",
        &[0x70FF],
        "$001000 (2 bytes)
d0.l = $ffffffff.l
flag n = true
flag z = false
flag v = false
flag c = false
",
    ),
    (
        "move #$2700,sr",
        &[0x46FC, 0x2700],
        "$001000 (4 bytes)
t0:bool = supervisor
t1:bool = not t0
if t1 exception 8 pc $001000
sr = $2700.w
",
    ),
    (
        "move d0,ccr",
        &[0x44C0],
        "$001000 (2 bytes)
t0:w = d0.w
t1:b = trunc t0
ccr = t1
",
    ),
    (
        "move sr,d0",
        &[0x40C0],
        "$001000 (2 bytes)
t0:w = sr
d0.w = t0
",
    ),
    (
        "move usp,a0",
        &[0x4E68],
        "$001000 (2 bytes)
t0:bool = supervisor
t1:bool = not t0
if t1 exception 8 pc $001000
t2:l = isp.l
a0.l = t2
",
    ),
    (
        "add.l d0,d1",
        &[0xD280],
        "$001000 (2 bytes)
t0:l = d0.l
t1:l = d1.l
t2:l = add t1, t0
d1.l = t2
t3:bool = lts t2, $0.l
flag n = t3
t4:bool = eq t2, $0.l
flag z = t4
t5:bool = add_overflow t1, t0, t2
flag v = t5
t6:bool = add_carry t1, t0, t2
flag c = t6
flag x = t6
",
    ),
    (
        "addq.w #1,a0",
        &[0x5248],
        "$001000 (2 bytes)
t0:l = a0.l
t1:l = add t0, $1.l
a0.l = t1
",
    ),
    (
        "addx.b d0,d1",
        &[0xD300],
        "$001000 (2 bytes)
t0:b = d0.b
t1:b = d1.b
t2:bool = flag x
t3:b = zext t2
t4:b = add t1, t0
t5:b = add t4, t3
d1.b = t5
t6:bool = lts t5, $0.b
flag n = t6
t7:bool = flag z
t8:bool = eq t5, $0.b
t9:bool = and t7, t8
flag z = t9
t10:bool = add_overflow t1, t0, t5
flag v = t10
t11:bool = add_carry t1, t0, t5
flag c = t11
flag x = t11
",
    ),
    (
        "subx.w -(a0),-(a1)",
        &[0x9348],
        "$001000 (2 bytes)
t0:l = a0.l
t1:l = sub t0, $2.l
a0.l = t1
t2:w = load.w t1
t3:l = a1.l
t4:l = sub t3, $2.l
a1.l = t4
t5:w = load.w t4
t6:bool = flag x
t7:w = zext t6
t8:w = sub t5, t2
t9:w = sub t8, t7
store.w t4, t9
t10:bool = lts t9, $0.w
flag n = t10
t11:bool = flag z
t12:bool = eq t9, $0.w
t13:bool = and t11, t12
flag z = t13
t14:bool = sub_overflow t5, t2, t9
flag v = t14
t15:bool = sub_carry t5, t2, t9
flag c = t15
flag x = t15
",
    ),
    (
        "negx.l d0",
        &[0x4080],
        "$001000 (2 bytes)
t0:l = d0.l
t1:l = sub $0.l, t0
t2:bool = flag x
t3:l = zext t2
t4:l = sub t1, t3
d0.l = t4
t5:bool = lts t4, $0.l
flag n = t5
t6:bool = flag z
t7:bool = eq t4, $0.l
t8:bool = and t6, t7
flag z = t8
t9:bool = sub_overflow $0.l, t0, t4
flag v = t9
t10:bool = sub_carry $0.l, t0, t4
flag c = t10
flag x = t10
",
    ),
    (
        "neg.b d0",
        &[0x4400],
        "$001000 (2 bytes)
t0:b = d0.b
t1:b = sub $0.b, t0
d0.b = t1
t2:bool = lts t1, $0.b
flag n = t2
t3:bool = eq t1, $0.b
flag z = t3
t4:bool = sub_overflow $0.b, t0, t1
flag v = t4
t5:bool = sub_carry $0.b, t0, t1
flag c = t5
flag x = t5
",
    ),
    (
        "cmp.b (a0),d0",
        &[0xB010],
        "$001000 (2 bytes)
t0:l = a0.l
t1:b = load.b t0
t2:b = d0.b
t3:b = sub t2, t1
t4:bool = lts t3, $0.b
flag n = t4
t5:bool = eq t3, $0.b
flag z = t5
t6:bool = sub_overflow t2, t1, t3
flag v = t6
t7:bool = sub_carry t2, t1, t3
flag c = t7
",
    ),
    (
        "cmpa.w d0,a1",
        &[0xB2C0],
        "$001000 (2 bytes)
t0:w = d0.w
t1:l = sext t0
t2:l = a1.l
t3:l = sub t2, t1
t4:bool = lts t3, $0.l
flag n = t4
t5:bool = eq t3, $0.l
flag z = t5
t6:bool = sub_overflow t2, t1, t3
flag v = t6
t7:bool = sub_carry t2, t1, t3
flag c = t7
",
    ),
    (
        "cmpm.b (a0)+,(a1)+",
        &[0xB308],
        "$001000 (2 bytes)
t0:l = a0.l
t1:l = add t0, $1.l
a0.l = t1
t2:b = load.b t0
t3:l = a1.l
t4:l = add t3, $1.l
a1.l = t4
t5:b = load.b t3
t6:b = sub t5, t2
t7:bool = lts t6, $0.b
flag n = t7
t8:bool = eq t6, $0.b
flag z = t8
t9:bool = sub_overflow t5, t2, t6
flag v = t9
t10:bool = sub_carry t5, t2, t6
flag c = t10
",
    ),
    (
        "andi.w #$ff,d0",
        &[0x0240, 0x00FF],
        "$001000 (4 bytes)
t0:w = d0.w
t1:w = and t0, $ff.w
d0.w = t1
t2:bool = lts t1, $0.w
flag n = t2
t3:bool = eq t1, $0.w
flag z = t3
flag v = false
flag c = false
",
    ),
    (
        "eori #$1f,ccr",
        &[0x0A3C, 0x001F],
        "$001000 (4 bytes)
t0:w = sr
t1:b = trunc t0
t2:b = xor t1, $1f.b
ccr = t2
",
    ),
    (
        "ori #$700,sr",
        &[0x007C, 0x0700],
        "$001000 (4 bytes)
t0:bool = supervisor
t1:bool = not t0
if t1 exception 8 pc $001000
t2:w = sr
t3:w = or t2, $700.w
sr = t3
",
    ),
    (
        "not.b d0",
        &[0x4600],
        "$001000 (2 bytes)
t0:b = d0.b
t1:b = not t0
d0.b = t1
t2:bool = lts t1, $0.b
flag n = t2
t3:bool = eq t1, $0.b
flag z = t3
flag v = false
flag c = false
",
    ),
    (
        "clr.w (a0)",
        &[0x4250],
        "$001000 (2 bytes)
t0:l = a0.l
store.w t0, $0.w
flag n = false
flag z = true
flag v = false
flag c = false
",
    ),
    (
        "tst.l d0",
        &[0x4A80],
        "$001000 (2 bytes)
t0:l = d0.l
t1:bool = lts t0, $0.l
flag n = t1
t2:bool = eq t0, $0.l
flag z = t2
flag v = false
flag c = false
",
    ),
    (
        "tas (a0)",
        &[0x4AD0],
        "$001000 (2 bytes)
t0:l = a0.l
t1:b = load.b t0
t2:bool = lts t1, $0.b
flag n = t2
t3:bool = eq t1, $0.b
flag z = t3
flag v = false
flag c = false
t4:b = or t1, $80.b
store.b t0, t4
",
    ),
    (
        "ext.w d0",
        &[0x4880],
        "$001000 (2 bytes)
t0:b = d0.b
t1:w = sext t0
d0.w = t1
t2:bool = lts t1, $0.w
flag n = t2
t3:bool = eq t1, $0.w
flag z = t3
flag v = false
flag c = false
",
    ),
    (
        "ext.l d0",
        &[0x48C0],
        "$001000 (2 bytes)
t0:w = d0.w
t1:l = sext t0
d0.l = t1
t2:bool = lts t1, $0.l
flag n = t2
t3:bool = eq t1, $0.l
flag z = t3
flag v = false
flag c = false
",
    ),
    (
        "swap d0",
        &[0x4840],
        "$001000 (2 bytes)
t0:l = d0.l
t1:l = shl t0, $10.b
t2:l = shr t0, $10.b
t3:l = or t1, t2
d0.l = t3
t4:bool = lts t3, $0.l
flag n = t4
t5:bool = eq t3, $0.l
flag z = t5
flag v = false
flag c = false
",
    ),
    (
        "mulu.w d1,d0",
        &[0xC0C1],
        "$001000 (2 bytes)
t0:w = d1.w
t1:w = d0.w
t2:l = zext t1
t3:l = zext t0
t4:l = mul t2, t3
d0.l = t4
t5:bool = lts t4, $0.l
flag n = t5
t6:bool = eq t4, $0.l
flag z = t6
flag v = false
flag c = false
",
    ),
    (
        "muls.w d1,d0",
        &[0xC1C1],
        "$001000 (2 bytes)
t0:w = d1.w
t1:w = d0.w
t2:l = sext t1
t3:l = sext t0
t4:l = mul t2, t3
d0.l = t4
t5:bool = lts t4, $0.l
flag n = t5
t6:bool = eq t4, $0.l
flag z = t6
flag v = false
flag c = false
",
    ),
    (
        "divu.w d1,d0",
        &[0x80C1],
        "$001000 (2 bytes)
t0:w = d1.w
t1:l = d0.l
t2:bool = eq t0, $0.w
if t2 exception 5 pc $001002
t3:l = zext t0
t4:l = divu t1, t3
t5:l = remu t1, t3
t6:w = trunc t4
t7:bool = ltu $ffff.l, t4
t8:l = shl t5, $10.b
t9:l = zext t6
t10:l = or t8, t9
t11:l = select t7, t1, t10
d0.l = t11
t12:bool = lts t6, $0.w
t13:bool = or t7, t12
flag n = t13
t14:bool = not t7
t15:bool = eq t6, $0.w
t16:bool = and t14, t15
flag z = t16
flag v = t7
flag c = false
",
    ),
    (
        "divs.w d1,d0",
        &[0x81C1],
        "$001000 (2 bytes)
t0:w = d1.w
t1:l = d0.l
t2:bool = eq t0, $0.w
if t2 exception 5 pc $001002
t3:l = sext t0
t4:l = divs t1, t3
t5:l = rems t1, t3
t6:w = trunc t4
t7:l = sext t6
t8:bool = ne t7, t4
t9:l = shl t5, $10.b
t10:l = zext t6
t11:l = or t9, t10
t12:l = select t8, t1, t11
d0.l = t12
t13:bool = lts t6, $0.w
t14:bool = or t8, t13
flag n = t14
t15:bool = not t8
t16:bool = eq t6, $0.w
t17:bool = and t15, t16
flag z = t17
flag v = t8
flag c = false
",
    ),
    (
        "chk.w d1,d0",
        &[0x4181],
        "$001000 (2 bytes)
t0:w = d1.w
t1:w = d0.w
t2:bool = lts t1, $0.w
flag n = t2
t3:bool = eq t1, $0.w
flag z = t3
flag v = false
flag c = false
t4:bool = lts t0, t1
t5:bool = or t2, t4
if t5 exception 6 pc $001002
",
    ),
    (
        "abcd d1,d0",
        &[0xC101],
        "$001000 (2 bytes)
t0:b = d1.b
t1:b = d0.b
t2:w = zext t1
t3:w = zext t0
t4:bool = flag x
t5:w = zext t4
t6:w = add t2, t3
t7:w = add t6, t5
t8:w = not t7
t9:w = and t2, t3
t10:w = or t2, t3
t11:w = and t8, t10
t12:w = or t9, t11
t13:w = and t12, $88.w
t14:w = add t7, $66.w
t15:w = xor t14, t7
t16:w = and t15, $110.w
t17:w = shr t16, $1.b
t18:w = or t13, t17
t19:w = shr t18, $2.b
t20:w = sub t18, t19
t21:w = add t7, t20
t22:w = not t21
t23:w = and t7, t22
t24:w = or t13, t23
t25:w = and t8, t21
t26:b = trunc t21
t27:bool = lts t26, $0.b
flag n = t27
t28:bool = flag z
t29:bool = eq t26, $0.b
t30:bool = and t28, t29
flag z = t30
t31:w = and t25, $80.w
t32:bool = ne t31, $0.w
flag v = t32
t33:w = and t24, $80.w
t34:bool = ne t33, $0.w
flag c = t34
flag x = t34
d0.b = t26
",
    ),
    (
        "sbcd -(a1),-(a0)",
        &[0x8109],
        "$001000 (2 bytes)
t0:l = a1.l
t1:l = sub t0, $1.l
a1.l = t1
t2:b = load.b t1
t3:l = a0.l
t4:l = sub t3, $1.l
a0.l = t4
t5:b = load.b t4
t6:w = zext t5
t7:w = zext t2
t8:bool = flag x
t9:w = zext t8
t10:w = sub t6, t7
t11:w = sub t10, t9
t12:w = not t11
t13:w = not t6
t14:w = and t13, t7
t15:w = xor t6, t7
t16:w = not t15
t17:w = and t11, t16
t18:w = or t14, t17
t19:w = and t18, $88.w
t20:w = shr t19, $2.b
t21:w = sub t19, t20
t22:w = sub t11, t21
t23:w = not t22
t24:w = and t12, t22
t25:w = or t19, t24
t26:w = and t11, t23
t27:b = trunc t22
t28:bool = lts t27, $0.b
flag n = t28
t29:bool = flag z
t30:bool = eq t27, $0.b
t31:bool = and t29, t30
flag z = t31
t32:w = and t26, $80.w
t33:bool = ne t32, $0.w
flag v = t33
t34:w = and t25, $80.w
t35:bool = ne t34, $0.w
flag c = t35
flag x = t35
store.b t4, t27
",
    ),
    (
        "nbcd d0",
        &[0x4800],
        "$001000 (2 bytes)
t0:b = d0.b
t1:w = zext t0
t2:bool = flag x
t3:w = zext t2
t4:w = sub $0.w, t1
t5:w = sub t4, t3
t6:w = not t5
t7:w = not t1
t8:w = and t5, t7
t9:w = or t1, t8
t10:w = and t9, $88.w
t11:w = shr t10, $2.b
t12:w = sub t10, t11
t13:w = sub t5, t12
t14:w = not t13
t15:w = and t6, t13
t16:w = or t10, t15
t17:w = and t5, t14
t18:b = trunc t13
t19:bool = lts t18, $0.b
flag n = t19
t20:bool = flag z
t21:bool = eq t18, $0.b
t22:bool = and t20, t21
flag z = t22
t23:w = and t17, $80.w
t24:bool = ne t23, $0.w
flag v = t24
t25:w = and t16, $80.w
t26:bool = ne t25, $0.w
flag c = t26
flag x = t26
d0.b = t18
",
    ),
    (
        "asl.w #1,d0",
        &[0xE340],
        "$001000 (2 bytes)
t0:w = d0.w
t1:w = shl t0, $1.b
t2:w = shr t0, $f.b
t3:w = and t2, $1.w
t4:bool = ne t3, $0.w
d0.w = t1
t5:bool = lts t1, $0.w
flag n = t5
t6:bool = eq t1, $0.w
flag z = t6
t7:w = shl t0, $1.b
t8:w = sar t7, $1.b
t9:bool = ne t8, t0
flag v = t9
flag c = t4
flag x = t4
",
    ),
    (
        "asr.w (a0)",
        &[0xE0D0],
        "$001000 (2 bytes)
t0:l = a0.l
t1:w = load.w t0
t2:w = sar t1, $1.b
t3:w = and t1, $1.w
t4:bool = ne t3, $0.w
store.w t0, t2
t5:bool = lts t2, $0.w
flag n = t5
t6:bool = eq t2, $0.w
flag z = t6
flag v = false
flag c = t4
flag x = t4
",
    ),
    (
        "lsr.l d1,d0",
        &[0xE2A8],
        "$001000 (2 bytes)
t0:b = d1.b
t1:b = and t0, $3f.b
t2:l = d0.l
t3:l = shr t2, t1
t4:b = sub t1, $1.b
t5:l = shr t2, t4
t6:l = and t5, $1.l
t7:bool = ne t6, $0.l
d0.l = t3
t8:bool = lts t3, $0.l
flag n = t8
t9:bool = eq t3, $0.l
flag z = t9
flag v = false
t10:bool = ne t1, $0.b
t11:bool = and t10, t7
flag c = t11
t12:bool = ne t1, $0.b
t13:bool = flag x
t14:bool = select t12, t7, t13
flag x = t14
",
    ),
    (
        "rol.b #8,d0",
        &[0xE118],
        "$001000 (2 bytes)
t0:b = d0.b
t1:b = shr t0, $8.b
t2:b = or t0, t1
t3:b = and t2, $1.b
t4:bool = ne t3, $0.b
d0.b = t2
t5:bool = lts t2, $0.b
flag n = t5
t6:bool = eq t2, $0.b
flag z = t6
flag v = false
flag c = t4
",
    ),
    (
        "ror.l d1,d0",
        &[0xE2B8],
        "$001000 (2 bytes)
t0:b = d1.b
t1:b = and t0, $3f.b
t2:l = d0.l
t3:b = and t1, $1f.b
t4:l = shr t2, t3
t5:b = sub $20.b, t3
t6:l = shl t2, t5
t7:l = or t4, t6
t8:bool = lts t7, $0.l
d0.l = t7
t9:bool = lts t7, $0.l
flag n = t9
t10:bool = eq t7, $0.l
flag z = t10
flag v = false
t11:bool = ne t1, $0.b
t12:bool = and t11, t8
flag c = t12
",
    ),
    (
        "roxl.l #1,d0",
        &[0xE390],
        "$001000 (2 bytes)
t0:l = d0.l
t1:bool = flag x
t2:l = zext t1
t3:l = shl t0, $1.b
t4:l = shr t0, $20.b
t5:l = or t3, t2
t6:l = or t5, t4
t7:l = shr t0, $1f.b
t8:l = and t7, $1.l
t9:bool = ne t8, $0.l
d0.l = t6
t10:bool = lts t6, $0.l
flag n = t10
t11:bool = eq t6, $0.l
flag z = t11
flag v = false
flag c = t9
flag x = t9
",
    ),
    (
        "roxr.w d1,d0",
        &[0xE270],
        "$001000 (2 bytes)
t0:b = d1.b
t1:b = and t0, $3f.b
t2:w = d0.w
t3:b = remu t1, $11.b
t4:bool = flag x
t5:w = zext t4
t6:b = sub t3, $1.b
t7:b = sub $11.b, t3
t8:w = shr t2, t3
t9:b = sub $10.b, t3
t10:w = shl t5, t9
t11:w = shl t2, t7
t12:w = or t8, t10
t13:w = or t12, t11
t14:w = shr t2, t6
t15:w = and t14, $1.w
t16:bool = ne t15, $0.w
d0.w = t13
t17:bool = lts t13, $0.w
flag n = t17
t18:bool = eq t13, $0.w
flag z = t18
flag v = false
t19:bool = ne t3, $0.b
t20:bool = select t19, t16, t4
flag c = t20
flag x = t20
",
    ),
    (
        "btst #3,d0",
        &[0x0800, 0x0003],
        "$001000 (4 bytes)
t0:l = d0.l
t1:l = and t0, $8.l
t2:bool = eq t1, $0.l
flag z = t2
",
    ),
    (
        "bchg #9,(a0)",
        &[0x0850, 0x0009],
        "$001000 (4 bytes)
t0:l = a0.l
t1:b = load.b t0
t2:b = and t1, $2.b
t3:bool = eq t2, $0.b
flag z = t3
t4:b = xor t1, $2.b
store.b t0, t4
",
    ),
    (
        "bclr d1,d0",
        &[0x0380],
        "$001000 (2 bytes)
t0:b = d1.b
t1:b = and t0, $1f.b
t2:l = d0.l
t3:l = shl $1.l, t1
t4:l = and t2, t3
t5:bool = eq t4, $0.l
flag z = t5
t6:l = not t3
t7:l = and t2, t6
d0.l = t7
",
    ),
    (
        "bset d1,(a0)",
        &[0x03D0],
        "$001000 (2 bytes)
t0:b = d1.b
t1:b = and t0, $7.b
t2:l = a0.l
t3:b = load.b t2
t4:b = shl $1.b, t1
t5:b = and t3, t4
t6:bool = eq t5, $0.b
flag z = t6
t7:b = or t3, t4
store.b t2, t7
",
    ),
    (
        "movem.l d0-d1/a0,-(a7)",
        &[0x48E7, 0xC080],
        "$001000 (4 bytes)
t0:l = a7.l
t1:l = sub t0, $4.l
t2:l = a0.l
store.l t1, t2
t3:l = sub t1, $4.l
t4:l = d1.l
store.l t3, t4
t5:l = sub t3, $4.l
t6:l = d0.l
store.l t5, t6
a7.l = t5
",
    ),
    (
        "movem.w (a7)+,d0/a1",
        &[0x4C9F, 0x0201],
        "$001000 (4 bytes)
t0:l = a7.l
t1:w = load.w t0
t2:l = sext t1
d0.l = t2
t3:l = add t0, $2.l
t4:w = load.w t3
t5:l = sext t4
a1.l = t5
t6:l = add t3, $2.l
a7.l = t6
",
    ),
    (
        "movem.l d0/a6,(a0)",
        &[0x48D0, 0x4001],
        "$001000 (4 bytes)
t0:l = a0.l
t1:l = d0.l
store.l t0, t1
t2:l = add t0, $4.l
t3:l = a6.l
store.l t2, t3
",
    ),
    (
        "movep.w d0,(2,a0)",
        &[0x0188, 0x0002],
        "$001000 (4 bytes)
t0:l = a0.l
t1:l = add t0, $2.l
t2:w = d0.w
t3:w = shr t2, $8.b
t4:b = trunc t3
store.b t1, t4
t5:b = trunc t2
t6:l = add t1, $2.l
store.b t6, t5
",
    ),
    (
        "movep.l (2,a0),d0",
        &[0x0148, 0x0002],
        "$001000 (4 bytes)
t0:l = a0.l
t1:l = add t0, $2.l
t2:b = load.b t1
t3:l = zext t2
t4:l = shl t3, $18.b
t5:l = add t1, $2.l
t6:b = load.b t5
t7:l = zext t6
t8:l = shl t7, $10.b
t9:l = or t4, t8
t10:l = add t1, $4.l
t11:b = load.b t10
t12:l = zext t11
t13:l = shl t12, $8.b
t14:l = or t9, t13
t15:l = add t1, $6.l
t16:b = load.b t15
t17:l = zext t16
t18:l = or t14, t17
d0.l = t18
",
    ),
    (
        "lea (4,pc),a0",
        &[0x41FA, 0x0004],
        "$001000 (4 bytes)
a0.l = $1006.l
",
    ),
    (
        "lea (2,a0,d1.w),a1",
        &[0x43F0, 0x1002],
        "$001000 (4 bytes)
t0:l = a0.l
t1:l = add t0, $2.l
t2:w = d1.w
t3:l = sext t2
t4:l = add t1, t3
a1.l = t4
",
    ),
    (
        "pea (a0)",
        &[0x4850],
        "$001000 (2 bytes)
t0:l = a0.l
t1:l = a7.l
t2:l = sub t1, $4.l
store.l t2, t0
a7.l = t2
",
    ),
    (
        "exg d0,a1",
        &[0xC189],
        "$001000 (2 bytes)
t0:l = d0.l
t1:l = a1.l
d0.l = t1
a1.l = t0
",
    ),
    (
        "link a6,#-4",
        &[0x4E56, 0xFFFC],
        "$001000 (4 bytes)
t0:l = a6.l
t1:l = a7.l
t2:l = sub t1, $4.l
store.l t2, t0
a7.l = t2
a6.l = t2
t3:l = add t2, $fffffffc.l
a7.l = t3
",
    ),
    (
        "link a7,#-4",
        &[0x4E57, 0xFFFC],
        "$001000 (4 bytes)
t0:l = a7.l
t1:l = a7.l
t2:l = sub t1, $4.l
store.l t2, t0
a7.l = t2
a7.l = t2
t3:l = add t2, $fffffffc.l
a7.l = t3
",
    ),
    (
        "unlk a6",
        &[0x4E5E],
        "$001000 (2 bytes)
t0:l = a6.l
t1:l = load.l t0
t2:l = add t0, $4.l
a7.l = t2
a6.l = t1
",
    ),
    (
        "bra.s *+6",
        &[0x6004],
        "$001000 (2 bytes)
jump $001006
",
    ),
    (
        "bsr.w *+$12",
        &[0x6100, 0x0010],
        "$001000 (4 bytes)
t0:l = a7.l
t1:l = sub t0, $4.l
store.l t1, $1004.l
a7.l = t1
call $001012 return $001004
",
    ),
    (
        "beq.s *+6",
        &[0x6704],
        "$001000 (2 bytes)
t0:bool = flag z
if t0 jump $001006
",
    ),
    (
        "bhi.s *+6",
        &[0x6204],
        "$001000 (2 bytes)
t0:bool = flag c
t1:bool = flag z
t2:bool = or t0, t1
t3:bool = not t2
if t3 jump $001006
",
    ),
    (
        "bge.s *+6",
        &[0x6C04],
        "$001000 (2 bytes)
t0:bool = flag n
t1:bool = flag v
t2:bool = xor t0, t1
t3:bool = not t2
if t3 jump $001006
",
    ),
    (
        "dbf d0,*-2",
        &[0x51C8, 0xFFFC],
        "$001000 (4 bytes)
t0:w = d0.w
t1:w = sub t0, $1.w
d0.w = t1
t2:bool = ne t1, $ffff.w
if t2 jump $000ffe
",
    ),
    (
        "sne d0",
        &[0x56C0],
        "$001000 (2 bytes)
t0:bool = flag z
t1:bool = not t0
t2:b = select t1, $ff.b, $0.b
d0.b = t2
",
    ),
    (
        "jmp (a0)",
        &[0x4ED0],
        "$001000 (2 bytes)
t0:l = a0.l
jump t0
",
    ),
    (
        "jmp $2000.l",
        &[0x4EF9, 0x0000, 0x2000],
        "$001000 (6 bytes)
jump $002000
",
    ),
    (
        "jsr (2,pc)",
        &[0x4EBA, 0x0002],
        "$001000 (4 bytes)
t0:l = a7.l
t1:l = sub t0, $4.l
store.l t1, $1004.l
a7.l = t1
call $001004 return $001004
",
    ),
    (
        "jsr (0,a0,d0.w)",
        &[0x4EB0, 0x0000],
        "$001000 (4 bytes)
t0:l = a0.l
t1:w = d0.w
t2:l = sext t1
t3:l = add t0, t2
t4:l = a7.l
t5:l = sub t4, $4.l
store.l t5, $1004.l
a7.l = t5
call t3 return $001004
",
    ),
    (
        "rts",
        &[0x4E75],
        "$001000 (2 bytes)
t0:l = a7.l
t1:l = load.l t0
t2:l = add t0, $4.l
a7.l = t2
return rts t1
",
    ),
    (
        "rtr",
        &[0x4E77],
        "$001000 (2 bytes)
t0:l = a7.l
t1:w = load.w t0
t2:l = add t0, $2.l
a7.l = t2
t3:l = a7.l
t4:l = load.l t3
t5:l = add t3, $4.l
a7.l = t5
t6:b = trunc t1
ccr = t6
return rtr t4
",
    ),
    (
        "rte",
        &[0x4E73],
        "$001000 (2 bytes)
t0:bool = supervisor
t1:bool = not t0
if t1 exception 8 pc $001000
t2:l = a7.l
t3:w = load.w t2
t4:l = add t2, $2.l
a7.l = t4
t5:l = a7.l
t6:l = load.l t5
t7:l = add t5, $4.l
a7.l = t7
sr = t3
return rte t6
",
    ),
    (
        "trap #3",
        &[0x4E43],
        "$001000 (2 bytes)
exception 35 pc $001002
",
    ),
    (
        "trapv",
        &[0x4E76],
        "$001000 (2 bytes)
t0:bool = flag v
if t0 exception 7 pc $001002
",
    ),
    (
        "illegal",
        &[0x4AFC],
        "$001000 (2 bytes)
exception 4 pc $001000
",
    ),
    (
        "line a",
        &[0xA000],
        "$001000 (2 bytes)
exception 10 pc $001000
",
    ),
    (
        "line f",
        &[0xF000],
        "$001000 (2 bytes)
exception 11 pc $001000
",
    ),
    (
        "stop #$2000",
        &[0x4E72, 0x2000],
        "$001000 (4 bytes)
t0:bool = supervisor
t1:bool = not t0
if t1 exception 8 pc $001000
sr = $2000.w
stop resume $001004
",
    ),
    (
        "reset",
        &[0x4E70],
        "$001000 (2 bytes)
t0:bool = supervisor
t1:bool = not t0
if t1 exception 8 pc $001000
reset devices
",
    ),
    (
        "nop",
        &[0x4E71],
        "$001000 (2 bytes)
",
    ),
];

fn shape(words: &[u16]) -> String {
    let instruction = decode(words).unwrap_or_else(|error| panic!("{words:04X?}: {error}"));
    lift(&instruction, Address::new(ADDRESS))
        .unwrap_or_else(|error| panic!("{words:04X?}: {error}"))
        .to_string()
}

#[test]
fn test_lift_shapes_match_snapshots() {
    let mismatches: Vec<String> = CASES
        .iter()
        .filter_map(|(name, words, expected)| {
            let actual = shape(words);
            (actual != *expected).then(|| format!("{name}:\n{actual}"))
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn test_exception_entry_shape_matches_snapshot() {
    let expected = "t0:w = sr
t1:w = or t0, $2000.w
t2:w = and t1, $7fff.w
sr = t2
t3:l = a7.l
t4:l = sub t3, $6.l
store.w t4, t0
t5:l = add t4, $2.l
store.l t5, $1004.l
a7.l = t4
t6:l = load.l $18.l
jump t6
";
    assert_eq!(
        exception_entry(6, Address::new(0x1004)).to_string(),
        expected
    );
}

#[test]
#[ignore = "prints the snapshots for review"]
fn print_shapes() {
    for (name, words, _) in CASES {
        println!("{name}\n{}", shape(words));
    }
    println!(
        "exception entry\n{}",
        exception_entry(6, Address::new(0x1004))
    );
}
