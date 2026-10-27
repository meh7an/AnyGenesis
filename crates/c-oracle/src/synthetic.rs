//! A small hand-assembled program the build script always translates, so the differential
//! runs in every `cargo test` without a ROM. It is shared by the build script and the tests
//! through `#[path]`, so it depends on nothing but the crates both have.
//!
//! - reset (0x300): bsr.w f; jsr g; andi.w #2,d5; a word-offset jump table on d5 into two
//!   arms (addq.w #1,d2 / neg.w d2) that meet at trap #0; bsr.w p; rts.
//! - f (0x400): moveq #3,d0, then a dbf loop storing d2 through (a0)+.
//! - g (0x500): RAM, ROM and I/O stores and loads, then a loop polling an I/O word until
//!   bit 1 is set, then a push popped again.
//! - p (0x600): pops its own return address when d3 is negative, so it returns to reset's
//!   caller (a popped caller). An `ori.w #0,sr` in the middle of its block resumes at itself
//!   after a privilege violation, a table row inside a cycle segment.
//! - spin (0x700, hinted): `bra.s *`, which never returns: every run reaches the cap.
//! - every exception vector points at an rte at 0x200.

use anygenesis_analysis::{Analysis, analyze};
use anygenesis_gamecfg::GameConfig;
use anygenesis_rom::Rom;

const SIZE: usize = 0x1000;
const HANDLER: u32 = 0x200;
const RESET: u32 = 0x300;

const HINTS: &str = "[[jump_table]]\naddress = 0x316\ncount = 2\nkind = \"word_offset\"\n\
                     [[function]]\naddress = 0x700\n";

/// The program's code, as (address, words).
const CODE: &[(u32, &[u16])] = &[
    (HANDLER, &[0x4E73]),
    (
        RESET,
        &[
            0x6100, 0x00FE, // bsr.w $400
            0x4EB9, 0x0000, 0x0500, // jsr $500
            0x0245, 0x0002, // andi.w #2,d5
            0x323B, 0x5006, // move.w (6,pc,d5.w),d1
            0x4EFB, 0x1002, // jmp (2,pc,d1.w)
            0x0004, 0x000A, // the table at 0x316: 0x31A, 0x320
            0x5242, // addq.w #1,d2
            0x6000, 0x0008, // bra.w $326
            0x4442, // neg.w d2
            0x6000, 0x0002, // bra.w $326
            0x4E40, // trap #0
            0x6100, 0x02D6, // bsr.w $600
            0x4E75, // rts
        ],
    ),
    (
        0x400,
        &[
            0x7003, // moveq #3,d0
            0x30C2, // move.w d2,(a0)+
            0x51C8, 0xFFFC, // dbf d0,$402
            0x4E75, // rts
        ],
    ),
    (
        0x500,
        &[
            0x31C0, 0x8000, // move.w d0,($8000).w
            0x3238, 0x0400, // move.w ($0400).w,d1
            0x1611, // move.b (a1),d3
            0x33C1, 0x00A1, 0x0000, // move.w d1,($a10000).l
            0x3839, 0x00C0, 0x0004, // move.w ($c00004).l,d4
            0x0804, 0x0001, // btst #1,d4
            0x67F4, // beq.s $510
            0x2F03, // move.l d3,-(a7)
            0x588F, // addq.l #4,a7
            0x4E75, // rts
        ],
    ),
    (
        0x600,
        &[
            0x4A03, // tst.b d3
            0x007C, 0x0000, // ori.w #0,sr
            0x6B02, // bmi.s $60a
            0x588F, // addq.l #4,a7
            0x4E75, // rts
        ],
    ),
    (0x700, &[0x60FE]),
];

/// Returns the program's ROM image.
pub fn image() -> Vec<u8> {
    let mut bytes = vec![0; SIZE];
    for vector in 2..48 {
        bytes[vector * 4..vector * 4 + 4].copy_from_slice(&HANDLER.to_be_bytes());
    }
    bytes[4..8].copy_from_slice(&RESET.to_be_bytes());
    for &(address, words) in CODE {
        for (index, word) in words.iter().enumerate() {
            let offset = address as usize + index * 2;
            bytes[offset..offset + 2].copy_from_slice(&word.to_be_bytes());
        }
    }
    bytes
}

/// Analyses the program.
pub fn analysis() -> Analysis {
    let rom = Rom::from_bytes(image()).expect("the synthetic image is a valid ROM");
    let text = format!("sha1 = \"{}\"\n{HINTS}", rom.sha1());
    let config = GameConfig::parse(&text).expect("the synthetic config is valid");
    analyze(&rom, &config).expect("the config carries the image's SHA-1")
}
