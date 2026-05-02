//! Flash storage helpers for chips with flash encryption enabled.
//!
//! Two implementations live here:
//!
//! - [`PlainFlashStorage`] — for app data (NVS, user data). Hardcodes capacity
//!   (working around `esp-storage`'s broken capacity detection on encrypted
//!   chips) and uses plain ROM SPI calls. Both reads and writes see the same
//!   plaintext bytes because neither path goes through encryption hardware.
//!   Bootloader/cache reads of these regions would see ciphertext-of-plaintext
//!   = garbage, but the bootloader never reads NVS / user data, so this is
//!   fine.
//!
//! - [`EncryptedOtaStorage`] — for the OTA partitions and otadata. Writes go
//!   through the chip's encryption hardware via `esp_rom_spiflash_write_encrypted`,
//!   so the bootloader's cache/MMU reads of the new app produce correct
//!   plaintext. Reads here use the plain ROM call (returning ciphertext); the
//!   OTA verify step is skipped because we can't get decrypted reads without
//!   ESP-IDF's mmap support.

use embedded_storage::{ReadStorage, Storage};
use esp_storage::FlashStorageError;

const SECTOR_SIZE: u32 = 4096;
const FLASH_CAPACITY: usize = 4 * 1024 * 1024;
const ENC_BLOCK: u32 = 32;

const ROM_READ: usize = 0x40000130;
const ROM_WRITE: usize = 0x4000012c;
const ROM_WRITE_ENCRYPTED: usize = 0x40000110;
const ROM_ERASE: usize = 0x40000128;
const ROM_UNLOCK: usize = 0x40000140;

#[inline(never)]
#[unsafe(link_section = ".rwtext")]
unsafe fn rom_read(src: u32, dst: *mut u32, len: u32) -> i32 {
    unsafe {
        let f: unsafe extern "C" fn(u32, *mut u32, u32) -> i32 = core::mem::transmute(ROM_READ);
        f(src, dst, len)
    }
}

#[inline(never)]
#[unsafe(link_section = ".rwtext")]
unsafe fn rom_write(addr: u32, data: *const u32, len: u32) -> i32 {
    unsafe {
        let f: unsafe extern "C" fn(u32, *const u32, u32) -> i32 = core::mem::transmute(ROM_WRITE);
        f(addr, data, len)
    }
}

#[inline(never)]
#[unsafe(link_section = ".rwtext")]
unsafe fn rom_write_encrypted(addr: u32, data: *const u32, len: u32) -> i32 {
    unsafe {
        let f: unsafe extern "C" fn(u32, *const u32, u32) -> i32 =
            core::mem::transmute(ROM_WRITE_ENCRYPTED);
        f(addr, data, len)
    }
}

#[inline(never)]
#[unsafe(link_section = ".rwtext")]
unsafe fn rom_erase_sector(sector: u32) -> i32 {
    unsafe {
        let f: unsafe extern "C" fn(u32) -> i32 = core::mem::transmute(ROM_ERASE);
        f(sector)
    }
}

#[inline(never)]
#[unsafe(link_section = ".rwtext")]
unsafe fn rom_unlock() -> i32 {
    unsafe {
        let f: unsafe extern "C" fn() -> i32 = core::mem::transmute(ROM_UNLOCK);
        f()
    }
}

fn check_bounds(offset: u32, length: usize) -> Result<(), FlashStorageError> {
    let offset = offset as usize;
    if length > FLASH_CAPACITY || offset > FLASH_CAPACITY - length {
        return Err(FlashStorageError::OutOfBounds);
    }
    Ok(())
}

fn plain_read(offset: u32, bytes: &mut [u8]) -> Result<(), FlashStorageError> {
    check_bounds(offset, bytes.len())?;

    let mut bytes = bytes;
    let mut current = offset;
    let mut buf = [0u32; 8]; // 32 bytes

    while !bytes.is_empty() {
        let to_read = bytes.len().min(32);
        let read_len = ((to_read + 3) & !3) as u32;
        let rc = unsafe { rom_read(current, buf.as_mut_ptr(), read_len) };
        if rc != 0 {
            return Err(FlashStorageError::Other(rc));
        }
        let src =
            unsafe { core::slice::from_raw_parts(buf.as_ptr() as *const u8, read_len as usize) };
        bytes[..to_read].copy_from_slice(&src[..to_read]);
        current += to_read as u32;
        bytes = &mut bytes[to_read..];
    }
    Ok(())
}

