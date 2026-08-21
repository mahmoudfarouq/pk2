//! Reader and patcher for Silkroad Online PK2 archives.
//!
//! A PK2 archive is a Blowfish-encrypted index of fixed-size entries laid over
//! a region of plain, uncompressed file payloads. The on-disk layout this crate
//! implements is documented in `docs/file-format.md`; the cipher and its key
//! derivation in `docs/encryption.md`.
//!
//! ```no_run
//! # fn main() -> Result<(), pk2::Error> {
//! let archive = pk2::Extractor::open("Media.pk2")?;
//!
//! for entry in archive.list("server_dep/silkroad/textdata")? {
//!     println!("{}", entry);
//! }
//!
//! let _bytes = archive.extract("server_dep/silkroad/textdata/weapon.txt")?;
//! # Ok(())
//! # }
//! ```

use std::cell::RefCell;
use std::collections::HashSet;
use std::error;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

mod blowfish;
mod header;

use crate::blowfish::BlowFish;
use crate::header::{HEADER_SIZE, Header};

// ---------------------------------------------------------------------------
// Format constants. See docs/file-format.md §9.
// ---------------------------------------------------------------------------

/// Offset of the first block of the root chain.
const ROOT_BLOCK_OFFSET: u64 = HEADER_SIZE as u64;
/// Every entry occupies exactly 128 bytes.
const ENTRY_SIZE: usize = 128;
/// Every block holds exactly 20 entries, no more and no fewer.
const BLOCK_ENTRY_COUNT: usize = 20;
/// 20 × 128 = 2560 bytes.
const BLOCK_SIZE: usize = ENTRY_SIZE * BLOCK_ENTRY_COUNT;
/// Width of an entry's name field, including its NUL terminator.
const NAME_BYTES: usize = 81;

// Field offsets within a 128-byte entry.
const OFF_NAME: usize = 0x01;
const OFF_ACCESS: usize = 0x52;
const OFF_CREATE: usize = 0x5A;
const OFF_MODIFY: usize = 0x62;
const OFF_POSITION: usize = 0x6A;
const OFF_SIZE: usize = 0x72;
const OFF_NEXT_BLOCK: usize = 0x76;
const OFF_PADDING: usize = 0x7E;

/// The key international Silkroad archives are packed with.
///
/// This is the ASCII key, not the derived one — see `docs/encryption.md` for
/// the salt-XOR that turns it into Blowfish key material.
pub const DEFAULT_KEY: &[u8] = b"169841";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Anything that can go wrong reading or modifying an archive.
#[derive(Debug)]
pub enum Error {
    /// The underlying file could not be read or written.
    Io(io::Error),
    /// An entry's type byte was outside `0..=2`.
    ///
    /// Almost always means the block did not decrypt correctly — typically a
    /// wrong key, or a chain pointer leading into a file payload.
    InvalidEntryKind(u8),
    /// A directory's block chain led back to a block already visited.
    ChainCycle(u64),
    /// No entry of that name exists.
    NotFound(String),
    /// A path component that must be a directory is not one.
    NotADirectory(String),
    /// A path that must name a file does not.
    NotAFile(String),
    /// A payload longer than `u32::MAX`, which the `size` field cannot express.
    FileTooLarge(usize),
    /// The file does not begin with the PK2 signature.
    NotAnArchive,
    /// The header records a version this crate does not understand.
    UnsupportedVersion(u32),
    /// The header's checksum does not match the key supplied.
    InvalidKey,
    /// A key Blowfish cannot accept: empty, or longer than 56 bytes.
    InvalidKeyLength(usize),
}

/// Shorthand for this crate's result type.
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Io(inner) => write!(f, "i/o error: {}", inner),
            Error::InvalidEntryKind(byte) => write!(
                f,
                "invalid entry type {:#04x}: the archive is corrupt or packed with a different key",
                byte
            ),
            Error::ChainCycle(offset) => {
                write!(f, "block chain loops back to offset {:#x}", offset)
            }
            Error::NotFound(name) => write!(f, "no such entry: {}", name),
            Error::NotADirectory(name) => write!(f, "not a directory: {}", name),
            Error::NotAFile(name) => write!(f, "not a file: {}", name),
            Error::FileTooLarge(len) => write!(
                f,
                "payload of {} bytes exceeds the 4 GiB maximum the size field can express",
                len
            ),
            Error::NotAnArchive => write!(f, "not a pk2 archive: signature does not match"),
            Error::UnsupportedVersion(version) => {
                write!(f, "unsupported archive version {:#010x}", version)
            }
            Error::InvalidKey => write!(f, "wrong key: the archive's checksum does not match"),
            Error::InvalidKeyLength(len) => {
                write!(f, "key of {} bytes; must be 1 to 56 bytes", len)
            }
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Error::Io(inner) => Some(inner),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(inner: io::Error) -> Self {
        Error::Io(inner)
    }
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

