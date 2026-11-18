//! Byte encodings shared by report and cache files: LEB128 varints and
//! an FNV-1a 64 checksum (corruption detection, not authentication).

use std::io::{self, Write};

/// FNV-1a 64.
#[derive(Debug, Clone, Copy)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Fnv {
    pub fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

/// FNV-1a 64 of `bytes`.
pub fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = Fnv::new();
    hash.update(bytes);
    hash.value()
}

/// Counts and checksums everything written through it.
pub struct HashingWriter<W> {
    inner: W,
    hash: Fnv,
    written: u64,
}

impl<W: Write> HashingWriter<W> {
    pub fn new(inner: W) -> Self {
        HashingWriter {
            inner,
            hash: Fnv::new(),
            written: 0,
        }
    }

    pub fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.hash.update(bytes);
        self.written += bytes.len() as u64;
        self.inner.write_all(bytes)
    }

    pub fn varint(&mut self, v: u64) -> io::Result<()> {
        let mut buf = [0u8; 10];
        let n = encode_varint(v, &mut buf);
        self.put(&buf[..n])
    }

    /// Checksum of the bytes written so far.
    pub fn checksum(&self) -> u64 {
        self.hash.value()
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Writes `v` as LEB128 into `buf`; returns the length used.
pub fn encode_varint(mut v: u64, buf: &mut [u8; 10]) -> usize {
    let mut n = 0;
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            buf[n] = byte;
            return n + 1;
        }
        buf[n] = byte | 0x80;
        n += 1;
    }
}

/// Appends `v` as LEB128.
pub fn push_varint(out: &mut Vec<u8>, v: u64) {
    let mut buf = [0u8; 10];
    let n = encode_varint(v, &mut buf);
    out.extend_from_slice(&buf[..n]);
}

/// Reads bytes and varints from a slice; `None` on truncation.
#[derive(Debug, Clone)]
pub struct Decoder<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(bytes: &'a [u8], at: usize) -> Self {
        Decoder { bytes, at }
    }

    pub fn at(&self) -> usize {
        self.at
    }

    pub fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    /// Bytes not read yet.
    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }

    pub fn byte(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(b)
    }

    pub fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f).checked_shl(shift)?;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    pub fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip_at_their_boundaries() {
        let values = [
            0,
            1,
            127,
            128,
            16_383,
            16_384,
            u64::from(u32::MAX),
            u64::MAX,
        ];
        let mut out = HashingWriter::new(Vec::new());
        for v in values {
            out.varint(v).unwrap();
        }
        let bytes = out.inner.clone();
        assert_eq!(out.written(), bytes.len() as u64);
        assert_eq!(out.checksum(), checksum(&bytes));
        let mut d = Decoder::new(&bytes, 0);
        for v in values {
            assert_eq!(d.varint(), Some(v));
        }
        assert!(d.is_empty());
        assert_eq!(d.varint(), None);
    }

    #[test]
    fn overlong_varints_are_rejected() {
        assert_eq!(Decoder::new(&[0xff; 11], 0).varint(), None);
        assert_eq!(Decoder::new(&[0x80], 0).varint(), None);
    }

    #[test]
    fn fnv_matches_the_published_test_vector() {
        assert_eq!(checksum(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(checksum(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
