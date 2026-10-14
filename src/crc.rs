//! CRC-32/BZIP2: polynomial 0x04C11DB7, MSB-first (not reflected), init and
//! final XOR 0xFFFFFFFF, applied by the caller as libbz2 does.

/// Feeds `data` into a running CRC (without the init/final XOR).
pub(crate) fn update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc = (crc << 8) ^ TABLE[usize::from((crc >> 24) as u8 ^ b)];
    }
    crc
}

static TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 { (c << 1) ^ 0x04C1_1DB7 } else { c << 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

#[cfg(test)]
mod tests {
    #[test]
    fn check_value() {
        // CRC-32/BZIP2 check value from the CRC catalogue.
        assert_eq!(!super::update(!0, b"123456789"), 0xFC89_1918);
    }
}
