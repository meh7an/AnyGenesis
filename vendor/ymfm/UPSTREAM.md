# Vendored ymfm

- Source: https://github.com/aaronsgiles/ymfm
- Commit: `81aec25ccbb98f4873a255f7551ac4dadac59b4a` (main, 2026-07-27; the project has no tags)
- License: BSD 3-Clause, in `LICENSE`.
- Files are unmodified. Kept: `LICENSE`, `README.md` and the sources the YM2612 build
  compiles or includes (`src/ymfm.h`, `ymfm_fm.h`, `ymfm_fm.ipp`, `ymfm_opn.h/.cpp`,
  `ymfm_ssg.h/.cpp`, `ymfm_adpcm.h/.cpp`; `ymfm_opn.cpp` defines the OPNA/OPNB classes too,
  which need the SSG and ADPCM parts).
- Left out: the other chips' sources (OPL, OPM, OPQ, OPZ, PCM, misc), `examples/`,
  `GeneralInfo.md`, build files.
- Test-only: built by `crates/oracle` as the YM2612 oracle and never linked into anything
  that ships. Read only as a reference; nothing from it is copied into crates/ym2612.
