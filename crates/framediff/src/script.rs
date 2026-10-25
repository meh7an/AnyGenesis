//! The reference run: a script for the reference emulator's debugger (BlastEm 1.0.0) that
//! stops at every vertical interrupt handler entry, saves the stop in the dump layout and
//! sets pad 1 as the input file says.
//!
//! The emulator runs unmodified, outside the repository, from the folder the dumps go to:
//! `blastem -t -d -r U <rom> < script.txt`. `-t` makes the debugger read the script from
//! standard input; without it the Windows build never reads it.

use std::fmt::Write as _;

use crate::{
    dump::{self, Region},
    input::{Buttons, Input},
};

/// The debugger's `save` arguments for each region: size suffix and source.
fn save_source(region: Region) -> (&'static str, &'static str) {
    match region {
        Region::Ram => ("", "[$FF0000] $10000"),
        Region::Vram => ("", "vdp:vram"),
        Region::Cram => ("/w", "vdp:cram"),
        Region::Vsram => ("/w", "vdp:vsram"),
        Region::Registers => ("", "vdp:reg"),
    }
}

/// A script that stops `stops` times at the vertical interrupt handler `handler`, saving
/// each stop and applying `input` before the handler runs, then quits.
pub fn blastem(handler: u32, stops: u64, input: &Input) -> String {
    let mut script = String::new();
    // Writing to a String cannot fail.
    let mut line = |text: String| writeln!(script, "{text}").expect("writing to a String");
    line(format!("breakpoint ${handler:X}"));
    let mut held = Buttons::default();
    for stop in 1..=stops {
        line("continue".to_owned());
        for region in Region::ALL {
            let (size, source) = save_source(region);
            let file = dump::file_name(stop, region);
            line(format!("save{size} \"{file}\" {source}"));
        }
        let buttons = input.at(stop);
        for name in held.without(buttons).bindings() {
            line(format!("bindup \"gamepads.1.{name}\""));
        }
        for name in buttons.without(held).bindings() {
            line(format!("binddown \"gamepads.1.{name}\""));
        }
        held = buttons;
    }
    line("quit".to_owned());
    script
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blastem_script_saves_every_stop_then_quits() {
        let script = blastem(0xB10, 2, &Input::default());
        let expected = "breakpoint $B10\n\
                        continue\n\
                        save \"00001-ram.bin\" [$FF0000] $10000\n\
                        save \"00001-vram.bin\" vdp:vram\n\
                        save/w \"00001-cram.bin\" vdp:cram\n\
                        save/w \"00001-vsram.bin\" vdp:vsram\n\
                        save \"00001-reg.bin\" vdp:reg\n\
                        continue\n\
                        save \"00002-ram.bin\" [$FF0000] $10000\n\
                        save \"00002-vram.bin\" vdp:vram\n\
                        save/w \"00002-cram.bin\" vdp:cram\n\
                        save/w \"00002-vsram.bin\" vdp:vsram\n\
                        save \"00002-reg.bin\" vdp:reg\n\
                        quit\n";
        assert_eq!(script, expected);
    }

    #[test]
    fn test_blastem_script_presses_and_releases_at_the_input_stops() {
        let input = Input::parse("2 SA\n3 A\n4 -\n").expect("ok");
        let script = blastem(0x200, 4, &input);
        let binds: Vec<(usize, &str)> = script
            .split("continue\n")
            .enumerate()
            .flat_map(|(stop, part)| {
                part.lines()
                    .filter(|line| line.starts_with("bind"))
                    .map(move |line| (stop, line))
            })
            .collect();
        assert_eq!(
            binds,
            [
                (2, "binddown \"gamepads.1.a\""),
                (2, "binddown \"gamepads.1.start\""),
                (3, "bindup \"gamepads.1.start\""),
                (4, "bindup \"gamepads.1.a\""),
            ]
        );
    }
}