/// What an [`Entry`] describes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// An unused slot.
    ///
    /// Empty entries are not inert: the `next_block` field of an empty entry is
    /// still live, so a partially-filled block can chain onward.
    Empty,
    /// A directory. Its `position` is the head of its children's block chain.
    Directory,
    /// A file. Its `position` and `size` describe the payload.
    File,
}

impl EntryKind {
    fn from_byte(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(EntryKind::Empty),
            1 => Ok(EntryKind::Directory),
            2 => Ok(EntryKind::File),
            other => Err(Error::InvalidEntryKind(other)),
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            EntryKind::Empty => 0,
            EntryKind::Directory => 1,
            EntryKind::File => 2,
        }
    }
}

/// One 128-byte index entry.
#[derive(Clone, Copy)]
pub struct Entry {
    /// Where this entry lives in the archive. Derived, not stored on disk.
    offset: u64,
    kind: EntryKind,
    name: [u8; NAME_BYTES],
    access_date: u64,
    create_date: u64,
    modify_date: u64,
    position: u64,
    size: u32,
    next_block: u64,
    /// Preserved verbatim so rewriting an entry is byte-faithful.
    padding: [u8; 2],
}

impl Entry {
    /// Offset of this entry within the archive.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Whether this entry is a file, a directory, or an unused slot.
    pub fn kind(&self) -> EntryKind {
        self.kind
    }

    /// The entry's name, decoded lossily.
    ///
    /// Original Joymax archives store names in EUC-KR, which this does not yet
    /// decode; non-ASCII names come back with replacement characters.
    pub fn name(&self) -> String {
        String::from_utf8_lossy(self.raw_name()).into_owned()
    }

    /// For a file, the offset of its payload. For a directory, the head of its
    /// children's block chain.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Payload length in bytes. Meaningful for files only.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The next block in this entry's chain, or zero at the end of it.
    ///
    /// Only meaningful for the last entry of a block.
    pub fn next_block(&self) -> u64 {
        self.next_block
    }

    pub fn is_file(&self) -> bool {
        self.kind == EntryKind::File
    }

    pub fn is_directory(&self) -> bool {
        self.kind == EntryKind::Directory
    }

    /// The name field up to its first NUL, undecoded.
    fn raw_name(&self) -> &[u8] {
        let end = self
            .name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(NAME_BYTES);
        &self.name[..end]
    }

    /// Whether this is a `.` or `..` link rather than real content.
    fn is_self_or_parent(&self) -> bool {
        let name = self.raw_name();
        name == &b"."[..] || name == &b".."[..]
    }

    fn from_bytes(raw: &[u8], offset: u64) -> Result<Self> {
        debug_assert_eq!(raw.len(), ENTRY_SIZE, "entries are always 128 bytes");

        let mut name = [0u8; NAME_BYTES];
        name.copy_from_slice(&raw[OFF_NAME..OFF_NAME + NAME_BYTES]);

        Ok(Self {
            offset,
            kind: EntryKind::from_byte(raw[0])?,
            name,
            access_date: read_u64(raw, OFF_ACCESS),
            create_date: read_u64(raw, OFF_CREATE),
            modify_date: read_u64(raw, OFF_MODIFY),
            position: read_u64(raw, OFF_POSITION),
            size: read_u32(raw, OFF_SIZE),
            next_block: read_u64(raw, OFF_NEXT_BLOCK),
            padding: [raw[OFF_PADDING], raw[OFF_PADDING + 1]],
        })
    }

