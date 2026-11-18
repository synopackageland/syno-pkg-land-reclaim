//! Bounds-checked little-endian decoding of on-disk/ioctl structures.

/// A read would run past the end of the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truncated {
    pub needed: usize,
    pub available: usize,
}

/// Sequential little-endian reader that never panics on short input.
#[derive(Debug, Clone)]
pub struct LeReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> LeReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        LeReader { buf, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn take(&mut self, len: usize) -> Result<&'a [u8], Truncated> {
        let available = self.remaining();
        if len > available {
            return Err(Truncated {
                needed: len,
                available,
            });
        }
        let bytes = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Ok(bytes)
    }

    pub fn skip(&mut self, len: usize) -> Result<(), Truncated> {
        self.take(len).map(|_| ())
    }

    pub fn u8(&mut self) -> Result<u8, Truncated> {
        self.array().map(u8::from_le_bytes)
    }

    pub fn u16(&mut self) -> Result<u16, Truncated> {
        self.array().map(u16::from_le_bytes)
    }

    pub fn u32(&mut self) -> Result<u32, Truncated> {
        self.array().map(u32::from_le_bytes)
    }

    pub fn u64(&mut self) -> Result<u64, Truncated> {
        self.array().map(u64::from_le_bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Truncated> {
        let bytes = self.take(N)?;
        let mut out = [0; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_fields_in_order() {
        let buf = [1, 2, 0, 3, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 9];
        let mut r = LeReader::new(&buf);
        assert_eq!(r.u8(), Ok(1));
        assert_eq!(r.u16(), Ok(2));
        assert_eq!(r.u32(), Ok(3));
        assert_eq!(r.u64(), Ok(4));
        assert_eq!(r.take(1), Ok(&[9][..]));
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn reading_past_end_is_an_error_not_a_panic() {
        let mut r = LeReader::new(&[1, 2, 3]);
        assert_eq!(
            r.u32(),
            Err(Truncated {
                needed: 4,
                available: 3
            })
        );
    }

    #[test]
    fn take_rejects_lengths_beyond_buffer() {
        let mut r = LeReader::new(&[1, 2, 3]);
        assert_eq!(
            r.take(usize::MAX),
            Err(Truncated {
                needed: usize::MAX,
                available: 3
            })
        );
    }

    #[test]
    fn skip_advances_position() {
        let mut r = LeReader::new(&[1, 2, 3]);
        r.skip(2).unwrap();
        assert_eq!(r.position(), 2);
        assert_eq!(r.u8(), Ok(3));
    }
}
