//! ESP32-C3 backend for [`FlashHardware`].
//!
//! All ROM addresses are taken from
//! `components/esp_rom/esp32c3/ld/esp32c3.rom.ld` in ESP-IDF v5.3. The cache
//! MMU mapping for decrypted reads uses `Cache_Dbus_MMU_Set` (which
//! configures the right bus-mode bits — direct register writes leave entries
//! in a state where reads return raw ciphertext).
//!
//! Designed to be constructed once per use (e.g. once per
//! `EncryptedOtaStorage::new` call). Multiple instances share global
//! hardware state (the MMU table, the lock register), which is fine because
//! `map_decrypt_region` is idempotent and `unlock` is harmless if already
//! unlocked.

use crate::{FlashError, FlashHardware};

// === ROM function addresses ===
const ROM_READ: usize = 0x40000130;
const ROM_WRITE: usize = 0x4000012c;
const ROM_WRITE_ENCRYPTED: usize = 0x40000110;
const ROM_ERASE_SECTOR: usize = 0x40000128;
const ROM_UNLOCK: usize = 0x40000140;
const ROM_CACHE_INVALIDATE_ADDR: usize = 0x400004D4;
const ROM_CACHE_DBUS_MMU_SET: usize = 0x40000564;

// === Memory map / hardware geometry ===
const SECTOR_SIZE: u32 = 4096;
const ENC_BLOCK_SIZE: u32 = 32; // XTS-AES-128 on ESP32-C3
const MMU_PAGE_SIZE: u32 = 0x10000; // 64 KiB
const DBUS_VBASE: u32 = 0x3C000000;

/// Default first MMU entry index. The bootloader and app together use entries
/// `0x00..=0x14` or so; `0x40` is well clear of those without consuming the
/// entire upper half of the MMU table.
pub const DEFAULT_MMU_ENTRY_BASE: u32 = 0x40;

/// Maximum number of mapped regions tracked per backend instance.
const MAX_MAPPINGS: usize = 4;

#[derive(Clone, Copy, Debug)]
struct Mapping {
    flash_offset: u32,
    size: u32,
    /// Virtual address that this region starts at — derived from the MMU
    /// entry base assigned at `map_decrypt_region` time.
    vaddr_base: u32,
}

/// ESP32-C3 hardware backend.
pub struct Esp32C3 {
    capacity: usize,
    /// MMU entry index to use for the *next* call to `map_decrypt_region`.
    /// Each region consumes `ceil(size / 64KiB)` consecutive entries starting
    /// here, then this advances.
    next_mmu_entry: u32,
    mappings: [Option<Mapping>; MAX_MAPPINGS],
    mapping_count: usize,
}

impl Esp32C3 {
    /// Construct with a known flash capacity (bytes). Use [`Self::default`]
    /// for the common 4 MB case.
    pub const fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            next_mmu_entry: DEFAULT_MMU_ENTRY_BASE,
            mappings: [None; MAX_MAPPINGS],
            mapping_count: 0,
        }
    }

    /// Override the starting MMU entry. Useful if the default range collides
    /// with another consumer of the MMU table in the same firmware.
    pub const fn with_mmu_entry_base(mut self, base: u32) -> Self {
        self.next_mmu_entry = base;
        self
    }
}

impl Default for Esp32C3 {
    fn default() -> Self {
        Self::with_capacity(4 * 1024 * 1024)
    }
}

// === Thin ROM wrappers ===

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
        let f: unsafe extern "C" fn(u32) -> i32 = core::mem::transmute(ROM_ERASE_SECTOR);
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

#[inline(never)]
#[unsafe(link_section = ".rwtext")]
unsafe fn rom_cache_dbus_mmu_set(
    mode: u32,
    vaddr: u32,
    paddr: u32,
    psize_kb: u32,
    num: u32,
    fixed: u32,
) -> i32 {
    unsafe {
        let f: unsafe extern "C" fn(u32, u32, u32, u32, u32, u32) -> i32 =
            core::mem::transmute(ROM_CACHE_DBUS_MMU_SET);
        f(mode, vaddr, paddr, psize_kb, num, fixed)
    }
}

// === Helpers used by the trait impl ===

/// Plain SPI read in 32-byte chunks. The ROM `esp_rom_spiflash_read`
/// requires 4-byte aligned `dst` and length, so we stage through a u32 buf.
fn read_via_rom(offset: u32, mut bytes: &mut [u8]) -> Result<(), FlashError> {
    let mut current = offset;
    let mut buf = [0u32; 8]; // 32 bytes, 4-byte aligned
    while !bytes.is_empty() {
        let to_read = bytes.len().min(32);
        let read_len = ((to_read + 3) & !3) as u32;
        let rc = unsafe { rom_read(current, buf.as_mut_ptr(), read_len) };
        if rc != 0 {
            return Err(FlashError::Hardware(rc));
        }
        let src = unsafe {
            core::slice::from_raw_parts(buf.as_ptr() as *const u8, read_len as usize)
        };
        bytes[..to_read].copy_from_slice(&src[..to_read]);
        current += to_read as u32;
        bytes = &mut bytes[to_read..];
    }
    Ok(())
}

