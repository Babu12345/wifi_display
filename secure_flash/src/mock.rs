//! In-memory [`FlashHardware`] implementation for host tests.
//!
//! Simulates the asymmetry of real ESP32-family hardware:
//!
//!   - Plain reads return raw flash bytes (ciphertext on encrypted writes).
//!   - Decrypted reads return what `write_encrypted` was called with —
//!     i.e. the cache MMU's view after AES-XTS decryption.
//!   - Erase resets raw flash to `0xFF` and the decrypted view to a
//!     deterministic non-`0xFF` pattern (mimics real hardware decrypting
//!     erased flash to non-trivial bytes).
//!   - `write_encrypted` requires the destination to be erased first;
//!     writing to non-erased flash mangles the result so tests catch the
//!     "forgot to erase" bug.
//!
//! The "encryption" is a simple byte-level XOR with `0xA5` plus the address
//! low byte — enough to make ciphertext distinguishable from plaintext, while
//! staying fast and obvious in test output.

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use crate::{FlashError, FlashHardware};

/// XOR mask used to fake encryption in the mock. Address-dependent so the
/// "ciphertext" varies across the chip the way real XTS does.
fn fake_cipher(addr: usize, byte: u8) -> u8 {
    byte ^ 0xA5 ^ (addr as u8)
}

/// Pattern stored in the "decrypted view" for erased (raw=0xFF) bytes —
/// mimics the deterministic non-`0xFF` plaintext you'd see if you read
/// erased flash through a cache-MMU mapping on a flash-encrypted chip.
fn erased_decrypted(addr: usize) -> u8 {
    !(addr as u8) ^ 0x55
}

#[derive(Clone, Copy)]
struct Mapping {
    offset: u32,
    size: u32,
}

pub struct MockFlash {
    capacity: usize,
    sector_size: u32,
    enc_block_size: u32,
    /// Raw flash content (what plain reads see).
    raw: Vec<u8>,
    /// Decrypted view (what reads through the cache MMU see).
    decrypted: Vec<u8>,
    /// Whether the most recent write to each byte went through encrypt-on-
    /// write. Used by `write_encrypted` to detect the "destination wasn't
    /// erased" bug.
    last_write_was_encrypted: Vec<bool>,
    mappings: Vec<Mapping>,
    locked: bool,
}

impl MockFlash {
    pub fn new(capacity: usize) -> Self {
        let mut decrypted = vec![0u8; capacity];
        for (i, b) in decrypted.iter_mut().enumerate() {
            *b = erased_decrypted(i);
        }
        Self {
            capacity,
            sector_size: 4096,
            enc_block_size: 32,
            raw: vec![0xFFu8; capacity],
            decrypted,
            last_write_was_encrypted: vec![false; capacity],
            mappings: Vec::new(),
            locked: true,
        }
    }

    /// Override the sector size (default 4096). Mainly for stress-testing
    /// the storage logic against unusual hardware geometry.
    pub fn with_sector_size(mut self, sector_size: u32) -> Self {
        self.sector_size = sector_size;
        self
    }

    /// Override the encryption block size (default 32). Must be ≤
    /// [`crate::encrypted`]'s `MAX_ENC_BLOCK_SIZE`.
    pub fn with_enc_block_size(mut self, block: u32) -> Self {
        self.enc_block_size = block;
        self
    }
}

impl Default for MockFlash {
    fn default() -> Self {
        Self::new(4 * 1024 * 1024)
    }
}

