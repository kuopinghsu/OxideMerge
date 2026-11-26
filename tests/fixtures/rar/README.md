# RAR archives for the tests

These RAR archives are fixed input files for the tests in `src/core/archive.rs`.
The tests read them directly rather than generating RAR archives at runtime.

| File | Format | Password | Holds |
|------|--------|----------|-------|
| `v5.rar` | RAR 5 | | `big.txt` (300 lines), `keep.txt`, `dir/a.txt`, an empty `sub/` |
| `v5-solid.rar` | RAR 5, solid | | the same |
| `v5-password.rar` | RAR 5, data encrypted | `secret` | the same |
| `v5-names.rar` | RAR 5, data and names encrypted | `secret` | the same |
| `v5-unicode-password.rar` | RAR 5, data encrypted | `päss密` | `keep.txt` |
| `v5-copies.rar` | RAR 5 | | `one.txt`, `two.txt` stored as a copy of it (`-oi1:0`), `other.txt` |
| `v5-link.rar` | RAR 5 | | `real/a.txt` and `link`, a junction to `real` (`-ol`) |
| `volume.part1.rar` | RAR 5, the first of three volumes | | the start of `small.bin` |
| `volume-names.part1.rar` | RAR 5, the first of five volumes, names encrypted | `secret` | the start of `small.bin` |
| `v4.rar` | RAR 4 | | `VERSION` |
| `v4-password.rar` | RAR 4, data encrypted | `unrar` | `.gitignore` |

The RAR 5 files were created with RAR 7.23, using `rar a -ma5 -m3 -r`.
The variants use `-s` for solid compression, `-psecret` for data encryption,
`-hpsecret` for encrypted file names, and `-v1k` for multiple volumes.
RAR 7.23 does not write RAR 4 files. The RAR 4 files come from `data/version.rar` and
`data/crypted.rar` in the [`unrar`](https://github.com/muja/unrar.rs) crate (MIT OR Apache-2.0).