    fn to_bytes(self) -> Vec<u8> {
        let mut raw = vec![0u8; ENTRY_SIZE];
        raw[0] = self.kind.to_byte();
        raw[OFF_NAME..OFF_NAME + NAME_BYTES].copy_from_slice(&self.name);
        raw[OFF_ACCESS..OFF_ACCESS + 8].copy_from_slice(&self.access_date.to_le_bytes());
        raw[OFF_CREATE..OFF_CREATE + 8].copy_from_slice(&self.create_date.to_le_bytes());
        raw[OFF_MODIFY..OFF_MODIFY + 8].copy_from_slice(&self.modify_date.to_le_bytes());
        raw[OFF_POSITION..OFF_POSITION + 8].copy_from_slice(&self.position.to_le_bytes());
        raw[OFF_SIZE..OFF_SIZE + 4].copy_from_slice(&self.size.to_le_bytes());
        raw[OFF_NEXT_BLOCK..OFF_NEXT_BLOCK + 8].copy_from_slice(&self.next_block.to_le_bytes());
        raw[OFF_PADDING..OFF_PADDING + 2].copy_from_slice(&self.padding);
        raw
    }
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Entry")
            .field("kind", &self.kind)
            .field("name", &self.name())
            .field("offset", &self.offset)
            .field("position", &self.position)
            .field("size", &self.size)
            .field("next_block", &self.next_block)
            .finish()
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.kind {
            EntryKind::Directory => write!(f, "{}/", self.name()),
            EntryKind::File => write!(f, "{} ({} bytes)", self.name(), self.size),
            EntryKind::Empty => write!(f, "<empty>"),
        }
    }
}

// ---------------------------------------------------------------------------
// Archive
// ---------------------------------------------------------------------------

/// An open PK2 archive.
pub struct Extractor {
    path: PathBuf,
    /// One read handle for the archive's lifetime.
    ///
    /// Reads are a seek plus a `read_exact`, so nothing is buffered here and a
    /// separate write handle opened by [`Extractor::patch`] stays coherent
    /// with it. Opened read-only, so read-only archives can be listed.
    ///
    /// `RefCell` rather than a lock: seeking mutates the handle, and the read
    /// methods take `&self`. This makes `Extractor` `!Sync`.
    file: RefCell<File>,
    /// `None` when the header says the index is stored in the clear.
    cipher: Option<BlowFish>,
    root: Entry,
}

