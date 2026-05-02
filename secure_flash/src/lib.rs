//! Hardware-agnostic flash storage helpers for chips with flash encryption.
//!
//! Two storage types live here, both generic over a [`FlashHardware`] trait:
//!
//!  - [`PlainFlashStorage`] reads and writes raw bytes via plain SPI calls.
//!    Suitable for partitions that the bootloader never reads (NVS, user
//!    data) — both reads and writes see the same plaintext bytes, so the
//!    chip's encryption hardware is bypassed for these regions.
//!
//!  - [`EncryptedOtaStorage`] is the encryption-aware sibling. Reads of
//!    addresses inside caller-mapped regions go through the hardware's
//!    decrypted-read path; writes go through the chip's encrypt-on-write
//!    hardware. A per-block buffer coalesces sub-block writes (e.g. the OTA
//!    library's three 4-byte seq/state/crc updates), and per-sector erase
//!    tracking ensures the destination is always `0xFF` before each
//!    encrypted write.
//!
//! See [`esp32c3::Esp32C3`] for the concrete ESP32-C3 backend, [`mock::MockFlash`]
//! for the in-memory test backend.

#![cfg_attr(not(any(test, feature = "mock")), no_std)]

#[cfg(feature = "esp32c3")]
pub mod esp32c3;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

mod encrypted;
mod plain;

pub use encrypted::EncryptedOtaStorage;
pub use plain::PlainFlashStorage;

/// Errors returned by flash storage operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashError {
    /// The requested offset/length exceeds the flash capacity.
    OutOfBounds,
    /// Address or length not aligned to a hardware-required boundary.
    NotAligned,
    /// Flash chip wasn't unlocked for writes (or unlock failed).
    Locked,
    /// A hardware-level error from a ROM call or peripheral. The wrapped
    /// integer is the raw return code so callers can match on specific
    /// failures.
    Hardware(i32),
}

impl core::fmt::Display for FlashError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutOfBounds => write!(f, "out of bounds"),
            Self::NotAligned => write!(f, "not aligned"),
            Self::Locked => write!(f, "flash locked"),
            Self::Hardware(rc) => write!(f, "hardware error rc={}", rc),
        }
    }
}

/// A contiguous flash region whose bytes we want to read decrypted.
///
/// Pass these to [`EncryptedOtaStorage::new`] for partitions whose contents
/// were written via the encryption hardware (otadata, OTA app slots).
#[derive(Debug, Clone, Copy)]
pub struct FlashRegion {
    pub offset: u32,
    pub size: u32,
}

impl FlashRegion {
    pub const fn new(offset: u32, size: u32) -> Self {
        Self { offset, size }
    }
}

/// Abstraction over a flash chip with optional encryption support.
///
/// All address arguments are flash-physical offsets (i.e. byte 0 = bootloader
/// region). Backends are responsible for any vaddr translation, ROM-call
/// thunking, MMU setup, and cache invalidation that the underlying hardware
/// requires.
pub trait FlashHardware {
    /// Total flash capacity in bytes.
    fn capacity(&self) -> usize;

    /// Sector size for erase. Typically 4096 bytes on ESP32-family chips.
    fn sector_size(&self) -> u32;

    /// Block size for encrypted writes. 32 bytes on ESP32-C3 (XTS-AES-128).
    fn enc_block_size(&self) -> u32;

    /// Plain SPI read. Returns raw flash bytes — ciphertext on chips with
    /// flash encryption enabled, plaintext on chips without.
    fn read_plain(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), FlashError>;

    /// Plain SPI write. Bytes go to flash literally; the encryption hardware
    /// is **not** engaged. Caller is responsible for erasing the destination
    /// sector first.
    fn write_plain(&mut self, offset: u32, data: &[u8]) -> Result<(), FlashError>;

