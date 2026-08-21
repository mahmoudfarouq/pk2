# pk2 documentation

Reference material for the PK2 archive format and for this crate.

| Document | Contents |
|---|---|
| [file-format.md](file-format.md) | Header, entry, block and chain layout; how directories are linked; traversal rules |
| [encryption.md](encryption.md) | Blowfish usage, the salt-XOR key derivation, key verification |

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

| Gap | Spec reference |
|---|---|
| Header is never read: no signature check, no version check, no key verification | [§2](file-format.md#2-header) |
| The derived Blowfish key is hardcoded, so archives packed with any other key fail to decrypt | [encryption.md — Key derivation](encryption.md#key-derivation) |
| Filenames are decoded as UTF-8; original archives use EUC-KR | [§3 Names](file-format.md#names) |
| The archive is reopened for every 128-byte read | — |
| No repack, so payloads orphaned by `patch` are never reclaimed | [§8 Rewriting a file](file-format.md#rewriting-a-file) |
| `Extractor` also patches, so the name no longer describes the type | — |

### Closed

| Gap | Fixed by |
|---|---|
| Directory walk stopped at the first `type == 0` entry, truncating chains | `fix/block-chain-walk` |
| Directory walk advanced by 128 B instead of following `entry[19].next_block` | `fix/block-chain-walk` |
| `..` was returned as a child of every directory | `fix/block-chain-walk` |
| No cycle guard when following chains | `fix/block-chain-walk` |
| Writes were buffered and flushed on drop, discarding I/O errors | `fix/block-chain-walk` |
| Failures panicked instead of returning a typed error | `fix/block-chain-walk` |

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
