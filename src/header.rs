//! The 256-byte archive header.
//!
//! Carries two independent checks: whether this is a PK2 file at all
//! (signature and version), and whether the key is right (an encrypted
//! checksum). See `docs/file-format.md` §2.

use crate::blowfish::BlowFish;

/// Size of the header. The root block begins immediately after it.
pub const HEADER_SIZE: usize = 256;

/// The signature every archive starts with, NUL-padded to 30 bytes.
const SIGNATURE: &[u8; 30] = b"JoyMax File Manager!\n\0\0\0\0\0\0\0\0\0";
/// The only version observed in the wild.
const VERSION: u32 = 0x0100_0002;
/// Plaintext whose ciphertext the header stores, so a key can be checked.
const CHECKSUM: &[u8; 16] = b"Joymax Pak File\0";
/// How many checksum bytes the format actually stores.
///
/// The header reserves 16, but the original writer only ever populated 3. The
/// remaining 13 are unreliable and must not be compared.
const CHECKSUM_STORED: usize = 3;

const OFF_SIGNATURE: usize = 0x00;
const OFF_VERSION: usize = 0x1E;
const OFF_ENCRYPTED: usize = 0x22;
const OFF_VERIFY: usize = 0x23;
const OFF_RESERVED: usize = 0x33;
const RESERVED_BYTES: usize = 205;

/// A parsed archive header.
pub struct Header {
    signature: [u8; 30],
    version: u32,
    encrypted: bool,
    verify: [u8; 16],
    /// Kept so rewriting a header preserves bytes we do not interpret.
    #[allow(dead_code, reason = "read back by to_bytes, which repack uses")]
    reserved: [u8; RESERVED_BYTES],
}

impl Header {
    /// Build a header describing an archive encrypted with `blowfish`.
    #[allow(dead_code, reason = "used by tests and by repack")]
    pub fn new_encrypted(blowfish: &BlowFish) -> Self {
        let mut verify = *CHECKSUM;
        blowfish.encrypt(&mut verify);

        Self {
            signature: *SIGNATURE,
            version: VERSION,
            encrypted: true,
            verify,
            reserved: [0; RESERVED_BYTES],
        }
    }

    pub fn parse(raw: &[u8; HEADER_SIZE]) -> Self {
        let mut signature = [0u8; 30];
        signature.copy_from_slice(&raw[OFF_SIGNATURE..OFF_SIGNATURE + 30]);

        let mut verify = [0u8; 16];
        verify.copy_from_slice(&raw[OFF_VERIFY..OFF_VERIFY + 16]);

        let mut reserved = [0u8; RESERVED_BYTES];
        reserved.copy_from_slice(&raw[OFF_RESERVED..OFF_RESERVED + RESERVED_BYTES]);

        Self {
            signature,
            version: u32::from_le_bytes([
                raw[OFF_VERSION],
                raw[OFF_VERSION + 1],
                raw[OFF_VERSION + 2],
                raw[OFF_VERSION + 3],
            ]),
            encrypted: raw[OFF_ENCRYPTED] != 0,
            verify,
            reserved,
        }
    }