    /// Encrypt-on-write. The chip's hardware encrypts `data` on the way to
    /// flash, so subsequent reads through the decrypt path return `data`.
    ///
    /// Constraints:
    ///   - `offset` must be aligned to [`Self::enc_block_size`]
    ///   - `data.len()` must be a multiple of [`Self::enc_block_size`]
    ///   - the destination region must be erased to `0xFF` first
    ///     (encrypt-write to non-erased flash silently fails to flip 0→1
    ///     bits, producing wrong ciphertext)
    ///
    /// Implementations should also invalidate any decrypt-cache lines for
    /// the affected range so a subsequent [`Self::read_decrypted`] sees the
    /// just-written ciphertext rather than stale cache.
    fn write_encrypted(&mut self, offset: u32, data: &[u8]) -> Result<(), FlashError>;

    /// Erase a single sector. The byte range
    /// `[sector_index * sector_size, (sector_index + 1) * sector_size)` is
    /// reset to all `0xFF` on flash.
    fn erase_sector(&mut self, sector_index: u32) -> Result<(), FlashError>;

    /// Unlock flash for write/erase operations. May be a no-op on backends
    /// that don't track lock state.
    fn unlock(&mut self) -> Result<(), FlashError>;

    /// Set up a mapping so that subsequent [`Self::read_decrypted`] calls in
    /// `[flash_offset, flash_offset + size)` return decrypted plaintext.
    /// Idempotent; safe to call repeatedly with the same arguments.
    fn map_decrypt_region(
        &mut self,
        flash_offset: u32,
        size: u32,
    ) -> Result<(), FlashError>;

    /// True if `flash_offset` is inside a region previously passed to
    /// [`Self::map_decrypt_region`].
    fn is_decrypt_mapped(&self, flash_offset: u32) -> bool;

    /// Read decrypted plaintext from a previously-mapped region.
    /// Returns `Err(OutOfBounds)` if the address isn't mapped.
    /// Implementations should invalidate any stale cache lines first.
    fn read_decrypted(
        &mut self,
        flash_offset: u32,
        buf: &mut [u8],
    ) -> Result<(), FlashError>;
}

/// Bounds-check helper used by both storage types.
pub(crate) fn check_bounds(
    offset: u32,
    length: usize,
    capacity: usize,
) -> Result<(), FlashError> {
    let off = offset as usize;
    if length > capacity || off > capacity - length {
        return Err(FlashError::OutOfBounds);
    }
    Ok(())
}

/// Expand a `(offset, size)` flash region to the smallest range of whole pages
/// that fully contains it. `page` must be a power of two.
///
/// Used by hardware backends whose mapping unit is larger than the regions
/// callers care about (e.g. ESP32-C3's MMU operates in 64 KiB pages but
/// otadata lives inside one of those pages).
pub(crate) fn page_align_region(offset: u32, size: u32, page: u32) -> (u32, u32) {
    debug_assert!(page.is_power_of_two());
    let mask = page - 1;
    let aligned_start = offset & !mask;
    let end = offset.saturating_add(size);
    let aligned_end = (end + mask) & !mask;
    (aligned_start, aligned_end - aligned_start)
}

#[cfg(test)]
mod align_tests {
    use super::page_align_region;

    #[test]
    fn aligned_region_unchanged() {
        assert_eq!(page_align_region(0x10000, 0x10000, 0x10000), (0x10000, 0x10000));
        assert_eq!(page_align_region(0x20000, 0x170000, 0x10000), (0x20000, 0x170000));
    }

    #[test]
    fn unaligned_start_grows_backwards_to_page() {
        // otadata at flash 0x18000, size 0x2000 — fits inside 0x10000..0x1FFFF.
        assert_eq!(
            page_align_region(0x18000, 0x2000, 0x10000),
            (0x10000, 0x10000)
        );
    }

    #[test]
    fn region_spanning_page_boundary_grows_to_cover_both() {
        // 0x18000 + 0x9000 = 0x21000 — runs into the next page, must cover it.
        assert_eq!(
            page_align_region(0x18000, 0x9000, 0x10000),
            (0x10000, 0x20000)
        );
    }

    #[test]
    fn region_smaller_than_page_pads_up_to_one_page() {
        // 4 KiB sector inside a 64 KiB page → expands to one full page.
        assert_eq!(
            page_align_region(0x18000, 0x1000, 0x10000),
            (0x10000, 0x10000)
        );
    }
}
