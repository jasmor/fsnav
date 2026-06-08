//! Lightweight content classification: is a file plain, compressed, archived,
//! encrypted-looking, media, or executable?
//!
//! We use two cheap signals:
//!   1. Magic bytes — reliable identification of known container formats.
//!   2. Shannon entropy of a sampled prefix — high entropy (~>7.5 bits/byte)
//!      means the bytes are near-random, i.e. compressed OR encrypted.
//!
//! Honesty matters here: entropy cannot *prove* encryption. A high-entropy blob
//! with no recognizable header is reported as "high entropy (encrypted or
//! compressed)", not asserted to be encrypted.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// How a file's bytes classify.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Plain,
    Compressed,
    Archive,
    Encrypted,
    HighEntropy, // random-looking but unrecognized header
    Media,
    Executable,
    Empty,
    Unknown,
}

impl Category {
    /// Single-glyph badge for the 3D view / info card.
    pub fn badge(self) -> &'static str {
        match self {
            Category::Plain => "T",
            Category::Compressed => "Z",
            Category::Archive => "A",
            Category::Encrypted => "K",     // "key"
            Category::HighEntropy => "?",
            Category::Media => "M",
            Category::Executable => "X",
            Category::Empty => "-",
            Category::Unknown => "·",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Category::Plain => "plain / text",
            Category::Compressed => "compressed",
            Category::Archive => "archive",
            Category::Encrypted => "encrypted",
            Category::HighEntropy => "high entropy (encrypted or compressed)",
            Category::Media => "media",
            Category::Executable => "executable",
            Category::Empty => "empty",
            Category::Unknown => "unknown",
        }
    }
}

/// Full classification result attached to a node.
#[derive(Clone, Copy)]
pub struct Content {
    pub category: Category,
    pub entropy: f32, // bits per byte over the sampled prefix, 0..=8
}

/// Classify a file by reading a bounded prefix. Never reads the whole file.
pub fn classify(path: &Path, size: u64) -> Content {
    if size == 0 {
        return Content {
            category: Category::Empty,
            entropy: 0.0,
        };
    }

    let mut buf = [0u8; 4096];
    let n = match File::open(path).and_then(|mut f| f.read(&mut buf)) {
        Ok(n) => n,
        Err(_) => {
            return Content {
                category: Category::Unknown,
                entropy: 0.0,
            }
        }
    };
    let sample = &buf[..n];

    let entropy = shannon_entropy(sample);

    // 1) Recognizable magic wins outright.
    if let Some(cat) = magic(sample) {
        return Content {
            category: cat,
            entropy,
        };
    }

    // 2) Otherwise lean on entropy + a printable-text heuristic.
    let category = if looks_like_text(sample) {
        Category::Plain
    } else if entropy > 7.5 {
        // near-random and unrecognized: most likely encrypted or an unknown
        // compressed format. We report the cautious label.
        Category::HighEntropy
    } else if entropy > 6.5 {
        Category::Compressed
    } else {
        Category::Unknown
    };

    Content { category, entropy }
}

/// Identify common formats by leading bytes.
fn magic(b: &[u8]) -> Option<Category> {
    let starts = |sig: &[u8]| b.len() >= sig.len() && &b[..sig.len()] == sig;

    // compressed streams
    if starts(&[0x1f, 0x8b]) {
        return Some(Category::Compressed); // gzip
    }
    if starts(b"BZh") {
        return Some(Category::Compressed); // bzip2
    }
    if starts(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        return Some(Category::Compressed); // xz
    }
    if starts(&[0x28, 0xb5, 0x2f, 0xfd]) {
        return Some(Category::Compressed); // zstd
    }
    if starts(&[0x04, 0x22, 0x4d, 0x18]) {
        return Some(Category::Compressed); // lz4
    }

    // archives / containers
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        return Some(Category::Archive); // zip (also docx/jar/etc.)
    }
    if starts(b"Rar!\x1a\x07") {
        return Some(Category::Archive); // rar
    }
    if starts(&[0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]) {
        return Some(Category::Archive); // 7z
    }
    if b.len() >= 262 && &b[257..262] == b"ustar" {
        return Some(Category::Archive); // tar
    }

    // encrypted / key material
    if starts(b"-----BEGIN PGP MESSAGE") || starts(&[0x85]) || starts(&[0x8c]) {
        return Some(Category::Encrypted); // PGP
    }
    if starts(b"Salted__") {
        return Some(Category::Encrypted); // openssl enc
    }
    if starts(b"age-encryption.org/v1") {
        return Some(Category::Encrypted); // age
    }
    if starts(b"-----BEGIN ") && contains(b, b"PRIVATE KEY-----") {
        return Some(Category::Encrypted); // private key PEM
    }

    // executables
    if starts(&[0x7f, b'E', b'L', b'F']) {
        return Some(Category::Executable); // ELF
    }
    if starts(b"MZ") {
        return Some(Category::Executable); // PE/DOS
    }
    if starts(&[0xfe, 0xed, 0xfa, 0xce])
        || starts(&[0xfe, 0xed, 0xfa, 0xcf])
        || starts(&[0xcf, 0xfa, 0xed, 0xfe])
        || starts(&[0xca, 0xfe, 0xba, 0xbe])
    {
        return Some(Category::Executable); // Mach-O / fat binary
    }

    // media
    if starts(&[0xff, 0xd8, 0xff]) {
        return Some(Category::Media); // jpeg
    }
    if starts(&[0x89, b'P', b'N', b'G']) {
        return Some(Category::Media); // png
    }
    if starts(b"GIF8") {
        return Some(Category::Media); // gif
    }
    if starts(b"%PDF") {
        return Some(Category::Media); // pdf (treat as media/document)
    }
    if starts(b"ID3") || starts(&[0xff, 0xfb]) {
        return Some(Category::Media); // mp3
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        return Some(Category::Media); // mp4/mov
    }
    if starts(b"OggS") {
        return Some(Category::Media); // ogg
    }
    if starts(b"RIFF") {
        return Some(Category::Media); // wav/avi
    }

    None
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle)
}

/// Shannon entropy in bits per byte over the sample (0.0..=8.0).
fn shannon_entropy(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let len = data.len() as f32;
    let mut h = 0.0f32;
    for &c in counts.iter() {
        if c > 0 {
            let p = c as f32 / len;
            h -= p * p.log2();
        }
    }
    h
}

/// Heuristic: is this prefix mostly printable text (UTF-8-ish)?
fn looks_like_text(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    let mut printable = 0usize;
    for &b in data {
        if b == b'\t' || b == b'\n' || b == b'\r' || (0x20..=0x7e).contains(&b) {
            printable += 1;
        } else if b >= 0x80 {
            // allow high bytes (could be UTF-8); count as printable-ish
            printable += 1;
        }
    }
    (printable as f32 / data.len() as f32) > 0.85
}