    #[allow(dead_code, reason = "used by tests and by repack")]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut raw = vec![0u8; HEADER_SIZE];
        raw[OFF_SIGNATURE..OFF_SIGNATURE + 30].copy_from_slice(&self.signature);
        raw[OFF_VERSION..OFF_VERSION + 4].copy_from_slice(&self.version.to_le_bytes());
        raw[OFF_ENCRYPTED] = u8::from(self.encrypted);
        raw[OFF_VERIFY..OFF_VERIFY + 16].copy_from_slice(&self.verify);
        raw[OFF_RESERVED..OFF_RESERVED + RESERVED_BYTES].copy_from_slice(&self.reserved);
        raw
    }

    /// Whether the index is Blowfish-encrypted. Some tools emit archives with
    /// a plaintext index.
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    /// Whether this looks like a PK2 archive at all.
    pub fn has_valid_signature(&self) -> bool {
        &self.signature == SIGNATURE
    }

    /// The version recorded in the header.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Whether this version is one we know how to read.
    pub fn is_supported_version(&self) -> bool {
        self.version == VERSION
    }

    /// Whether `blowfish` is the key this archive was written with.
    ///
    /// Only the first [`CHECKSUM_STORED`] bytes are compared, because that is
    /// all the format stores. Three bytes is a weak check — roughly a
    /// 1-in-16.7-million false accept — but it is far better than decrypting
    /// the root block with a wrong key and reading the resulting noise as
    /// directory entries.
    pub fn key_matches(&self, blowfish: &BlowFish) -> bool {
        let mut expected = *CHECKSUM;
        blowfish.encrypt(&mut expected);
        expected[..CHECKSUM_STORED] == self.verify[..CHECKSUM_STORED]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> BlowFish {
        BlowFish::from_ascii_key(b"169841").unwrap()
    }

    #[test]
    fn signature_is_exactly_thirty_bytes() {
        assert_eq!(SIGNATURE.len(), 30);
        assert!(SIGNATURE.starts_with(b"JoyMax File Manager!\n"));
    }

    #[test]
    fn field_offsets_tile_the_header() {
        assert_eq!(OFF_SIGNATURE + 30, OFF_VERSION);
        assert_eq!(OFF_VERSION + 4, OFF_ENCRYPTED);
        assert_eq!(OFF_ENCRYPTED + 1, OFF_VERIFY);
        assert_eq!(OFF_VERIFY + 16, OFF_RESERVED);
        assert_eq!(OFF_RESERVED + RESERVED_BYTES, HEADER_SIZE);
    }

    #[test]
    fn round_trips_through_bytes() {
        let header = Header::new_encrypted(&key());
        let raw: [u8; HEADER_SIZE] = header.to_bytes().try_into().unwrap();
        let parsed = Header::parse(&raw);

        assert!(parsed.has_valid_signature());
        assert!(parsed.is_supported_version());
        assert!(parsed.is_encrypted());
        assert_eq!(parsed.verify, header.verify);
        assert_eq!(parsed.to_bytes(), header.to_bytes());
    }

    #[test]
    fn accepts_the_key_it_was_built_with() {
        assert!(Header::new_encrypted(&key()).key_matches(&key()));
    }

    #[test]
    fn rejects_a_different_key() {
        let header = Header::new_encrypted(&key());
        let wrong = BlowFish::from_ascii_key(b"000000").unwrap();
        assert!(!header.key_matches(&wrong));
    }

    #[test]
    fn compares_only_the_stored_checksum_bytes() {
        // Corrupting a byte past the third must not change the verdict, since
        // the format never stored those bytes reliably.
        let mut header = Header::new_encrypted(&key());
        header.verify[CHECKSUM_STORED] ^= 0xFF;
        assert!(header.key_matches(&key()));

        header.verify[CHECKSUM_STORED - 1] ^= 0xFF;
        assert!(!header.key_matches(&key()));
    }

    #[test]
    fn rejects_a_corrupt_signature() {
        let mut raw: [u8; HEADER_SIZE] =
            Header::new_encrypted(&key()).to_bytes().try_into().unwrap();
        raw[0] = b'X';
        assert!(!Header::parse(&raw).has_valid_signature());
    }

    #[test]
    fn reports_an_unknown_version() {
        let mut raw: [u8; HEADER_SIZE] =
            Header::new_encrypted(&key()).to_bytes().try_into().unwrap();
        raw[OFF_VERSION..OFF_VERSION + 4].copy_from_slice(&0x0100_0003u32.to_le_bytes());

        let parsed = Header::parse(&raw);
        assert!(parsed.has_valid_signature());
        assert!(!parsed.is_supported_version());
        assert_eq!(parsed.version(), 0x0100_0003);
    }

    #[test]
    fn preserves_reserved_bytes() {
        let mut raw: [u8; HEADER_SIZE] =
            Header::new_encrypted(&key()).to_bytes().try_into().unwrap();
        raw[OFF_RESERVED] = 0xAB;
        raw[HEADER_SIZE - 1] = 0xCD;

        let round_tripped = Header::parse(&raw).to_bytes();
        assert_eq!(round_tripped[OFF_RESERVED], 0xAB);
        assert_eq!(round_tripped[HEADER_SIZE - 1], 0xCD);
    }
}
