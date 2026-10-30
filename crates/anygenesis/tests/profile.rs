//! How much of a run each discovery stage puts in optimised code, on the ROM
//! `ANYGENESIS_ROM` names (ignored without it). The build's own profile run (title, attract
//! mode and demos, no input) is measured, and with `ANYGENESIS_INPUT` (an input file) and
//! `ANYGENESIS_FRAMES` also a replay of that recording, which shows how the attract-mode
//! profile carries over to play. An instruction counts as optimised when a translated
//! function of the stage's analysis holds its address; the run is on the interpreter, so it
//! is the path every build of the game takes.

use std::{collections::BTreeSet, env, path::PathBuf, thread};

use anygenesis_analysis::{Analysis, DiscoveryStage, analyze_profiled, analyze_stage};
use anygenesis_framediff::Input;
use anygenesis_recomp::build;
use anygenesis_rom::Rom;
use anygenesis_runtime::{
    Buttons, Options, RUN_STACK, Runtime, Vdp,
    profile::{Quiet, run_until_quiet},
};
use anygenesis_shared::{Address, Profile};

const VBLANK_LEVEL: u8 = 6;

/// Runs `rom` from reset on the interpreter, keeping a profile: with `replay`, that input
/// for that many frames; without it, as the build's profile run.
fn profile(rom: Rom, replay: Option<(Input, u64)>) -> (Profile, u64) {
    thread::Builder::new()
        .stack_size(RUN_STACK)
        .spawn(move || {
            let options = Options {
                stop_when_idle: true,
                ..Options::default()
            };
            let mut runtime = Runtime::new(rom, &[], Box::<Vdp>::default(), options);
            runtime.record_profile();
            match replay {
                Some((input, frames)) => {
                    runtime.set_interrupt_hook(VBLANK_LEVEL, move |stop| {
                        let buttons = input.at(stop.taken());
                        stop.set_pad(Buttons(buttons.0));
                    });
                    runtime.run_frames(frames);
                }
                None => {
                    run_until_quiet(&mut runtime, Quiet::BUILD);
                }
            }
            let frames = runtime.frames();
            (runtime.take_profile().expect("a profile was kept"), frames)
        })
        .expect("the run starts")
        .join()
        .expect("the run finishes")
}

/// The addresses of every instruction in a translated function of `analysis`.
fn optimised(analysis: &Analysis) -> BTreeSet<Address> {
    analysis
        .functions
        .values()
        .flat_map(|function| &function.blocks)
        .flat_map(|start| {
            let block = &analysis.blocks[start];
            analysis
                .instructions
                .range(block.start..=block.last)
                .map(|(&address, _)| address)
        })
        .collect()
}

/// The share of the instructions `profile` counted that `optimised` holds, in percent.
fn share(profile: &Profile, optimised: &BTreeSet<Address>) -> f64 {
    let covered: u64 = profile
        .executed()
        .filter(|(address, _)| optimised.contains(address))
        .map(|(_, count)| count)
        .sum();
    covered as f64 * 100.0 / profile.total() as f64
}

#[test]
#[ignore = "needs ANYGENESIS_ROM; runs the game on the interpreter for minutes"]
fn test_profile_share_of_executed_instructions_in_optimised_code_per_stage() {
    let path = PathBuf::from(env::var_os("ANYGENESIS_ROM").expect("ANYGENESIS_ROM is set"));
    let rom = build::load_rom(&path).expect("the ROM loads");
    let (attract, frames) = profile(rom.clone(), None);
    let stages = [
        ("vectors", analyze_stage(&rom, DiscoveryStage::Vectors)),
        ("static", analyze_stage(&rom, DiscoveryStage::Static)),
        ("profile", analyze_profiled(&rom, &attract)),
    ];
    let covers: Vec<(&str, usize, BTreeSet<Address>)> = stages
        .iter()
        .map(|(name, analysis)| (*name, analysis.functions.len(), optimised(analysis)))
        .collect();

    let mut runs = vec![(format!("attract, {frames} frames"), attract)];
    if let Some(input) = env::var_os("ANYGENESIS_INPUT") {
        let frames: u64 = env::var("ANYGENESIS_FRAMES")
            .expect("ANYGENESIS_FRAMES is set with ANYGENESIS_INPUT")
            .parse()
            .expect("ANYGENESIS_FRAMES is a frame count");
        let input = Input::load(&PathBuf::from(input)).expect("the input file loads");
        let (replay, _) = profile(rom.clone(), Some((input, frames)));
        runs.push((format!("replay, {frames} frames"), replay));
    }

    println!("{} (SHA-1 {})", rom.title(), rom.sha1());
    for (name, functions, cover) in &covers {
        println!(
            "{name}: {functions} functions, {} instructions",
            cover.len()
        );
    }
    for (run, profile) in &runs {
        let shares: Vec<String> = covers
            .iter()
            .map(|(name, _, cover)| format!("{name} {:.2}%", share(profile, cover)))
            .collect();
        println!(
            "{run}: {} instructions run ({} addresses): {}",
            profile.total(),
            profile.distinct(),
            shares.join(", ")
        );
    }
    let attract = &runs[0].1;
    assert!(share(attract, &covers[2].2) >= share(attract, &covers[1].2));
}
