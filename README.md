# pk2

[![CI](https://github.com/mahmoudfarouq/pk2/actions/workflows/ci.yml/badge.svg)](https://github.com/mahmoudfarouq/pk2/actions/workflows/ci.yml)

A Rust library for reading and modifying Silkroad Online PK2 archives.

```rust
let archive = pk2::Extractor::open("Media.pk2")?;

for entry in archive.list("server_dep/silkroad/textdata")? {
    println!("{}", entry);
}

let bytes = archive.extract("server_dep/silkroad/textdata/weapon.txt")?;
archive.patch("server_dep/silkroad/textdata/weapon.txt", &bytes)?;
```

## Documentation

The archive format is documented in [`docs/`](docs/):

- [`docs/file-format.md`](docs/file-format.md) — header, entry, block and chain
  layout, and the rules for walking an archive
- [`docs/encryption.md`](docs/encryption.md) — Blowfish usage, key derivation
  and key verification

## Status

Reads and patches archives packed with the default international key. Known
limitations are tracked in [`docs/README.md`](docs/README.md#known-gaps-in-this-implementation).

## Testing

`cargo test` runs against archives built in-memory by the test fixtures, so no
game data is needed. To exercise the library against a real archive, generate
one — `Media.pk2` itself is copyrighted client data and is not redistributable:

```bash
cargo install --git https://github.com/Veykril/pk2 pk2_mate
pk2_mate pack -d ./testdata -a test.pk2 -k 169841
```

## Development

CI runs on Linux, macOS and Windows. To reproduce it locally:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo test --doc
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items
python3 scripts/check-doc-links.py docs README.md
```