// ============================================================================
// PlainFlashStorage — for NVS and user data
// ============================================================================

pub struct PlainFlashStorage {
    unlocked: bool,
}

impl PlainFlashStorage {
    pub fn new() -> Self {
        Self { unlocked: false }
    }

    fn unlock_once(&mut self) -> Result<(), FlashStorageError> {
        if !self.unlocked {
            let rc = unsafe { rom_unlock() };
            if rc != 0 {
                return Err(FlashStorageError::CantUnlock);
            }
            self.unlocked = true;
        }
        Ok(())
    }
}

impl Default for PlainFlashStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadStorage for PlainFlashStorage {
    type Error = FlashStorageError;
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        plain_read(offset, bytes)
    }
    fn capacity(&self) -> usize {
        FLASH_CAPACITY
    }
}

impl Storage for PlainFlashStorage {
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_bounds(offset, bytes.len())?;
        self.unlock_once()?;

        let mut sector_buf = [0u8; SECTOR_SIZE as usize];
        let mut bytes = bytes;
        let mut current = offset;

        while !bytes.is_empty() {
            let sector_addr = current & !(SECTOR_SIZE - 1);
            let off_in_sector = (current - sector_addr) as usize;

            plain_read(sector_addr, &mut sector_buf)?;

            let to_write = (SECTOR_SIZE as usize - off_in_sector).min(bytes.len());
            sector_buf[off_in_sector..off_in_sector + to_write]
                .copy_from_slice(&bytes[..to_write]);

            let rc = unsafe { rom_erase_sector(sector_addr / SECTOR_SIZE) };
            if rc != 0 {
                return Err(FlashStorageError::Other(rc));
            }

            let rc =
                unsafe { rom_write(sector_addr, sector_buf.as_ptr() as *const u32, SECTOR_SIZE) };
            if rc != 0 {
                return Err(FlashStorageError::Other(rc));
            }

            current += to_write as u32;
            bytes = &bytes[to_write..];
        }
        Ok(())
    }
}

// ============================================================================
// EncryptedOtaStorage — for the OTA partitions and otadata
// ============================================================================

/// Single 32-byte block staged for an encrypted write.
///
/// The encrypt-on-write ROM helper requires writing a full 32-byte block of
/// plaintext at once. After it runs, flash holds ciphertext that we can no
/// longer "decrypt-modify-encrypt" via the plain SPI ROM path. So we accumulate
/// sub-block writes (e.g. esp-hal-ota's three 4-byte otadata fields) in
/// `data`, only emitting an encrypted write when we move on to a different
/// block or `flush()` is called. `data` defaults to `0xFF` to match a freshly-
/// erased block, which is what the destination must be for partial-block
/// updates to produce a correct result.
struct PendingBlock {
    addr: u32,
    data: [u8; ENC_BLOCK as usize],
}

pub struct EncryptedOtaStorage {
    unlocked: bool,
    pending: Option<PendingBlock>,
    /// Sector currently being written. When a new block targets a different
    /// sector, we erase that sector first. Required because `esp-hal-ota`
    /// doesn't erase the OTA partition itself — the default `FlashStorage::write`
    /// does sector-level read-modify-write internally, but our encryption-aware
    /// write doesn't, so we have to handle erase here. Encrypted writes to a
    /// non-erased sector silently fail to set 0→1 bits, producing wrong
    /// ciphertext that the cache MMU then "decrypts" into garbage.
    current_sector: Option<u32>,
}

impl EncryptedOtaStorage {
    pub fn new() -> Self {
        Self {
            unlocked: false,
            pending: None,
            current_sector: None,
        }
    }

    fn unlock_once(&mut self) -> Result<(), FlashStorageError> {
        if !self.unlocked {
            let rc = unsafe { rom_unlock() };
            if rc != 0 {
                log::error!("spi_flash_unlock failed: {}", rc);
                return Err(FlashStorageError::CantUnlock);
            }
            self.unlocked = true;
        }
        Ok(())
    }

