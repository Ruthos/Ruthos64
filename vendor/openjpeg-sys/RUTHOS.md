# Why this crate is here

Ruthos builds this copy of `openjpeg-sys` in place of the crates.io one, through
`[patch.crates-io]` in the workspace and fuzz manifests.

OpenJPEG has marked its repository unmaintained, and `openjpeg-sys` 1.0.12 bundles
2.5.3, so decoder fixes do not reach Ruthos on their own.

| Part | Source |
|---|---|
| `src/`, `build.rs`, `config/`, `README.md` | `openjpeg-sys` 1.0.12 from crates.io, with the version numbers in `config/` raised |
| `vendor/` | The `.c` and `.h` files of `src/lib/openjp2` and `LICENSE` from OpenJPEG tag v2.5.4 |
| `vendor/src/lib/openjp2/j2k.c` | Plus upstream commit 91d08b11a: the bounds check in `opj_j2k_read_sod` |

`scripts/check_codecs.py --verify-vendor` compares `vendor/` with the upstream tag in
CI. Every file must match it, except those under `patched` in `codecs.toml`, whose
SHA-256 is recorded there. To add a fix, apply the upstream commit here, name it in the
table above and update the SHA-256.