impl Extractor {
    /// Open an archive packed with the default international key.
    ///
    /// Equivalent to [`Extractor::open_with_key`] with [`DEFAULT_KEY`].
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_with_key(path, DEFAULT_KEY)
    }

    /// Open an archive packed with `ascii_key`.
    ///
    /// The key is the user-facing ASCII one, such as `b"169841"`; the salt-XOR
    /// derivation is applied internally.
    ///
    /// Validates the header before touching the index, so a file that is not
    /// an archive, is an unknown version, or was packed with a different key
    /// fails here with a clear error rather than decrypting to noise.
    pub fn open_with_key<P: AsRef<Path>>(path: P, ascii_key: &[u8]) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        let file = RefCell::new(File::open(&path)?);

        let raw: [u8; HEADER_SIZE] = read_at(&file, 0, HEADER_SIZE)?
            .try_into()
            .expect("read_at returns the requested length");
        let header = Header::parse(&raw);

        if !header.has_valid_signature() {
            return Err(Error::NotAnArchive);
        }
        if !header.is_supported_version() {
            return Err(Error::UnsupportedVersion(header.version()));
        }

        let cipher = if header.is_encrypted() {
            let blowfish = BlowFish::from_ascii_key(ascii_key)
                .ok_or(Error::InvalidKeyLength(ascii_key.len()))?;
            if !header.key_matches(&blowfish) {
                return Err(Error::InvalidKey);
            }
            Some(blowfish)
        } else {
            None
        };

        let root = read_entry_at(&file, cipher.as_ref(), ROOT_BLOCK_OFFSET)?;

        Ok(Self {
            path,
            file,
            cipher,
            root,
        })
    }

    /// The root directory entry.
    pub fn root(&self) -> &Entry {
        &self.root
    }

    /// Resolve a slash-separated path to its entry.
    ///
    /// `.` and an empty path both mean the root. `..` ascends, clamped at the
    /// root. Matching is case-insensitive, as the format's own tools are.
    pub fn entry(&self, path: &str) -> Result<Entry> {
        let mut stack = vec![self.root];

        for component in split_path(path) {
            if component == ".." {
                if stack.len() > 1 {
                    stack.pop();
                }
                continue;
            }
            let current = *stack.last().expect("stack always holds the root");
            stack.push(self.child_named(&current, component)?);
        }

        Ok(*stack.last().expect("stack always holds the root"))
    }

    /// List a directory's contents.
    ///
    /// `.` and `..` are omitted. Walks the directory's whole block chain, so
    /// directories larger than one block are returned in full.
    pub fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let entry = self.entry(path)?;
        if !entry.is_directory() {
            return Err(Error::NotADirectory(path.to_string()));
        }
        self.children(&entry)
    }

    /// Read a file's payload.
    pub fn extract(&self, path: &str) -> Result<Vec<u8>> {
        let entry = self.entry(path)?;
        if !entry.is_file() {
            return Err(Error::NotAFile(path.to_string()));
        }
        Ok(read_at(&self.file, entry.position, entry.size as usize)?)
    }

    /// Replace a file's contents.
    ///
    /// The new payload is appended at the end of the archive and the entry is
    /// repointed at it. The previous payload is left in place, unreachable —
    /// the format has no free list, so patching always grows the file. Use a
    /// repack to reclaim the space.
    pub fn patch(&self, path: &str, data: &[u8]) -> Result<()> {
        if data.len() > u32::MAX as usize {
            return Err(Error::FileTooLarge(data.len()));
        }

        let mut entry = self.entry(path)?;
        if !entry.is_file() {
            return Err(Error::NotAFile(path.to_string()));
        }

        entry.position = append(&self.path, data)?;
        entry.size = data.len() as u32;

        let mut encoded = entry.to_bytes();
        if let Some(cipher) = &self.cipher {
            cipher.encrypt(&mut encoded);
        }
        write_at(&self.path, entry.offset, &encoded)?;

        Ok(())
    }

    /// Every child of `entry`, walking its entire block chain.
    ///
    /// Returns an empty vector for anything that is not a directory. `.` and
    /// `..` are omitted.
    fn children(&self, entry: &Entry) -> Result<Vec<Entry>> {
        if !entry.is_directory() {
            return Ok(Vec::new());
        }

        let mut children = Vec::new();
        let mut visited = HashSet::new();
        let mut block_offset = entry.position;

        while block_offset != 0 {
            if !visited.insert(block_offset) {
                return Err(Error::ChainCycle(block_offset));
            }

            let block = self.read_block(block_offset)?;

            children.extend(
                block
                    .iter()
                    .filter(|entry| entry.kind != EntryKind::Empty && !entry.is_self_or_parent())
                    .copied(),
            );

            // Only the final entry of a block carries the chain pointer, and it
            // stays valid even when that entry is itself empty. Empty slots
            // earlier in the block are holes to skip, not end-of-directory.
            block_offset = block[BLOCK_ENTRY_COUNT - 1].next_block;
        }

        Ok(children)
    }

    fn child_named(&self, directory: &Entry, name: &str) -> Result<Entry> {
        if !directory.is_directory() {
            return Err(Error::NotADirectory(directory.name()));
        }

        self.children(directory)?
            .into_iter()
            .find(|child| child.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::NotFound(name.to_string()))
    }

    fn read_block(&self, offset: u64) -> Result<Vec<Entry>> {
        read_block_at(&self.file, self.cipher.as_ref(), offset)
    }
}

// ---------------------------------------------------------------------------
// Reading and writing
// ---------------------------------------------------------------------------

/// Read the 20 entries of the block at `offset`, decrypting if needed.
fn read_block_at(
    file: &RefCell<File>,
    cipher: Option<&BlowFish>,
    offset: u64,
) -> Result<Vec<Entry>> {
    let mut raw = read_at(file, offset, BLOCK_SIZE)?;
    if let Some(cipher) = cipher {
        cipher.decrypt(&mut raw);
    }

    let (entries, _remainder) = raw.as_chunks::<ENTRY_SIZE>();
    entries
        .iter()
        .enumerate()
        .map(|(index, chunk)| Entry::from_bytes(chunk, offset + (index * ENTRY_SIZE) as u64))
        .collect()
}

/// Read the single entry at `offset`, decrypting if needed.
fn read_entry_at(file: &RefCell<File>, cipher: Option<&BlowFish>, offset: u64) -> Result<Entry> {
    let mut raw = read_at(file, offset, ENTRY_SIZE)?;
    if let Some(cipher) = cipher {
        cipher.decrypt(&mut raw);
    }
    Entry::from_bytes(&raw, offset)
}

