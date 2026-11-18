//! CRC-32C (Castagnoli), the checksum of Btrfs tree blocks and
//! superblocks with `csum_type` 0.

/// Reflected Castagnoli polynomial.
const POLY: u32 = 0x82F6_3B78;

const TABLE: [u32; 256] = table();

const fn table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut n = 0;
    while n < 256 {
        let mut crc = n as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ POLY
            };
            bit += 1;
        }
        table[n] = crc;
        n += 1;
    }
    table
}

/// CRC-32C of `data`, with the standard all-ones seed and final inversion.
pub fn crc32c(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |crc, &byte| {
        TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_standard_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn the_empty_input_checksums_to_zero() {
        assert_eq!(crc32c(b""), 0);
    }
}
