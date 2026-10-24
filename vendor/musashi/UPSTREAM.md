# Vendored Musashi

- Source: https://github.com/kstenerud/Musashi
- Commit: `313ebf1bd9f4d0d93341eb5ce21fd8a119e9dbdd` (master, 2026-03-08)
- Files are unmodified. Upstream has no LICENSE file; the license is in `readme.txt` and in
  each source header. `softfloat/` keeps its own notice in `softfloat/README.txt`.
- Left out: `test/`, `example/`, `Makefile`, `.gitignore`. `test/` and `example/` hold
  `.bin` files the legal guard rejects.
- Test-only: built by `crates/oracle` and never linked into anything that ships.
