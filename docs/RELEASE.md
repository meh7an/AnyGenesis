# Release procedure

Releases are built locally, on a Windows x64 host and on a Linux x86_64 host (WSL is fine),
and published by hand. GitHub CI does not build them. Users get one file per OS: a Windows
installer and a Linux AppImage; the toolchain, the command-line anygenesis and the folder
layout stay inside them.

## Tools on the build machines

- Windows: Inno Setup 7 (found in its default per-user or per-machine folder, or set
  `ANYGENESIS_ISCC` to its `ISCC.exe`).
- Linux: appimagetool (on PATH, or set `ANYGENESIS_APPIMAGETOOL`).
- Both: the pinned Zig archive named in `xtask/src/toolchain.rs` (`HOST.archive`), from
  ziglang.org; the toolchain task checks its SHA-256.

## 1. Prepare

1. Set the version in `[workspace.package]` of the root `Cargo.toml` (every crate takes it
   from there), then run any cargo command so `Cargo.lock` follows.
2. Run the gate: `cargo fmt --all --check`, `cargo xtask legal`,
   `cargo clippy --workspace --all-targets --locked -- -D warnings`,
   `cargo test --workspace --locked`.
3. Commit. The release task refuses a working tree with changes.

## 2. Build, on each host

1. `cargo xtask toolchain <zig archive>` builds anygenesis, anygenesis-gui and the player
   library into `target/bundle/<host>/` (Windows: static C runtime, no Visual C++ runtime
   needed; Linux: linked against glibc 2.28).
2. `cargo xtask release` stages `target/dist/anygenesis-<version>-<host>/` (the bundle,
   `THIRD-PARTY-NOTICES.txt`, `LICENSE-MIT`, `LICENSE-APACHE`) and builds
   `AnyGenesis-<version>-windows-x64-setup.exe` or `AnyGenesis-<version>-x86_64.AppImage`
   next to it, with its line in `target/dist/SHA256SUMS`. The notices come from
   `Cargo.lock`; the task fails if a crate's licence is not on the allow list in
   `xtask/src/notices.rs`, a crate ships no usable text, or a test-only crate (oracle,
   c-oracle: Musashi, ymfm) is in the tree. `--dirty` skips the clean-commit check for
   trying the packaging; never publish such a build.
3. Merge the Linux `SHA256SUMS` line into the Windows file.

Never put ROMs, ROM-derived files, built games, caches or dumps into a release folder.

## 3. Check on clean machines

- Windows: a fresh VM with nothing installed (Windows Sandbox refuses unsigned programs).
  Run the installer, start AnyGenesis from the Start menu, build and play Sonic 1 and
  Sonic 2, then uninstall.
- Linux: a clean distribution with no compiler (the WSL Ubuntu used since M0.3). Make the
  AppImage executable, start it, build and play both games.

## 4. Publish

1. Push `main`.
2. Tag `v<version>` and push the tag.
3. Create a draft GitHub release on meh7an/AnyGenesis for the tag, attach the installer, the
   AppImage and `SHA256SUMS`, write the notes from the template below, then publish it.

## Release notes template

> AnyGenesis turns your own Sega Genesis / Mega Drive cartridge dump into a native game for
> your PC. It contains no games and never downloads any; a game you build holds your ROM, so
> it is for you only.
>
> - Windows 10 or 11 (x64): run `AnyGenesis-<version>-windows-x64-setup.exe`. No
>   administrator rights needed.
> - Linux x86_64 (glibc 2.28 or newer, a Vulkan driver, libxkbcommon-x11, and
>   xdg-desktop-portal for file dialogs): make `AnyGenesis-<version>-x86_64.AppImage`
>   executable and run it.
>
> The programs are not code-signed yet. If Windows shows "Windows protected your PC",
> choose "More info", then "Run anyway". With Smart App Control on, Windows blocks unsigned
> programs (the games you build too) and offers no exception for them.

## Code signing

Nothing is signed yet. Signing the installer and our programs would not cover zig.exe or the
games users build, so Smart App Control still blocks those. Options for later: Azure Artifact
Signing, SignPath Foundation (open-source projects), or an OV certificate.