impl Esp32C3 {
    fn vaddr_for(&self, flash_offset: u32) -> Option<u32> {
        for m in self.mappings.iter().take(self.mapping_count).flatten() {
            if flash_offset >= m.flash_offset
                && flash_offset < m.flash_offset + m.size
            {
                return Some(m.vaddr_base + (flash_offset - m.flash_offset));
            }
        }
        None
    }
}

impl FlashHardware for Esp32C3 {
    fn capacity(&self) -> usize {
        self.capacity
    }

    fn sector_size(&self) -> u32 {
        SECTOR_SIZE
    }

    fn enc_block_size(&self) -> u32 {
        ENC_BLOCK_SIZE
    }

    fn read_plain(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), FlashError> {
        read_via_rom(offset, buf)
    }

    fn write_plain(&mut self, offset: u32, data: &[u8]) -> Result<(), FlashError> {
        // ROM `esp_rom_spiflash_write` wants 4-byte aligned addr/length and
        // a u32 source. Caller is responsible for sector-aligning when
        // appropriate; here we just do whatever they asked for.
        if (offset & 3) != 0 || (data.len() & 3) != 0 {
            return Err(FlashError::NotAligned);
        }
        let rc = unsafe { rom_write(offset, data.as_ptr() as *const u32, data.len() as u32) };
        if rc != 0 {
            return Err(FlashError::Hardware(rc));
        }
        Ok(())
    }

    fn write_encrypted(&mut self, offset: u32, data: &[u8]) -> Result<(), FlashError> {
        if (offset % ENC_BLOCK_SIZE) != 0 || (data.len() as u32 % ENC_BLOCK_SIZE) != 0 {
            return Err(FlashError::NotAligned);
        }
        let rc = unsafe {
            rom_write_encrypted(offset, data.as_ptr() as *const u32, data.len() as u32)
        };
        if rc != 0 {
            return Err(FlashError::Hardware(rc));
        }
        // Invalidate any decrypt-cache lines for the range so a later read
        // sees fresh ciphertext rather than stale lines from before the write.
        if let Some(vaddr) = self.vaddr_for(offset) {
            let page_aligned = vaddr & !(MMU_PAGE_SIZE - 1);
            unsafe { rom_cache_invalidate_addr(page_aligned, MMU_PAGE_SIZE) };
        }
        Ok(())
    }

    fn erase_sector(&mut self, sector_index: u32) -> Result<(), FlashError> {
        let rc = unsafe { rom_erase_sector(sector_index) };
        if rc != 0 {
            return Err(FlashError::Hardware(rc));
        }
        Ok(())
    }

    fn unlock(&mut self) -> Result<(), FlashError> {
        let rc = unsafe { rom_unlock() };
        if rc != 0 {
            return Err(FlashError::Locked);
        }
        Ok(())
    }

    fn map_decrypt_region(
        &mut self,
        flash_offset: u32,
        size: u32,
    ) -> Result<(), FlashError> {
        if (flash_offset % MMU_PAGE_SIZE) != 0 {
            return Err(FlashError::NotAligned);
        }
        // Idempotent: if we've already mapped this exact (offset, size),
        // bail out without consuming more entries.
        for m in self.mappings.iter().take(self.mapping_count).flatten() {
            if m.flash_offset == flash_offset && m.size == size {
                return Ok(());
            }
        }
        if self.mapping_count >= MAX_MAPPINGS {
            return Err(FlashError::OutOfBounds);
        }
        let num_pages = size.div_ceil(MMU_PAGE_SIZE);
        let entry_base = self.next_mmu_entry;
        let vaddr_base = DBUS_VBASE + entry_base * MMU_PAGE_SIZE;
        let rc = unsafe {
            // mode=0 (MMU_ACCESS_FLASH), psize_kb=64, fixed=0
            rom_cache_dbus_mmu_set(0, vaddr_base, flash_offset, 64, num_pages, 0)
        };
        if rc != 0 {
            return Err(FlashError::Hardware(rc));
        }
        unsafe { rom_cache_invalidate_addr(vaddr_base, num_pages * MMU_PAGE_SIZE) };

        self.mappings[self.mapping_count] = Some(Mapping {
            flash_offset,
            size,
            vaddr_base,
        });
        self.mapping_count += 1;
        self.next_mmu_entry += num_pages;
        Ok(())
    }

    fn is_decrypt_mapped(&self, flash_offset: u32) -> bool {
        self.vaddr_for(flash_offset).is_some()
    }

    fn read_decrypted(
        &mut self,
        flash_offset: u32,
        buf: &mut [u8],
    ) -> Result<(), FlashError> {
        let vaddr = self.vaddr_for(flash_offset).ok_or(FlashError::OutOfBounds)?;
        // Invalidate cache for the read range — defensively, in case stale
        // ciphertext is still cached from before the most recent write.
        let page_aligned = vaddr & !(MMU_PAGE_SIZE - 1);
        let span = ((vaddr + buf.len() as u32 + MMU_PAGE_SIZE - 1)
            & !(MMU_PAGE_SIZE - 1))
            - page_aligned;
        unsafe { rom_cache_invalidate_addr(page_aligned, span) };
        let src = unsafe { core::slice::from_raw_parts(vaddr as *const u8, buf.len()) };
        buf.copy_from_slice(src);
        Ok(())
    }
}