impl FlashHardware for MockFlash {
    fn capacity(&self) -> usize {
        self.capacity
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn enc_block_size(&self) -> u32 {
        self.enc_block_size
    }

    fn read_plain(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), FlashError> {
        let off = offset as usize;
        if off + buf.len() > self.capacity {
            return Err(FlashError::OutOfBounds);
        }
        buf.copy_from_slice(&self.raw[off..off + buf.len()]);
        Ok(())
    }

    fn write_plain(&mut self, offset: u32, data: &[u8]) -> Result<(), FlashError> {
        if self.locked {
            return Err(FlashError::Locked);
        }
        let off = offset as usize;
        if off + data.len() > self.capacity {
            return Err(FlashError::OutOfBounds);
        }
        for (i, &b) in data.iter().enumerate() {
            let addr = off + i;
            self.raw[addr] = b;
            // Cache MMU "decrypts" plaintext-on-flash into garbage.
            self.decrypted[addr] = fake_cipher(addr, b);
            self.last_write_was_encrypted[addr] = false;
        }
        Ok(())
    }

    fn write_encrypted(&mut self, offset: u32, data: &[u8]) -> Result<(), FlashError> {
        if self.locked {
            return Err(FlashError::Locked);
        }
        let off = offset as usize;
        if off as u32 % self.enc_block_size != 0 {
            return Err(FlashError::NotAligned);
        }
        if data.len() as u32 % self.enc_block_size != 0 {
            return Err(FlashError::NotAligned);
        }
        if off + data.len() > self.capacity {
            return Err(FlashError::OutOfBounds);
        }
        for (i, &p) in data.iter().enumerate() {
            let addr = off + i;
            // Encrypt-write requires erased (raw=0xFF) destination. If raw
            // already has 0 bits, the resulting ciphertext is wrong because
            // flash bits can't go 0→1 without erase.
            let cipher = fake_cipher(addr, p);
            if self.raw[addr] == 0xFF {
                self.raw[addr] = cipher;
                self.decrypted[addr] = p;
            } else {
                // Approximate the real-hardware failure mode: bits stick at
                // their old values where they were 0. The decrypted view
                // becomes garbage relative to `p`.
                self.raw[addr] &= cipher;
                self.decrypted[addr] = fake_cipher(addr, self.raw[addr]);
            }
            self.last_write_was_encrypted[addr] = true;
        }
        Ok(())
    }

    fn erase_sector(&mut self, sector_index: u32) -> Result<(), FlashError> {
        if self.locked {
            return Err(FlashError::Locked);
        }
        let start = (sector_index * self.sector_size) as usize;
        let end = start + self.sector_size as usize;
        if end > self.capacity {
            return Err(FlashError::OutOfBounds);
        }
        for addr in start..end {
            self.raw[addr] = 0xFF;
            self.decrypted[addr] = erased_decrypted(addr);
            self.last_write_was_encrypted[addr] = false;
        }
        Ok(())
    }

    fn unlock(&mut self) -> Result<(), FlashError> {
        self.locked = false;
        Ok(())
    }

    fn map_decrypt_region(
        &mut self,
        flash_offset: u32,
        size: u32,
    ) -> Result<(), FlashError> {
        if (flash_offset as usize) + (size as usize) > self.capacity {
            return Err(FlashError::OutOfBounds);
        }
        // Idempotent: don't double-register exact duplicates.
        if !self
            .mappings
            .iter()
            .any(|m| m.offset == flash_offset && m.size == size)
        {
            self.mappings.push(Mapping {
                offset: flash_offset,
                size,
            });
        }
        Ok(())
    }

    fn is_decrypt_mapped(&self, flash_offset: u32) -> bool {
        self.mappings
            .iter()
            .any(|m| flash_offset >= m.offset && flash_offset < m.offset + m.size)
    }

    fn read_decrypted(
        &mut self,
        flash_offset: u32,
        buf: &mut [u8],
    ) -> Result<(), FlashError> {
        if !self.is_decrypt_mapped(flash_offset) {
            return Err(FlashError::OutOfBounds);
        }
        let off = flash_offset as usize;
        if off + buf.len() > self.capacity {
            return Err(FlashError::OutOfBounds);
        }
        buf.copy_from_slice(&self.decrypted[off..off + buf.len()]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_chip_reads_0xff_plain_and_pattern_decrypted() {
        let mut hw = MockFlash::new(0x1000);
        hw.map_decrypt_region(0, 0x1000).unwrap();

        let mut plain = [0u8; 4];
        hw.read_plain(0, &mut plain).unwrap();
        assert_eq!(plain, [0xFF, 0xFF, 0xFF, 0xFF]);

        let mut decr = [0u8; 4];
        hw.read_decrypted(0, &mut decr).unwrap();
        // Not 0xFF — matches what real hardware shows for erased encrypted flash.
        assert_ne!(decr, [0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn encrypted_write_round_trips_after_erase() {
        let mut hw = MockFlash::new(0x10000);
        hw.unlock().unwrap();
        hw.map_decrypt_region(0, 0x10000).unwrap();
        hw.erase_sector(0).unwrap();

        let plaintext = [0x42u8; 32];
        hw.write_encrypted(0, &plaintext).unwrap();

        let mut decr = [0u8; 32];
        hw.read_decrypted(0, &mut decr).unwrap();
        assert_eq!(decr, plaintext);

        let mut raw = [0u8; 32];
        hw.read_plain(0, &mut raw).unwrap();
        assert_ne!(raw, plaintext, "raw must hold ciphertext, not plaintext");
    }

    #[test]
    fn encrypted_write_without_erase_corrupts() {
        let mut hw = MockFlash::new(0x10000);
        hw.unlock().unwrap();
        hw.map_decrypt_region(0, 0x10000).unwrap();
        hw.erase_sector(0).unwrap();

        // First encrypted write succeeds (sector was just erased).
        let first = [0x11u8; 32];
        hw.write_encrypted(0, &first).unwrap();

        // Second encrypted write to the SAME address without erase — the
        // mock simulates the real-hardware failure: bits stick where they
        // were 0, decrypted view drifts away from `second`.
        let second = [0x22u8; 32];
        hw.write_encrypted(0, &second).unwrap();

        let mut decr = [0u8; 32];
        hw.read_decrypted(0, &mut decr).unwrap();
        assert_ne!(
            decr, second,
            "writing without erase must NOT round-trip — that's why erase tracking exists"
        );
    }

    #[test]
    fn locked_flash_rejects_writes() {
        let mut hw = MockFlash::new(0x1000);
        // Did NOT call unlock.
        let res = hw.write_plain(0, &[0u8; 4]);
        assert_eq!(res, Err(FlashError::Locked));
    }

    #[test]
    fn unaligned_encrypted_write_errors() {
        let mut hw = MockFlash::new(0x1000);
        hw.unlock().unwrap();
        let res = hw.write_encrypted(1, &[0u8; 32]);
        assert_eq!(res, Err(FlashError::NotAligned));
        let res = hw.write_encrypted(0, &[0u8; 30]);
        assert_eq!(res, Err(FlashError::NotAligned));
    }

    #[test]
    fn read_decrypted_fails_outside_mapped_region() {
        let mut hw = MockFlash::new(0x10000);
        hw.map_decrypt_region(0x1000, 0x1000).unwrap();
        let mut buf = [0u8; 4];
        // Address inside the mapping — fine.
        hw.read_decrypted(0x1000, &mut buf).unwrap();
        // Address outside — error.
        let res = hw.read_decrypted(0x500, &mut buf);
        assert_eq!(res, Err(FlashError::OutOfBounds));
    }

    #[test]
    fn map_region_is_idempotent() {
        let mut hw = MockFlash::new(0x10000);
        hw.map_decrypt_region(0, 0x1000).unwrap();
        hw.map_decrypt_region(0, 0x1000).unwrap();
        hw.map_decrypt_region(0, 0x1000).unwrap();
        assert_eq!(hw.mappings.len(), 1);
    }
}
