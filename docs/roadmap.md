# Roadmap

Known improvement opportunities, written down so they are not lost.

**Nothing here is in progress.** This is a parking record, not a plan with
dates. Items 1 and 5 are defects; everything else is an enhancement. They are
ordered by value, highest first.

---

## Contents

- [1. `extract` allocates whatever size the archive claims](#1-extract-allocates-whatever-size-the-archive-claims)
- [2. Path resolution re-walks every directory](#2-path-resolution-re-walks-every-directory)
- [3. No streaming extract](#3-no-streaming-extract)
- [4. No tree walk API](#4-no-tree-walk-api)
- [5. Incorrect comment in `plan()`](#5-incorrect-comment-in-plan)
- [6. `Error::Io` carries no context](#6-errorio-carries-no-context)
- [7. No `create` / `pack`](#7-no-create--pack)
- [8. Smaller items](#8-smaller-items)

---

## 1. `extract` allocates whatever size the archive claims

*Defect. The only correctness concern on this list.*

In `Archive::extract` (`src/lib.rs`) the payload read is:

```rust
read_at(&self.file, entry.position, entry.size as usize)
```

`entry.size` is an untrusted `u32` read straight out of the archive's index, and
`read_at` does `vec![0u8; len]` **before** reading anything. A malformed or
hostile archive therefore dictates the allocation size, up to 4 GiB.

Measured: a 2816-byte archive containing one entry claiming a size of 1 GiB
caused a 1 GiB allocation and then returned
`Err("i/o error: failed to fill whole buffer")` after 324µs.

Being accurate about severity: it degraded to a clean error rather than
crashing, because `vec![0u8; n]` goes through `alloc_zeroed` and the pages are
never touched. The concern is what happens when the allocation itself fails —
in Rust that aborts the process rather than returning an error. Under a
container memory limit, or at `u32::MAX`, this becomes a hard abort instead of
an `Err`. It is unbounded allocation driven by untrusted input.

**Suggested fix.** Bounds-check `position + size` against the length of the file
before allocating, and return a typed error when it does not fit. The archive
holds its `File` handle, so `metadata()?.len()` is available without reopening.
The same check is worth applying in `read_block_at`.

---

## 2. Path resolution re-walks every directory

*The biggest practical performance win.*

`Archive::entry` calls `child_named` once per path component, and every
`child_named` calls `children()`, which is a full block-chain walk issuing fresh
I/O. Resolving `a/b/c/d.txt` walks `a`, then `b`, then `c`. Extracting 100 files
out of one directory walks that directory 100 times.

On top of that, `child_named` matches candidates with
`child.name().eq_ignore_ascii_case(name)`, and `name()` allocates a `String` for
every candidate it examines, not just the one it matches.

**Suggested fix.** Build a chain index at open time — a `HashMap<u64, Vec<Entry>>`
mapping a chain offset to its parsed entries. This is the approach
[Veykril/pk2](https://github.com/Veykril/pk2) takes with its `ChainIndex`.
Lookups become hash hits.

This is also the change that makes holding a single shared file handle actually
pay off, and it is the largest practical win for bulk extraction.

---

## 3. No streaming extract

`extract` returns `Vec<u8>`, so pulling a large file out holds all of it in
memory at once. Original archives contain multi-megabyte files.

A handle implementing `Read + Seek`, bounded to the payload's range, would
compose with `io::copy`, make extract-to-disk allocation-free, and sidestep
item 1 entirely for the common case.

---

## 4. No tree walk API

Callers have to recurse `list()` by hand. A `walk()` iterator yielding
`(path, Entry)` pairs is a small addition, and is likely the first thing most
callers reach for.

---

## 5. Incorrect comment in `plan()`

*Defect, documentation only.*

The doc comment on `Archive::plan` says the tree is collected "breadth first",
but the traversal uses `queue.pop()` on a `Vec`, which is LIFO — so it is
depth-first. The resulting layout is correct either way; only the comment is
wrong. Trivial to fix.

---

## 6. `Error::Io` carries no context

`Error::Io(io::Error)` does not record which file or which operation failed, so
a failure reports only e.g. `i/o error: No such file or directory`. For a
library doing as much seeking and offset arithmetic as this one, something like

```rust
Io { op: &'static str, offset: Option<u64>, source: io::Error }
```

would be considerably more useful.

This is also the point at which adopting [`thiserror`](https://docs.rs/thiserror)
would start to pay off, because the variants become structured. Adopting it
purely to delete the current hand-written `Display`, `Error` and `From` impls
would save roughly 33 lines, but would pull `thiserror-impl`, `proc-macro2`,
`quote` and `syn` into a crate that has one optional dependency today and zero
with `--no-default-features`. Not worth it on its own; worth revisiting
alongside this change.

---

## 7. No `create` / `pack`

The crate can read, patch and repack, but it cannot build an archive from
scratch. `write_repacked` already contains the block-chain writer, so this is
largely a matter of feeding a directory tree into the same layout pass rather
than an existing archive's plan.

---

## 8. Smaller items

- **Fuzzing.** The entry and header parsers consume untrusted bytes and are an
  ideal `cargo-fuzz` target — it would surface the whole family of problems in
  item 1 automatically. This has unusually high value here because the format is
  reverse-engineered rather than specified.
- `Entry` is `Copy` at roughly 136 bytes, so it is memcpy'd through every filter
  and collect.
- `Archive` is `!Sync`, because the shared file handle is a `RefCell`. Swapping
  it for a `Mutex` is the single change needed if that ever becomes a
  constraint.
- `repack` cannot change the key on the way out. Re-keying an archive would be a
  small addition to `write_repacked`.
- Not yet published to crates.io, though the package metadata and the dual
  MIT/Apache-2.0 license are now in place for it.
