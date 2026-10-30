//! Profiles of interpreted 68000 programs: what each instruction ran and where computed
//! calls went, and how long a profile run lasts.

mod common;

use anygenesis_rom::Rom;
use anygenesis_runtime::{
    Exit, Options, Runtime, Vdp,
    profile::{Quiet, run_until_quiet},
};
use anygenesis_shared::Address;

use crate::common::{Scripted, image};

/// `lea $300,a0; jsr (a0); jsr $300; bra *`, and an `rts` at $300.
fn calls() -> Rom {
    let main: &[u16] = &[
        0x41F9, 0x0000, 0x0300, 0x4E90, 0x4EB9, 0x0000, 0x0300, 0x60FE,
    ];
    image(0x200, &[], &[(0x200, main), (0x300, &[0x4E75])])
}

#[test]
fn test_profile_counts_instructions_and_keeps_only_computed_calls() {
    let options = Options {
        max_cycles: 2_000,
        ..Options::default()
    };
    let mut runtime = Runtime::new(calls(), &[], Scripted::new(&[]), options);
    runtime.record_profile();
    let exit = runtime.run();
    assert!(matches!(exit, Exit::CycleCap { .. }), "{exit:?}");
    let profile = runtime.take_profile().expect("a profile was kept");
    let count = |address| profile.count(Address::new(address));
    assert_eq!(
        [count(0x200), count(0x206), count(0x208), count(0x300)],
        [1, 1, 1, 2]
    );
    assert!(count(0x20E) > 1);
    assert_eq!(profile.distinct(), 5);
    let computed = profile.computed_targets();
    assert_eq!(computed.len(), 1, "{computed:?}");
    assert!(computed[&Address::new(0x206)].contains(&Address::new(0x300)));
    assert!(runtime.profile().is_none());
}

#[test]
fn test_profile_run_ends_after_quiet_frames_or_at_the_most() {
    let run = |quiet| {
        let mut runtime = Runtime::new(calls(), &[], Box::<Vdp>::default(), Options::default());
        runtime.record_profile();
        let exit = run_until_quiet(&mut runtime, quiet);
        assert!(matches!(exit, Exit::Frames { .. }), "{exit:?}");
        runtime.frames()
    };
    let settled = Quiet {
        least: 120,
        quiet: 60,
        most: 600,
    };
    assert_eq!(run(settled), 120);
    let capped = Quiet {
        least: 0,
        quiet: 10_000,
        most: 180,
    };
    assert_eq!(run(capped), 180);
}
