# The PK2 Archive Format

PK2 (also "Joymax Pak File") is the container format used by Joymax's *Silkroad
Online* client for `Media.pk2`, `Map.pk2`, `Music.pk2`, `Particles.pk2` and
friends.

It is a **flat file store with an encrypted, block-chained index**. Think of it
as a very small FAT-style filesystem embedded in one file:

- the *index* is a tree of fixed-size directory blocks, Blowfish-encrypted;
- the *data* is raw file bytes, stored verbatim with no encryption and no
  compression.

> **Provenance.** There is no vendor specification. Everything here is
> reverse-engineered by the community and cross-checked against independent
> implementations. Anything not verified is called out explicitly in
> [Open questions](#10-open-questions).

---

## Contents

- [1. Archive layout](#1-archive-layout)
- [2. Header](#2-header)
- [3. Entry](#3-entry)
- [4. Block](#4-block)
- [5. Block chains](#5-block-chains)
- [6. How the tree fits together](#6-how-the-tree-fits-together)
- [7. Walking the archive](#7-walking-the-archive)
- [8. File data](#8-file-data)
- [9. Constants](#9-constants)
- [10. Open questions](#10-open-questions)

---

## 1. Archive layout

Only two offsets are fixed: the header at `0x0000` and the root block at
`0x0100`. Everything after that is reached by following pointers — blocks and
file payloads are interleaved in whatever order the writer allocated them.

```
        ┌──────────────────────────────────────────────────────────┐
0x0000  │  PackHeader                                     256 B    │
        │  signature · version · encrypted · verify                │
        ├──────────────────────────────────────────────────────────┤
0x0100  │  Root PackBlock                                2560 B    │
        │  first (and possibly only) block of the root chain       │
        ├──────────────────────────────────────────────────────────┤
0x0B00  │                                                          │
        │  Blocks and file payloads, interleaved in                │
        │  allocation order.                                       │
        │                                                          │
        │  There is NO ordering guarantee here. Never assume       │
        │  a block follows another in the file just because it     │
        │  follows it in a chain. Follow the pointers.             │
        │                                                          │
        └──────────────────────────────────────────────────────────┘
```

All multi-byte integers are **little-endian**.

---

## 2. Header

256 bytes, at offset `0x0000`. Never encrypted itself, but the `verify` field
holds Blowfish ciphertext used to validate the key.

```
offset  size  field       type       value / notes
──────  ────  ─────────   ────────   ───────────────────────────────────────
0x00      30  signature   char[30]   "JoyMax File Manager!\n" + NUL padding
0x1E       4  version     u32 LE     0x01000002
0x22       1  encrypted   u8         0 = blocks are plaintext
                                     otherwise = blocks are Blowfish-encrypted
0x23      16  verify      u8[16]     Blowfish("Joymax Pak File\0")
0x33     205  reserved    u8[205]    zero
──────  ────
0x100    256  total
```

Two independent checks, and they answer different questions:

| Check | Field | Answers |
|---|---|---|
| Signature + version | `signature`, `version` | "Is this a PK2 file I understand?" |
| Checksum | `verify` | "Is my Blowfish key correct?" |

**The checksum quirk:** encrypting the 16-byte constant `"Joymax Pak File\0"`
produces 16 bytes, but **only the first 3 are actually compared**. The
remaining 13 bytes of `verify` are not reliable. Compare 3 bytes, no more.

See [encryption.md](encryption.md) for how the key is derived.

---

## 3. Entry

The atom of the index. Exactly **128 bytes**, which is a multiple of the
Blowfish block size (8), so entries encrypt cleanly on their own.

```
offset  size  field         type       meaning
──────  ────  ───────────   ────────   ─────────────────────────────────────
0x00       1  type          u8         0 = empty · 1 = directory · 2 = file
0x01      81  name          char[81]   EUC-KR, NUL-terminated, NUL-padded
0x52       8  access_time   FILETIME   100 ns ticks since 1601-01-01 UTC
0x5A       8  create_time   FILETIME
0x62       8  modify_time   FILETIME
0x6A       8  position      u64 LE     see table below
0x72       4  size          u32 LE     payload length — files only
0x76       8  next_block    u64 LE     chain pointer — see §5
0x7E       2  padding       u8[2]      pads 126 → 128
──────  ────
0x80     128  total
```

### `position` is overloaded

This is the single most important thing to internalise about the format. The
same 8 bytes mean two completely different things:

| `type` | `position` points to | `size` |
|---|---|---|
| `1` directory | the **first block of the child chain** | unused |
| `2` file | the **raw payload** in the data region | payload length |
| `0` empty | unused | unused |

### Empty entries are not inert

An entry with `type == 0` has no name, no timestamps and no position — **but
its `next_block` field is still live**. A block can be half empty and still
chain to another block.

```
        an "empty" entry, byte 0x00 .. 0x7F
        ┌────┬──────────────────────────────────┬────────────┬────┐
        │ 00 │  garbage / stale / zero          │ next_block │ ?? │
        └────┴──────────────────────────────────┴────────────┴────┘
          ▲   0x01                          0x76 ▲       0x7E
          │                                      │
     type = 0                        STILL VALID — must be read
```

A reader that stops at the first `type == 0` entry will silently truncate any
directory whose chain continues past a partially-filled block.

### Names

81 bytes, **EUC-KR**, not UTF-8 or ASCII. Read up to the first NUL, then decode.
Original Joymax archives contain Korean filenames; a UTF-8 decoder will fail on
them. The effective maximum name length is 80 bytes plus the terminator.

---

## 4. Block

20 entries, contiguous: **2560 bytes** (`0xA00`).

```
        PackBlock — 2560 bytes = 20 × 128
                                             block-relative
        ┌───────────────────────────────────┐   offset
        │ entry[ 0]   "."                   │   0x000   → this chain
        ├───────────────────────────────────┤
        │ entry[ 1]   ".."                  │   0x080   → parent chain
        ├───────────────────────────────────┤
        │ entry[ 2]                         │   0x100  ┐
        │    ⋮        files / dirs / empty  │    ⋮     │  payload
        │ entry[18]                         │   0x900  ┘
        ├───────────────────────────────────┤
        │ entry[19]   file / dir / empty    │   0x980  ← ALSO holds
        │                                   │            next_block
        └───────────────────────────────────┘
                                                0xA00 = 2560
```

Two rules that are easy to get wrong:

1. **`.` and `..` exist only in the first block of a chain.** Continuation
   blocks have no self/parent entries — `entry[0]` there is an ordinary payload
   entry. `.` points at its own chain, `..` at the parent's.

2. **`next_block` is only meaningful in `entry[19]`.** The field exists in all
   20 entries because they share one struct, but only the last one is read.
   Ignore it everywhere else.

Because a block is 2560 bytes and Blowfish operates on 8-byte blocks in ECB
mode, decrypting a whole block at once and decrypting each 128-byte entry
individually give identical results. Either is valid.

---

## 5. Block chains

A directory's contents are a **linked list of blocks**, called a *chain*. The
chain head's offset is what a directory entry's `position` points at.

```
   directory entry "textdata"  (type = 1)
             │
             │  position = 0x01A400
             ▼
   ┌─────────────────────────┐  0x01A400   ◄── chain head: this offset
   │ block A                 │                 identifies the whole chain
   │  [ 0] "."               │
   │  [ 1] ".."              │
   │  [ 2..19] children      │
   │  [19].next_block ───────┼──┐
   └─────────────────────────┘  │
                                │
   ┌─────────────────────────┐  │  0x02F800
   │ block B                 │◄─┘
   │  [ 0..19] children      │     no "." / ".." — continuation block
   │  [19].next_block ───────┼──┐
   └─────────────────────────┘  │
                                │
   ┌─────────────────────────┐  │  0x031C00
   │ block C                 │◄─┘
   │  [ 0.. 6] children      │
   │  [ 7..19] empty         │     holes are legal and must be skipped,
   │  [19].next_block = 0    │     not treated as end-of-directory
   └─────────────────────────┘
             ▲
             └── next_block == 0 marks the end of the chain
```

So a directory holding 47 children occupies `ceil((47 + 2) / 20) = 3` blocks —
the `+ 2` accounting for `.` and `..` in the head block.

**A chain is never empty.** Every directory has at least one block, because it
always has at least `.` and `..`.

---

## 6. How the tree fits together

Directory entries point at chains; chains contain entries; some of those are
directory entries pointing at further chains. That recursion is the whole
filesystem.

```
  ROOT CHAIN                                  0x000100
  ┌────────────────────────────────────┐
  │ [0] "."   dir  position=0x000100 ──┼──┐ (self)
  │ [1] ".."  dir  position=0x000100 ──┼──┘ (root's parent is itself)
  │ [2] "server_dep"   dir             │
  │           position = 0x000B00 ─────┼──────────┐
  │ [3] "music"        dir             │          │
  │           position = 0x04C200 ─────┼───┐      │
  │ [4] "readme.txt"   file            │   │      │
  │           position = 0x1F3A00 ───┐ │   │      │
  │           size     = 4096        │ │   │      │
  │ [5..19] empty                    │ │   │      │
  └──────────────────────────────────┼─┘   │      │
                                     │     │      │
      ┌──────────────────────────────┘     │      │
      │                                    │      │
      ▼  DATA REGION                       │      ▼  CHAIN "server_dep"
  ┌───────────────────┐                    │  ┌────────────────────────┐
  │ 0x1F3A00          │                    │  │ [0] "."   → 0x000B00   │
  │ raw bytes, 4096   │                    │  │ [1] ".."  → 0x000100   │
  │ not encrypted     │                    │  │ [2] "silkroad" dir     │
  │ not compressed    │                    │  │        position ───────┼──▶ ...
  └───────────────────┘                    │  │ [3..19] empty          │
                                           │  └────────────────────────┘
                                           ▼
                                       CHAIN "music"  0x04C200
```

Note what `..` gives you: a chain knows its parent, so upward traversal is free
and `a/../b` resolves naturally. Note also that **`.` and `..` are ordinary
directory entries** — a naive lister will happily return them as children. Skip
them by name.

---

## 7. Walking the archive

### Resolving a path

```
  resolve("server_dep/silkroad/textdata")

  chain ← root chain at 0x0100
    │
    ├─ component "server_dep"
    │     scan every entry of every block in chain
    │     match name, case-insensitive, must be type 1
    │     chain ← entry.position
    │
    ├─ component "silkroad"        (same)
    │
    └─ component "textdata"        (same)
          │
          └─▶ resulting chain offset
```

### Listing a chain

```
  entries ← []
  block   ← read_block(chain_offset)

  loop:
      for i in 0..20:
          e ← block.entry[i]
          if e.type != 0 and e.name not in {".", ".."}:
              entries.push(e)          ◄── do NOT break on type == 0

      if block.entry[19].next_block == 0:
          break                        ◄── the ONLY termination condition
      block ← read_block(block.entry[19].next_block)

  return entries
```

The two failure modes worth naming, because both produce plausible-looking
wrong answers rather than errors:

| Mistake | Symptom |
|---|---|
| Break on the first `type == 0` | Directories silently truncate |
| Advance by 128 B past `entry[19]` instead of following `next_block` | Reader walks off the block into payload bytes, decrypts noise, and emits phantom entries whenever a random byte lands on 1 or 2 |

A robust reader should also **guard against cycles**. `..` points backwards by
design, and a corrupt archive can point a chain at itself; track visited block
offsets.

---

## 8. File data

There is nothing clever here, which is worth stating plainly because it is
often assumed otherwise:

- File payloads are **not encrypted**.
- File payloads are **not compressed**.
- A payload is `size` bytes starting at `position`. That is the entire contract.

Payloads live wherever the writer put them, interleaved with index blocks.

### Rewriting a file

The format has no free-list. Writers therefore tend to:

1. append the new payload at end-of-file;
2. update the entry's `position` and `size` in place;
3. leave the old payload stranded.

This is correct but monotonically grows the archive — the old bytes are
unreachable yet still present. Reclaiming them requires a full repack.

```
        before                          after patching "a.txt"

   ┌──────────────┐                ┌──────────────┐
   │ index blocks │                │ index blocks │  entry.position updated
   ├──────────────┤                ├──────────────┤            │
   │ a.txt  v1    │                │ a.txt  v1    │  ◄── orphaned, unreachable
   ├──────────────┤                ├──────────────┤
   │ b.txt        │                │ b.txt        │
   └──────────────┘                ├──────────────┤
                                   │ a.txt  v2    │  ◄── appended ◄──┘
                                   └──────────────┘
```

---

## 9. Constants

```
PK2_SIGNATURE          "JoyMax File Manager!\n" padded to 30 bytes with NULs
PK2_VERSION            0x01000002
PK2_CHECKSUM           "Joymax Pak File\0"        (16 bytes)
PK2_CHECKSUM_STORED    3                          (bytes actually compared)

HEADER_SIZE            256      0x100
ENTRY_SIZE             128      0x80
BLOCK_ENTRY_COUNT      20
BLOCK_SIZE             2560     0xA00
ROOT_BLOCK_OFFSET      256      0x100

NAME_BYTES             81
FILETIME epoch         1601-01-01 UTC, 100 ns ticks
UNIX epoch as FILETIME 116444736000000000
```

---

## 10. Open questions

Documented honestly rather than guessed at:

- **Root's `..`.** Writers set the root chain's `..` to the root chain itself.
  Whether every real Joymax archive does this, or leaves it zero, is unverified.
  Readers should tolerate both.
- **Timestamp fidelity.** The three FILETIME fields are structurally confirmed,
  but Joymax's own writer does not appear to maintain them consistently. Treat
  them as advisory.
- **`reserved[205]`.** Assumed zero. Not confirmed to be zero in every shipped
  archive; do not validate it.
- **Entry padding.** The trailing 2 bytes are assumed to be pure alignment.
  No known archive stores anything meaningful there.
- **Version values other than `0x01000002`.** No other version has been
  observed in the wild. Behaviour is undefined.

---

## References

Independent implementations used to cross-check this document:

- [Veykril/pk2](https://github.com/Veykril/pk2) — Rust, read + write, the most
  complete public implementation.
- [JellyBitz/SRO.PK2API](https://github.com/JellyBitz/SRO.PK2API) — C#.
- [SilkroadDoc wiki](https://github.com/DummkopfOfHachtenduden/SilkroadDoc/wiki)
  — community format notes.
