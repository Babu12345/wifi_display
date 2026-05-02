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
const ROM_CACHE_INVALIDATE_ADDR: usize = 0x400004D4;
const ROM_CACHE_DBUS_MMU_SET: usize = 0x40000564;

// Cache MMU mapping. The flash encryption hardware on ESP32-C3 only decrypts
// when reads go through the cache (memory-mapped at 0x3C000000+ on DBus). The
// bootloader maps the running app's IROM/DROM but not arbitrary flash regions
// — we have to set up our own mappings to read otadata and the inactive OTA
// partition decrypted, which the OTA library needs for `is_pending_verification`
// and `ota_flush(verify=true)`.
const MMU_PAGE_SIZE: u32 = 0x10000; // 64 KB
const DBUS_VBASE: u32 = 0x3C000000;
// Entries 0x40-0x7E. App typically uses 0x00-0x14 (IROM) and 0x0F-0x14 (DROM),
// so 0x40 onwards is well clear.
const MMU_ENTRY_OTADATA: u32 = 0x40; // covers flash page 0x10000
const MMU_ENTRY_OTA0_BASE: u32 = 0x41; // covers flash 0x20000+ (23 pages)
const MMU_ENTRY_OTA1_BASE: u32 = 0x58; // covers flash 0x190000+ (23 pages)
const OTA_PARTITION_PAGES: u32 = 23;
// Hardcoded partition layout, must match `partitions_secure.csv` and
// `secure_partition_info()` in `ota_flash.rs`.
const OTA_0_BASE: u32 = 0x20000;
const OTA_1_BASE: u32 = 0x190000;
const OTA_PARTITION_SIZE: u32 = 0x170000;
const OTADATA_BASE: u32 = 0x18000;
const OTADATA_SIZE: u32 = 0x2000;

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

#[inline(never)]
#[unsafe(link_section = ".rwtext")]
unsafe fn rom_cache_invalidate_addr(vaddr: u32, size: u32) {
    unsafe {
        let f: unsafe extern "C" fn(u32, u32) =
            core::mem::transmute(ROM_CACHE_INVALIDATE_ADDR);
        f(vaddr, size);
    }
}

/// Map a contiguous flash region into the DBus virtual address space.
///
/// Uses the ROM helper rather than writing MMU entries directly. The ROM
/// function configures the entries with the correct bus-mode bits so the
/// cache controller actually performs flash decryption on subsequent reads;
/// raw register writes leave entries in a state where reads return raw
/// ciphertext.
///
/// Signature:
///   int Cache_Dbus_MMU_Set(uint32_t mode, uint32_t vaddr, uint32_t paddr,
///                          uint32_t psize_kb, uint32_t num, uint32_t fixed);
/// `mode = 0` means MMU_ACCESS_FLASH (the only mode used in ESP-IDF for
/// regular flash reads). `psize_kb = 64` is the page size. `fixed = 0` means
/// the entry can be replaced later.
fn map_flash_region(entry_id_base: u32, flash_addr: u32, num_pages: u32) {
    let vaddr = DBUS_VBASE + entry_id_base * MMU_PAGE_SIZE;
    unsafe {
        let f: unsafe extern "C" fn(u32, u32, u32, u32, u32, u32) -> i32 =
            core::mem::transmute(ROM_CACHE_DBUS_MMU_SET);
        let _ = f(0, vaddr, flash_addr, 64, num_pages, 0);
        rom_cache_invalidate_addr(vaddr, num_pages * MMU_PAGE_SIZE);
    }
}

/// Set up the MMU mappings for otadata, ota_0, and ota_1 in DBus virtual
/// address space. Idempotent — safe to call multiple times. After this, reads
/// at the corresponding `dbus_vaddr_for_*` addresses go through the cache and
/// are decrypted by the flash encryption hardware.
fn ensure_mappings() {
    // otadata page (covers flash 0x10000-0x1FFFF, includes partition table at
    // 0x10000 and otadata at 0x18000)
    map_flash_region(MMU_ENTRY_OTADATA, 0x10000, 1);
    // OTA partitions
    map_flash_region(MMU_ENTRY_OTA0_BASE, OTA_0_BASE, OTA_PARTITION_PAGES);
    map_flash_region(MMU_ENTRY_OTA1_BASE, OTA_1_BASE, OTA_PARTITION_PAGES);
}