/// Read `len` bytes at `offset` through the archive's shared handle.
fn read_at(file: &RefCell<File>, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut file = file.borrow_mut();
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0u8; len];
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

fn write_at(path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)?;
    file.flush()
}

/// Append `data` and return the offset it was written at.
fn append(path: &Path, data: &[u8]) -> io::Result<u64> {
    let mut file = OpenOptions::new().append(true).open(path)?;
    let offset = file.seek(SeekFrom::End(0))?;
    file.write_all(data)?;
    file.flush()?;
    Ok(offset)
}

/// Split a path into meaningful components, dropping empties and `.`.
fn split_path(path: &str) -> impl Iterator<Item = &str> + '_ {
    path.split(['/', '\\'])
        .filter(|component| !component.is_empty() && *component != ".")
}

fn read_u64(raw: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(raw[at..at + 8].try_into().expect("8 bytes"))
}

fn read_u32(raw: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(raw[at..at + 4].try_into().expect("4 bytes"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Block offsets in the fixtures below.
    const BLOCK_0: u64 = ROOT_BLOCK_OFFSET;
    const BLOCK_1: u64 = BLOCK_0 + BLOCK_SIZE as u64;
    const BLOCK_2: u64 = BLOCK_1 + BLOCK_SIZE as u64;
    const DATA: u64 = BLOCK_2 + BLOCK_SIZE as u64;

    /// A temporary archive on disk that cleans up after itself.
    struct TempArchive {
        path: PathBuf,
    }

    impl TempArchive {
        fn new(name: &str, contents: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!("pk2-test-{}.pk2", name));
            std::fs::write(&path, contents).expect("write the fixture");
            TempArchive { path }
        }

        fn open(&self) -> Extractor {
            Extractor::open(&self.path).expect("open the fixture")
        }
    }

    impl Drop for TempArchive {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    // --- fixture builders --------------------------------------------------

    fn raw_entry(kind: u8, name: &str, position: u64, size: u32) -> Vec<u8> {
        let mut raw = vec![0u8; ENTRY_SIZE];
        raw[0] = kind;

        let name = name.as_bytes();
        assert!(name.len() < NAME_BYTES, "name must fit with a terminator");
        raw[OFF_NAME..OFF_NAME + name.len()].copy_from_slice(name);

        raw[OFF_POSITION..OFF_POSITION + 8].copy_from_slice(&position.to_le_bytes());
        raw[OFF_SIZE..OFF_SIZE + 4].copy_from_slice(&size.to_le_bytes());
        raw
    }

    fn dir(name: &str, children_at: u64) -> Vec<u8> {
        raw_entry(1, name, children_at, 0)
    }

    fn file(name: &str, at: u64, size: u32) -> Vec<u8> {
        raw_entry(2, name, at, size)
    }

    fn hole() -> Vec<u8> {
        vec![0u8; ENTRY_SIZE]
    }

    fn cipher() -> BlowFish {
        BlowFish::from_ascii_key(DEFAULT_KEY).expect("default key is valid")
    }

    /// A valid 256-byte header for an encrypted archive.
    fn valid_header() -> Vec<u8> {
        Header::new_encrypted(&cipher()).to_bytes()
    }

    /// Assemble up to 19 entries into one encrypted 2560-byte block.
    ///
    /// Entry 19 is deliberately left *empty* while still carrying `next_block`.
    /// That is legal per the format and is precisely the shape a walker that
    /// stops at the first empty entry gets wrong.
    fn block(entries: Vec<Vec<u8>>, next_block: u64) -> Vec<u8> {
        block_with(entries, next_block, Some(&cipher()))
    }

    fn block_with(
        mut entries: Vec<Vec<u8>>,
        next_block: u64,
        cipher: Option<&BlowFish>,
    ) -> Vec<u8> {
        assert!(
            entries.len() < BLOCK_ENTRY_COUNT,
            "entry 19 is reserved for the chain pointer"
        );
        while entries.len() < BLOCK_ENTRY_COUNT - 1 {
            entries.push(hole());
        }

        let mut chain_entry = hole();
        chain_entry[OFF_NEXT_BLOCK..OFF_NEXT_BLOCK + 8].copy_from_slice(&next_block.to_le_bytes());
        entries.push(chain_entry);

        let mut raw: Vec<u8> = entries.concat();
        assert_eq!(raw.len(), BLOCK_SIZE);
        if let Some(cipher) = cipher {
            cipher.encrypt(&mut raw);
        }
        raw
    }

    /// An archive whose root directory spans two chained blocks.
    ///
    /// ```text
    ///   root chain:  BLOCK_0 ──▶ BLOCK_1 ──▶ end
    ///   "sub" chain: BLOCK_2
    /// ```
    ///
    /// `BLOCK_1` holds a hole between real entries, and `BLOCK_0`'s chain
    /// pointer lives in an empty entry 19.
    fn chained_archive() -> Vec<u8> {
        let mut out = valid_header();

        out.extend(block(
            vec![
                dir(".", BLOCK_0),
                dir("..", BLOCK_0),
                file("alpha.txt", DATA, 5),
                file("beta.txt", DATA, 5),
            ],
            BLOCK_1,
        ));

        out.extend(block(
            vec![
                file("gamma.txt", DATA, 5),
                hole(), // a gap between real entries
                dir("sub", BLOCK_2),
                file("delta.txt", DATA, 5),
            ],
            0,
        ));

        out.extend(block(
            vec![
                dir(".", BLOCK_2),
                dir("..", BLOCK_0),
                file("inner.txt", DATA, 5),
            ],
            0,
        ));

        out.extend_from_slice(b"hello");
        out
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(Entry::name).collect()
    }

    fn sorted_names(entries: &[Entry]) -> Vec<String> {
        let mut names = names(entries);
        names.sort();
        names
    }

    // --- header and key ----------------------------------------------------

    #[test]
    fn rejects_a_file_that_is_not_an_archive() {
        let archive = TempArchive::new("notpk2", &vec![0u8; 4096]);
        assert!(matches!(
            Extractor::open(&archive.path),
            Err(Error::NotAnArchive)
        ));
    }

    #[test]
    fn reports_an_unsupported_version() {
        let mut raw = chained_archive();
        // Version sits at 0x1E, just past the 30-byte signature.
        raw[0x1E..0x22].copy_from_slice(&0x0100_0009u32.to_le_bytes());

        let archive = TempArchive::new("badversion", &raw);
        assert!(matches!(
            Extractor::open(&archive.path),
            Err(Error::UnsupportedVersion(0x0100_0009))
        ));
    }

    #[test]
    fn reports_a_wrong_key_instead_of_decoding_noise() {
        // Without the header check this would decrypt the root block to
        // garbage and surface as InvalidEntryKind, or worse, as a plausible
        // entry pointing at a bogus offset.
        let archive = TempArchive::new("wrongkey", &chained_archive());
        assert!(matches!(
            Extractor::open_with_key(&archive.path, b"000000"),
            Err(Error::InvalidKey)
        ));
    }

    #[test]
    fn accepts_the_correct_explicit_key() {
        let archive = TempArchive::new("rightkey", &chained_archive());
        assert!(Extractor::open_with_key(&archive.path, DEFAULT_KEY).is_ok());
    }

    #[test]
    fn rejects_a_key_blowfish_cannot_accept() {
        let archive = TempArchive::new("keylen", &chained_archive());
        assert!(matches!(
            Extractor::open_with_key(&archive.path, b""),
            Err(Error::InvalidKeyLength(0))
        ));
        assert!(matches!(
            Extractor::open_with_key(&archive.path, &[0u8; 57]),
            Err(Error::InvalidKeyLength(57))
        ));
    }

    #[test]
    fn reads_an_archive_with_a_plaintext_index() {
        // The header's encrypted flag is honoured; some tools emit these.
        let mut header = Header::new_encrypted(&cipher()).to_bytes();
        header[0x22] = 0; // encrypted = false

        let mut raw = header;
        raw.extend(block_with(
            vec![
                dir(".", BLOCK_0),
                dir("..", BLOCK_0),
                file("plain.txt", BLOCK_1, 5),
            ],
            0,
            None,
        ));
        raw.extend_from_slice(b"hello");

        let archive = TempArchive::new("plaintext", &raw);
        let opened = Extractor::open(&archive.path).expect("open");
        assert_eq!(names(&opened.list(".").expect("list")), vec!["plain.txt"]);
        assert_eq!(
            opened.extract("plain.txt").expect("extract"),
            b"hello".to_vec()
        );
    }

    // --- entry parsing -----------------------------------------------------

    #[test]
    fn entry_round_trips_through_bytes() {
        let raw = file("weapon.txt", 0xDEAD, 4096);
        let entry = Entry::from_bytes(&raw, 0x100).expect("parse");

        assert_eq!(entry.kind(), EntryKind::File);
        assert_eq!(entry.name(), "weapon.txt");
        assert_eq!(entry.position(), 0xDEAD);
        assert_eq!(entry.size(), 4096);
        assert_eq!(entry.offset(), 0x100);
        assert_eq!(entry.to_bytes(), raw, "re-encoding must be byte-faithful");
    }

    #[test]
    fn rejects_an_unknown_entry_kind() {
        let mut raw = hole();
        raw[0] = 9;
        assert!(matches!(
            Entry::from_bytes(&raw, 0),
            Err(Error::InvalidEntryKind(9))
        ));
    }

    #[test]
    fn name_stops_at_the_first_nul() {
        let mut raw = file("ok.txt", 0, 0);
        raw[OFF_NAME + 7] = b'X'; // garbage past the terminator
        assert_eq!(Entry::from_bytes(&raw, 0).expect("parse").name(), "ok.txt");
    }

    #[test]
    fn empty_entry_still_exposes_its_chain_pointer() {
        let mut raw = hole();
        raw[OFF_NEXT_BLOCK..OFF_NEXT_BLOCK + 8].copy_from_slice(&0x2A00u64.to_le_bytes());

        let entry = Entry::from_bytes(&raw, 0).expect("parse");
        assert_eq!(entry.kind(), EntryKind::Empty);
        assert_eq!(entry.next_block(), 0x2A00);
    }

    // --- directory walking -------------------------------------------------

    #[test]
    fn lists_every_child_across_a_block_chain() {
        let archive = TempArchive::new("chain", &chained_archive());
        assert_eq!(
            sorted_names(&archive.open().list(".").expect("list root")),
            vec!["alpha.txt", "beta.txt", "delta.txt", "gamma.txt", "sub"]
        );
    }

    #[test]
    fn follows_next_block_from_an_empty_final_entry() {
        // "gamma.txt" is in the second block, reachable only by following the
        // chain pointer held in BLOCK_0's empty entry 19.
        let archive = TempArchive::new("chainptr", &chained_archive());
        let listed = names(&archive.open().list(".").expect("list root"));
        assert!(listed.contains(&"gamma.txt".to_string()));
    }

    #[test]
    fn does_not_treat_a_hole_as_end_of_directory() {
        // "delta.txt" sits after an empty slot inside BLOCK_1.
        let archive = TempArchive::new("hole", &chained_archive());
        let listed = names(&archive.open().list(".").expect("list root"));
        assert!(listed.contains(&"delta.txt".to_string()));
    }

    #[test]
    fn omits_self_and_parent_links() {
        let archive = TempArchive::new("dots", &chained_archive());
        let listed = names(&archive.open().list(".").expect("list root"));
        assert!(
            !listed.iter().any(|name| name == "." || name == ".."),
            "got {:?}",
            listed
        );
    }

    #[test]
    fn descends_into_a_subdirectory() {
        let archive = TempArchive::new("descend", &chained_archive());
        assert_eq!(
            names(&archive.open().list("sub").expect("list sub")),
            vec!["inner.txt"]
        );
    }

    #[test]
    fn detects_a_chain_cycle() {
        let mut raw = valid_header();
        raw.extend(block(vec![dir(".", BLOCK_0), dir("..", BLOCK_0)], BLOCK_0));

        let archive = TempArchive::new("cycle", &raw);
        assert!(matches!(
            archive.open().list("."),
            Err(Error::ChainCycle(_))
        ));
    }

    // --- path resolution ---------------------------------------------------

    #[test]
    fn resolves_root_synonyms() {
        let archive = TempArchive::new("rootnames", &chained_archive());
        let archive = archive.open();

        let expected = sorted_names(&archive.list(".").expect("dot"));
        for path in &["", "/", "./", "."] {
            assert_eq!(sorted_names(&archive.list(path).expect(path)), expected);
        }
    }

    #[test]
    fn resolves_parent_components() {
        let archive = TempArchive::new("dotdot", &chained_archive());
        assert_eq!(
            names(&archive.open().list("sub/../sub").expect("list")),
            vec!["inner.txt"]
        );
    }

    #[test]
    fn parent_of_root_clamps_at_root() {
        let archive = TempArchive::new("clamp", &chained_archive());
        let archive = archive.open();
        assert_eq!(
            sorted_names(&archive.list("../../..").expect("list")),
            sorted_names(&archive.list(".").expect("list"))
        );
    }

    #[test]
    fn matching_is_case_insensitive() {
        let archive = TempArchive::new("case", &chained_archive());
        assert_eq!(
            names(&archive.open().list("SUB").expect("list")),
            vec!["inner.txt"]
        );
    }

    #[test]
    fn reports_a_missing_path() {
        let archive = TempArchive::new("missing", &chained_archive());
        assert!(matches!(
            archive.open().list("nope"),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn refuses_to_list_a_file() {
        let archive = TempArchive::new("notdir", &chained_archive());
        assert!(matches!(
            archive.open().list("alpha.txt"),
            Err(Error::NotADirectory(_))
        ));
    }

    #[test]
    fn refuses_to_descend_through_a_file() {
        let archive = TempArchive::new("through", &chained_archive());
        assert!(matches!(
            archive.open().list("alpha.txt/deeper"),
            Err(Error::NotADirectory(_))
        ));
    }

    // --- payloads ----------------------------------------------------------

    #[test]
    fn extracts_file_bytes() {
        let archive = TempArchive::new("extract", &chained_archive());
        assert_eq!(
            archive.open().extract("sub/inner.txt").expect("extract"),
            b"hello".to_vec()
        );
    }

    #[test]
    fn refuses_to_extract_a_directory() {
        let archive = TempArchive::new("extractdir", &chained_archive());
        assert!(matches!(
            archive.open().extract("sub"),
            Err(Error::NotAFile(_))
        ));
    }

    #[test]
    fn patch_appends_and_repoints_the_entry() {
        let archive = TempArchive::new("patch", &chained_archive());

        archive
            .open()
            .patch("sub/inner.txt", b"replacement")
            .expect("patch");

        // Re-open so nothing can be served from in-memory state.
        assert_eq!(
            archive.open().extract("sub/inner.txt").expect("extract"),
            b"replacement".to_vec()
        );
    }

    #[test]
    fn patch_is_visible_through_the_same_handle() {
        // patch writes through a separate handle while the read handle stays
        // open, so the two must not go out of step.
        let archive = TempArchive::new("coherent", &chained_archive());
        let opened = archive.open();

        opened.patch("sub/inner.txt", b"rewritten").expect("patch");

        assert_eq!(
            opened.extract("sub/inner.txt").expect("extract"),
            b"rewritten".to_vec(),
            "the open read handle must see the write"
        );
    }

    #[test]
    fn many_reads_reuse_one_handle() {
        // Walking a tree used to reopen the file for every 128-byte read.
        let archive = TempArchive::new("reuse", &chained_archive());
        let opened = archive.open();

        for _ in 0..64 {
            assert_eq!(opened.list(".").expect("list").len(), 5);
            assert_eq!(opened.list("sub").expect("list").len(), 1);
        }
    }

    #[test]
    fn patch_leaves_other_entries_alone() {
        let archive = TempArchive::new("patchother", &chained_archive());

        archive
            .open()
            .patch("sub/inner.txt", b"replacement")
            .expect("patch");

        let reopened = archive.open();
        assert_eq!(
            sorted_names(&reopened.list(".").expect("list root")),
            vec!["alpha.txt", "beta.txt", "delta.txt", "gamma.txt", "sub"]
        );
        assert_eq!(
            reopened.extract("alpha.txt").expect("extract"),
            b"hello".to_vec()
        );
    }

    #[test]
    fn refuses_to_patch_a_directory() {
        let archive = TempArchive::new("patchdir", &chained_archive());
        assert!(matches!(
            archive.open().patch("sub", b"nope"),
            Err(Error::NotAFile(_))
        ));
    }

    #[test]
    fn opening_a_missing_file_is_an_io_error() {
        let missing = std::env::temp_dir().join("pk2-test-does-not-exist.pk2");
        assert!(matches!(Extractor::open(missing), Err(Error::Io(_))));
    }
}
