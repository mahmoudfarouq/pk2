# PK2 Encryption

PK2 protects **only its index**. File payloads are stored verbatim. The cipher
is standard Blowfish in ECB mode, with a Joymax-specific key derivation bolted
on the front.

> Blowfish here is obfuscation, not security. The key is a fixed constant
> shipped in every client. Nothing in this document should be reused as a
> security primitive.

---

## What is and is not encrypted

```
   ┌──────────────────────────────────────────────────────────────┐
   │ PackHeader                                                   │
   │   signature   ░░░░░  plaintext                               │
   │   version     ░░░░░  plaintext                               │
   │   encrypted   ░░░░░  plaintext                               │
   │   verify      █████  ENCRYPTED  (key check)                  │
   │   reserved    ░░░░░  plaintext                               │
   ├──────────────────────────────────────────────────────────────┤
   │ Index blocks   █████████████████  ENCRYPTED, every byte      │
   │                █████████████████  (if header.encrypted != 0) │
   ├──────────────────────────────────────────────────────────────┤
   │ File payloads  ░░░░░░░░░░░░░░░░░  plaintext, uncompressed    │
   └──────────────────────────────────────────────────────────────┘
```

If `header.encrypted == 0` the index blocks are plaintext too. Some tools
produce such archives; a reader must honour the flag rather than assume
encryption.

---

## Key derivation

The key you type (`169841` for the international client) is **not** the Blowfish
key. It is XOR-folded against a fixed 10-byte salt first.

```
PK2_SALT = 03 F8 E4 44 88 99 3F 64 FE 35
```

```
   ascii key   "1"  "6"  "9"  "8"  "4"  "1"
                31   36   39   38   34   31
                 │    │    │    │    │    │
   PK2_SALT     03   F8   E4   44   88   99          XOR
                 │    │    │    │    │    │
                 ▼    ▼    ▼    ▼    ▼    ▼
   Blowfish     32   CE   DD   7C   BC   A8
   key
```

In pseudocode:

```
derive_key(ascii_key):
    n = min(len(ascii_key), 56)
    for i in 0..n:
        ascii_key[i] ^= PK2_SALT[i]      # PK2_SALT is zero-extended past 10
    return ascii_key
```

Notes:

- The salt is 10 bytes and is conceptually zero-extended to 56. A key longer
  than 10 bytes therefore passes its tail through **unchanged**.
- Blowfish accepts 4–56 byte keys; keys outside that range must be rejected.
- The derived key then goes through the ordinary Blowfish key schedule with the
  standard P-array and S-boxes. There is nothing Joymax-specific after this
  point.

### Known keys

| Client | ASCII key | Derived Blowfish key |
|---|---|---|
| International Silkroad Online | `169841` | `32 CE DD 7C BC A8` |

Other regions and private servers repack with their own keys. **Store the ASCII
key and derive**, rather than hardcoding the derived bytes — hardcoding the
derived form makes every other archive unopenable and, worse, unopenable
*silently*.

---

## Verifying a key

This is what the header's `verify` field is for, and it is the difference
between "wrong key" and "this archive is corrupt garbage".

```
   candidate key
        │
        ▼
   derive_key ──▶ Blowfish ──▶ encrypt("Joymax Pak File\0")
                                          │
                                          ▼
                                   16 bytes of ciphertext
                                          │
                                   compare FIRST 3 BYTES
                                          │
                          ┌───────────────┴───────────────┐
                          ▼                               ▼
                 matches header.verify[0..3]        does not match
                          │                               │
                     key is correct                  wrong key —
                                                   fail loudly here
```

**Only 3 bytes are compared.** The header nominally reserves 16 bytes for the
checksum but the original writer only ever populated 3 of them; the other 13 are
unreliable and must not be compared.

Three bytes is a weak check — roughly a 1-in-16.7-million false accept — but it
is vastly better than the alternative, which is decrypting the root block with a
wrong key and interpreting the resulting noise as directory entries.

Skip verification when `header.encrypted == 0`; there is no ciphertext to check
against.

---

## Block cipher details

- **Mode:** ECB. Each 8-byte block is independent. There is no IV and no
  chaining between blocks.
- **Word order:** each 8-byte block is read as two little-endian `u32`s —
  `L` from bytes `0..4`, `R` from bytes `4..8`.
- **Granularity:** because ECB has no inter-block state, decrypting a whole
  2560-byte index block in one call and decrypting each 128-byte entry
  separately produce identical output. Both are correct; pick whichever suits
  your I/O pattern.
- **No padding scheme.** Every encrypted region in PK2 is already a multiple of
  8 bytes (128-byte entries, 2560-byte blocks, the 16-byte checksum), so the
  question never arises.

```
        one 8-byte unit

        byte  0    1    2    3    4    5    6    7
             ┌────┬────┬────┬────┬────┬────┬────┬────┐
             │      L (u32 LE)   │      R (u32 LE)   │
             └────┴────┴────┴────┴────┴────┴────┴────┘
                        │                 │
                        └──── 16 rounds ──┘
                             standard Blowfish
```

---

## References

- Bruce Schneier, *Description of a New Variable-Length Key, 64-Bit Block
  Cipher (Blowfish)*, 1993 — for the P-array, S-boxes and round function.
- [Veykril/pk2 `blowfish.rs`](https://github.com/Veykril/pk2/blob/master/src/blowfish.rs)
  — reference implementation of the salt derivation.
