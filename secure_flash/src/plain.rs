//! [`PlainFlashStorage`] — simple plain-SPI storage for partitions where the
//! bootloader never reads (NVS, user data). Hardcodes the capacity from the
//! hardware backend, which sidesteps `esp-storage`'s broken capacity probe on
//! flash-encryption-enabled chips.

use embedded_storage::{ReadStorage, Storage};

use crate::{check_bounds, FlashError, FlashHardware};

/// Maximum sector size we allocate a stack buffer for. ESP32 family is 4 KB.
const MAX_SECTOR_SIZE: usize = 4096;

/// Plain-SPI flash storage.
///
/// Reads return whatever the underlying backend's plain SPI read produces
/// (ciphertext on encrypted chips, plaintext on others). Writes do plain
/// sector-aligned read-modify-write — i.e. for each sector touched, read it
/// out, splice in the new bytes, erase, and write back.
///
/// On a flash-encryption-enabled chip this means flash content for these
/// partitions is **not** encrypted by the hardware: both writes and reads use
/// the same plain SPI path so the bytes are consistent.
pub struct PlainFlashStorage<H: FlashHardware> {
    hw: H,
    unlocked: bool,
}

impl<H: FlashHardware> PlainFlashStorage<H> {
    /// Construct from an explicit hardware instance.
    pub fn new(hw: H) -> Self {
        Self { hw, unlocked: false }
    }

    /// Borrow the underlying hardware mutably (mainly useful for tests and
    /// for backends that need post-construction tweaking).
    pub fn hw_mut(&mut self) -> &mut H {
        &mut self.hw
    }

    fn unlock_once(&mut self) -> Result<(), FlashError> {
        if !self.unlocked {
            self.hw.unlock()?;
            self.unlocked = true;
        }
        Ok(())
    }
}

impl<H: FlashHardware + Default> Default for PlainFlashStorage<H> {
    fn default() -> Self {
        Self::new(H::default())
    }
}

impl<H: FlashHardware> ReadStorage for PlainFlashStorage<H> {
    type Error = FlashError;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_bounds(offset, bytes.len(), self.hw.capacity())?;
        self.hw.read_plain(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.hw.capacity()
    }
}

impl<H: FlashHardware> Storage for PlainFlashStorage<H> {
    fn write(&mut self, offset: u32, mut bytes: &[u8]) -> Result<(), Self::Error> {
        check_bounds(offset, bytes.len(), self.hw.capacity())?;
        self.unlock_once()?;

        let sector_size = self.hw.sector_size();
        debug_assert!(sector_size as usize <= MAX_SECTOR_SIZE);
        let mut sector_buf = [0u8; MAX_SECTOR_SIZE];
        let buf = &mut sector_buf[..sector_size as usize];

        let mut current = offset;
        while !bytes.is_empty() {
            let sector_addr = current & !(sector_size - 1);
            let off_in_sector = (current - sector_addr) as usize;

            self.hw.read_plain(sector_addr, buf)?;

            let to_write = (sector_size as usize - off_in_sector).min(bytes.len());
            buf[off_in_sector..off_in_sector + to_write]
                .copy_from_slice(&bytes[..to_write]);

            self.hw.erase_sector(sector_addr / sector_size)?;
            self.hw.write_plain(sector_addr, buf)?;

            current += to_write as u32;
            bytes = &bytes[to_write..];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockFlash;
    use embedded_storage::{ReadStorage, Storage};

    fn make_storage() -> PlainFlashStorage<MockFlash> {
        PlainFlashStorage::new(MockFlash::new(0x100000)) // 1 MiB
    }

    #[test]
    fn read_back_what_we_wrote() {
        let mut s = make_storage();
        let payload = b"hello world this is plaintext";
        s.write(0x10000, payload).unwrap();

        let mut got = [0u8; 29];
        s.read(0x10000, &mut got).unwrap();
        assert_eq!(&got, payload);
    }

    #[test]
    fn write_preserves_other_bytes_in_sector() {
        let mut s = make_storage();
        // Write a marker at offset 0, then write something else mid-sector.
        s.write(0x0, b"START").unwrap();
        s.write(0x800, b"MIDDLE").unwrap();

        let mut got = [0u8; 5];
        s.read(0x0, &mut got).unwrap();
        assert_eq!(&got, b"START");

        let mut got = [0u8; 6];
        s.read(0x800, &mut got).unwrap();
        assert_eq!(&got, b"MIDDLE");
    }

    #[test]
    fn write_spanning_sector_boundary() {
        let mut s = make_storage();
        // 4 bytes before sector boundary, 4 after — the write must update
        // both sectors via two read-modify-write cycles.
        let bytes = b"ABCDEFGH";
        s.write(0x1000 - 4, bytes).unwrap();

        let mut got = [0u8; 8];
        s.read(0x1000 - 4, &mut got).unwrap();
        assert_eq!(&got, bytes);
    }

    #[test]
    fn read_past_capacity_errors() {
        let mut s = make_storage();
        let mut buf = [0u8; 16];
        // 0xFFFF8 + 16 = 0x100008, beyond the 1 MiB capacity.
        let res = s.read(0xFFFF8, &mut buf);
        assert_eq!(res, Err(FlashError::OutOfBounds));
    }

    #[test]
    fn read_at_exact_capacity_boundary_succeeds() {
        // 0xFFFF0 + 16 = 0x100000 = exactly capacity — fits.
        let mut s = make_storage();
        let mut buf = [0u8; 16];
        s.read(0xFFFF0, &mut buf).unwrap();
    }

    #[test]
    fn capacity_is_what_hw_reports() {
        let s = make_storage();
        assert_eq!(s.capacity(), 0x100000);
    }
}