/// Convert a flash physical offset into the DBus virtual address it's mapped
/// to (if it falls in a region we've mapped). Returns `None` for offsets
/// outside the OTA-related regions, in which case the caller falls back to
/// plain SPI reads (which return ciphertext but are fine for non-encrypted
/// regions or when the caller already deals in ciphertext).
fn dbus_vaddr_for_flash(flash_offset: u32) -> Option<u32> {
    if (OTADATA_BASE..OTADATA_BASE + OTADATA_SIZE).contains(&flash_offset) {
        // otadata page covers flash 0x10000+, so flash 0x18000 is at offset
        // 0x8000 within the mapped page.
        let page_offset = flash_offset - 0x10000;
        Some(DBUS_VBASE + MMU_ENTRY_OTADATA * MMU_PAGE_SIZE + page_offset)
    } else if (OTA_0_BASE..OTA_0_BASE + OTA_PARTITION_SIZE).contains(&flash_offset) {
        let off = flash_offset - OTA_0_BASE;
        Some(DBUS_VBASE + MMU_ENTRY_OTA0_BASE * MMU_PAGE_SIZE + off)
    } else if (OTA_1_BASE..OTA_1_BASE + OTA_PARTITION_SIZE).contains(&flash_offset) {
        let off = flash_offset - OTA_1_BASE;
        Some(DBUS_VBASE + MMU_ENTRY_OTA1_BASE * MMU_PAGE_SIZE + off)
    } else {
        None
    }
}

/// Read decrypted bytes via memory-mapped flash. The MMU mapping must already
/// be in place (call `ensure_mappings()` first). Cache is invalidated for the
/// read range to avoid returning data stale from before the most recent write.
fn mapped_read(flash_offset: u32, buf: &mut [u8]) {
    if let Some(vaddr) = dbus_vaddr_for_flash(flash_offset) {
        // Cache may hold stale data from before the most recent encrypted
        // write to this region — invalidate before reading. Round to a page
        // boundary because `Cache_Invalidate_Addr` operates per-page.
        let page_aligned = vaddr & !(MMU_PAGE_SIZE - 1);
        let span = ((vaddr + buf.len() as u32 + MMU_PAGE_SIZE - 1)
            & !(MMU_PAGE_SIZE - 1))
            - page_aligned;
        unsafe { rom_cache_invalidate_addr(page_aligned, span) };
        let src = unsafe { core::slice::from_raw_parts(vaddr as *const u8, buf.len()) };
        buf.copy_from_slice(src);
    } else {
        // Outside our mapped regions — fall back to plain SPI ROM read.
        let _ = plain_read(flash_offset, buf);
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
        // Set up cache MMU mappings for otadata + both OTA partitions on first
        // construction so reads through `mapped_read` return decrypted data.
        // `map_flash_region` is idempotent, so repeated calls are safe.
        ensure_mappings();
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

            // Invalidate cache for this block's mapped vaddr so a subsequent
            // verify-read sees the fresh ciphertext we just wrote.
            if let Some(vaddr) = dbus_vaddr_for_flash(p.addr) {
                let page_aligned = vaddr & !(MMU_PAGE_SIZE - 1);
                unsafe { rom_cache_invalidate_addr(page_aligned, MMU_PAGE_SIZE) };
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
        check_bounds(offset, bytes.len())?;
        // For otadata and OTA partition addresses, route through the cache
        // MMU so reads return decrypted plaintext. This is what makes
        // `is_pending_verification` (read otadata state) and
        // `ota_flush(verify=true)` (CRC of newly written partition) work.
        // Outside those ranges, fall back to plain SPI reads.
        if dbus_vaddr_for_flash(offset).is_some() {
            mapped_read(offset, bytes);
            Ok(())
        } else {
            plain_read(offset, bytes)
        }
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

            let to_write = (ENC_BLOCK as usize - off_in_block).min(bytes.len());

            // Start a fresh buffer for this block if we don't already have one.
            //
            // For a sub-block write (`to_write < ENC_BLOCK`) we have to preserve
            // the bytes we aren't touching — otherwise our flush_pending erase +
            // encrypt-write would zero them out. Read the existing decrypted
            // plaintext via the cache MMU mapping (if the address falls inside
            // one we've mapped); this is what makes `set_ota_state` (the
            // 4-byte mark-valid write) work without wiping the rest of the
            // otadata struct.
            //
            // For a full-block write we don't need to read anything — the
            // caller is overwriting all 32 bytes. Initialize to 0xFF as a
            // defensive default; flush_pending will erase the sector before
            // writing anyway.
            if self.pending.is_none() {
                let mut data = [0xFF; ENC_BLOCK as usize];
                let is_partial = off_in_block != 0 || to_write < ENC_BLOCK as usize;
                if is_partial && dbus_vaddr_for_flash(block_addr).is_some() {
                    mapped_read(block_addr, &mut data);
                }
                self.pending = Some(PendingBlock {
                    addr: block_addr,
                    data,
                });
            }

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
