# pk2 documentation

Reference material for the PK2 archive format and for this crate.

| Document | Contents |
|---|---|
| [file-format.md](file-format.md) | Header, entry, block and chain layout; how directories are linked; traversal rules |
| [encryption.md](encryption.md) | Blowfish usage, the salt-XOR key derivation, key verification |
| [roadmap.md](roadmap.md) | Known improvement opportunities, recorded for later; none in progress |

## About the format

PK2 is Joymax's container format for *Silkroad Online* client data
(`Media.pk2`, `Map.pk2`, …). It stores an encrypted, block-chained index over a
region of plain, uncompressed file payloads.

There is **no vendor specification**. These documents are reverse-engineered and
cross-checked against independent implementations — chiefly
[Veykril/pk2](https://github.com/Veykril/pk2) (Rust) and
[JellyBitz/SRO.PK2API](https://github.com/JellyBitz/SRO.PK2API) (C#). Claims
that could not be verified are collected under *Open questions* in
[file-format.md](file-format.md#10-open-questions) rather than presented as
fact.

## Known gaps in this implementation

Written down so the format docs can serve as the acceptance criteria for
fixing them. This list shrinks as work lands.

Nothing outstanding. Everything tracked here has been addressed.

### Closed

| Gap | Fixed by |
|---|---|
| Directory walk stopped at the first `type == 0` entry, truncating chains | `fix/block-chain-walk` |
| Directory walk advanced by 128 B instead of following `entry[19].next_block` | `fix/block-chain-walk` |
| `..` was returned as a child of every directory | `fix/block-chain-walk` |
| No cycle guard when following chains | `fix/block-chain-walk` |
| Writes were buffered and flushed on drop, discarding I/O errors | `fix/block-chain-walk` |
| Failures panicked instead of returning a typed error | `fix/block-chain-walk` |
| Header was never read: no signature, version or key check | `feat/production-hardening` |
| The derived Blowfish key was hardcoded | `feat/production-hardening` |
| The header's `encrypted` flag was ignored | `feat/production-hardening` |
| The archive was reopened for every 128-byte read | `feat/production-hardening` |
| Filenames were decoded as UTF-8 rather than EUC-KR | `feat/production-hardening` |
| `Extractor` also patched, so the name did not describe the type | `feat/production-hardening` |
| No repack, so payloads orphaned by `patch` were never reclaimed | `feat/production-hardening` |

## Testing against a real archive

`Media.pk2` is copyrighted Joymax client data and is not redistributable, so
this repository ships no archive fixtures. Generate a conformant one instead:

```bash
cargo install --git https://github.com/Veykril/pk2 pk2_mate
pk2_mate pack -d ./testdata -a test.pk2 -k 169841
```

Put more than 18 files in a single directory so the head block fills and the
writer is forced to allocate a chain — that exercises the block-chaining paths
that a single-block archive never reaches.
