# AnyGenesis

AnyGenesis turns a Sega Genesis / Mega Drive game into a native program. It statically
recompiles the cartridge's 68000 code to C, compiles that C with clang on your own machine,
and links it with a small runtime into one self-contained executable for that game.

## How it works

- **From the ROM alone.** The game's code is found by analysing the ROM image; there are no
  per-game configuration files or hints.
- **Native everywhere.** Every 68000 instruction the game runs is native code. Each routine
  becomes a C function, and every instruction address the analysis keeps also gets a native
  entry point, so jumps the analysis did not predict still land in native code.
- **Exact timing.** The runtime models the VDP, the Z80 sound CPU, the PSG and the YM2612
  against the 68000's cycle count, and takes interrupts at the exact instruction the
  hardware would.
- **Checked at every layer.** Instruction test suites, reference CPU cores and frame-by-frame
  comparison with an external reference emulator validate each part.

## Status

Early development. Sonic the Hedgehog is playable through Green Hill Zone Act 1. Sound,
more games and a builder app are in progress. Windows x64 and Linux come first; macOS and
Windows on ARM follow.

## Legal

- **No game data.** This repository holds no ROM images, no code or data generated from
  them, and no extracted assets, and its tooling refuses to commit any. You need your own
  dump of a cartridge you own.
- **Built games stay with you.** An executable built from a ROM contains that ROM. It is for
  your own use; do not share it.
- **Clean room.** The code is written from public documentation and test suites. No code
  from GPL, AGPL or non-commercial emulators is used.
- AnyGenesis is not affiliated with or endorsed by Sega. Sega, Genesis, Mega Drive and
  Sonic the Hedgehog are trademarks of Sega Corporation.

## Third-party code

- [Musashi](https://github.com/kstenerud/Musashi) (`vendor/musashi`, its license in
  `readme.txt`; SoftFloat keeps its own notice) and
  [ymfm](https://github.com/aaronsgiles/ymfm) (`vendor/ymfm`, BSD 3-Clause) are test-only
  reference cores. Neither is linked into anything that ships.
- [SDL3](https://libsdl.org) (zlib license) provides the window, input and audio.
- Rust dependencies are under their own licenses.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.