    fn flush_pending(&mut self) -> Result<(), FlashStorageError> {
        if let Some(p) = self.pending.take() {
            // Erase the destination sector if we haven't already. Encrypted
            // writes need fresh 0xFF flash to produce correct ciphertext.
            let sector = p.addr / SECTOR_SIZE;
            if self.current_sector != Some(sector) {
                let rc = unsafe { rom_erase_sector(sector) };
                if rc != 0 {
                    log::error!("erase_sector({:#x}) failed: {}", sector, rc);
                    return Err(FlashStorageError::Other(rc));
                }
                self.current_sector = Some(sector);
            }

            // On ESP32-C3 the ROM `write_encrypted` handles the SPI controller
            // encryption setup internally; no enable/disable wrapper is needed
            // (and calling `_enable` from app context returns a bogus value).
            let rc = unsafe {
                rom_write_encrypted(p.addr, p.data.as_ptr() as *const u32, ENC_BLOCK)
            };
            if rc != 0 {
                log::error!("write_encrypted({:#x}) failed: {}", p.addr, rc);
                return Err(FlashStorageError::Other(rc));
            }
        }
        Ok(())
    }

    /// Commit any block currently buffered for write. Must be called after the
    /// OTA flow completes to flush the final partial block of firmware and the
    /// otadata block written in `ota_flush`.
    pub fn flush(&mut self) -> Result<(), FlashStorageError> {
        self.flush_pending()
    }

    /// Erase a contiguous region. Used to clear otadata before `ota_flush` so
    /// the destination block is `0xFF`, which is required for our buffered
    /// encrypted writes to produce correct ciphertext.
    pub fn erase_region(&mut self, offset: u32, length: u32) -> Result<(), FlashStorageError> {
        self.unlock_once()?;
        let start = offset / SECTOR_SIZE;
        let end = (offset + length).div_ceil(SECTOR_SIZE);
        for sector in start..end {
            let rc = unsafe { rom_erase_sector(sector) };
            if rc != 0 {
                return Err(FlashStorageError::Other(rc));
            }
        }
        Ok(())
    }
}

impl Default for EncryptedOtaStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadStorage for EncryptedOtaStorage {
    type Error = FlashStorageError;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        // Returns ciphertext — fine for the OTA flow because we skip the
        // post-write CRC verify; otadata reads will return ciphertext too
        // but the OTA library only uses these to compute a sequence number,
        // and CRC validation in `EspOtaSelectEntry::check_crc` zeroes the seq
        // when the CRC doesn't match. Both slots end up with seq=0, the
        // library increments to 1/2 to target the right partition, and we
        // erase the slot before writing so the new entry is well-formed.
        plain_read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        FLASH_CAPACITY
    }
}

impl Storage for EncryptedOtaStorage {
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_bounds(offset, bytes.len())?;
        self.unlock_once()?;

        let mut bytes = bytes;
        let mut current = offset;

        while !bytes.is_empty() {
            let block_addr = current & !(ENC_BLOCK - 1);
            let off_in_block = (current - block_addr) as usize;

            // If a different block is currently buffered, commit it before
            // moving on.
            if let Some(p) = &self.pending {
                if p.addr != block_addr {
                    self.flush_pending()?;
                }
            }

            // Start a fresh buffer for this block if we don't already have one.
            // Initialized to 0xFF — this is correct because our caller (the OTA
            // library, plus our pre-erase of otadata) ensures the destination
            // sector was erased before writing.
            if self.pending.is_none() {
                self.pending = Some(PendingBlock {
                    addr: block_addr,
                    data: [0xFF; ENC_BLOCK as usize],
                });
            }

            let to_write = (ENC_BLOCK as usize - off_in_block).min(bytes.len());
            let pending = self.pending.as_mut().unwrap();
            pending.data[off_in_block..off_in_block + to_write]
                .copy_from_slice(&bytes[..to_write]);

            current += to_write as u32;
            bytes = &bytes[to_write..];

            // If this completed the block, flush immediately so subsequent
            // writes start from a clean slate.
            if off_in_block + to_write == ENC_BLOCK as usize {
                self.flush_pending()?;
            }
        }
        Ok(())
    }
}
