//! [`EncryptedOtaStorage`] — encryption-aware storage for OTA partitions and
//! otadata. Reads inside caller-mapped regions go through the hardware's
//! decrypted-read path; writes go through encrypt-on-write hardware. Sub-block
//! writes (e.g. the OTA library's three 4-byte field updates to otadata) are
//! coalesced via a per-block buffer.

use embedded_storage::{ReadStorage, Storage};

use crate::{check_bounds, FlashError, FlashHardware, FlashRegion};

/// Maximum encryption block size we allocate a stack buffer for. ESP32-C3 is
/// 32 bytes (XTS-AES-128). Larger values waste a few stack bytes; smaller
/// would be incorrect.
const MAX_ENC_BLOCK_SIZE: usize = 32;

/// Single block staged for an encrypted write.
///
/// Encrypt-on-write requires a full-block plaintext buffer per call, but
/// callers (esp-hal-ota in particular) often write a single block in several
/// sub-block fragments. We accumulate the fragments in `data` and only emit
/// the encrypted write when the block fills, the caller starts writing to a
/// different block, or [`EncryptedOtaStorage::flush`] is called explicitly.
struct PendingBlock {
    addr: u32,
    data: [u8; MAX_ENC_BLOCK_SIZE],
}

/// Encryption-aware OTA storage.
pub struct EncryptedOtaStorage<H: FlashHardware> {
    hw: H,
    unlocked: bool,
    pending: Option<PendingBlock>,
    /// Sector currently in the middle of being (over)written. When a write
    /// targets a different sector, that new sector is erased first. This
    /// matters because the OTA library doesn't erase before writing —
    /// encrypt-write to a non-erased sector silently fails to flip 0→1 bits
    /// and produces wrong ciphertext.
    current_sector: Option<u32>,
}

impl<H: FlashHardware> EncryptedOtaStorage<H> {
    /// Construct with the regions whose contents need to be readable
    /// decrypted. Each region is registered with the hardware via
    /// [`FlashHardware::map_decrypt_region`]; reads inside any of these
    /// regions will go through the decrypt path.
    pub fn new(mut hw: H, regions: &[FlashRegion]) -> Result<Self, FlashError> {
        for r in regions {
            hw.map_decrypt_region(r.offset, r.size)?;
        }
        Ok(Self {
            hw,
            unlocked: false,
            pending: None,
            current_sector: None,
        })
    }

    /// Borrow the underlying hardware mutably (useful for tests / debugging).
    pub fn hw_mut(&mut self) -> &mut H {
        &mut self.hw
    }

    /// Consume this storage and return the underlying hardware. Useful for
    /// tests that want to recreate a storage with fresh per-instance state
    /// (`pending`, `current_sector`) while keeping the persistent flash
    /// state intact.
    pub fn into_hw(self) -> H {
        self.hw
    }

    /// Commit any block currently in the per-block buffer.
    ///
    /// Must be called after a sequence of writes that may end on a sub-block
    /// boundary — e.g., the OTA library's `set_target_ota_boot_partition`
    /// stops on a 4-byte CRC write at offset 28 of the block, which leaves
    /// the buffer not-quite-full. Forgetting to flush means the otadata
    /// update never reaches flash.
    pub fn flush(&mut self) -> Result<(), FlashError> {
        self.flush_pending()
    }

    /// Erase a contiguous byte range, sector by sector.
    ///
    /// Useful for tests and debug paths. The OTA flow itself relies on
    /// per-sector erase tracking inside [`Storage::write`], so callers
    /// normally don't need this.
    pub fn erase_region(&mut self, offset: u32, length: u32) -> Result<(), FlashError> {
        self.unlock_once()?;
        let ss = self.hw.sector_size();
        let start = offset / ss;
        let end = (offset + length).div_ceil(ss);
        for sector in start..end {
            self.hw.erase_sector(sector)?;
        }
        Ok(())
    }

    fn unlock_once(&mut self) -> Result<(), FlashError> {
        if !self.unlocked {
            self.hw.unlock()?;
            self.unlocked = true;
        }
        Ok(())
    }

    fn flush_pending(&mut self) -> Result<(), FlashError> {
        let Some(p) = self.pending.take() else {
            return Ok(());
        };
        let block_size = self.hw.enc_block_size();
        let sector_size = self.hw.sector_size();

        // Erase the destination sector if we haven't already started writing
        // to it. We track sectors so multiple block writes within the same
        // sector don't re-erase (which would wipe earlier writes).
        let sector = p.addr / sector_size;
        if self.current_sector != Some(sector) {
            self.hw.erase_sector(sector)?;
            self.current_sector = Some(sector);
        }

        self.hw
            .write_encrypted(p.addr, &p.data[..block_size as usize])?;
        Ok(())
    }
}

impl<H: FlashHardware> ReadStorage for EncryptedOtaStorage<H> {
    type Error = FlashError;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_bounds(offset, bytes.len(), self.hw.capacity())?;
        if self.hw.is_decrypt_mapped(offset) {
            self.hw.read_decrypted(offset, bytes)
        } else {
            self.hw.read_plain(offset, bytes)
        }
    }

    fn capacity(&self) -> usize {
        self.hw.capacity()
    }
}

impl<H: FlashHardware> Storage for EncryptedOtaStorage<H> {
    fn write(&mut self, offset: u32, mut bytes: &[u8]) -> Result<(), Self::Error> {
        check_bounds(offset, bytes.len(), self.hw.capacity())?;
        self.unlock_once()?;

        let block_size = self.hw.enc_block_size();
        debug_assert!(block_size as usize <= MAX_ENC_BLOCK_SIZE);

        let mut current = offset;
        while !bytes.is_empty() {
            let block_addr = current & !(block_size - 1);
            let off_in_block = (current - block_addr) as usize;

            // Different block buffered → commit it before moving on.
            if let Some(p) = &self.pending {
                if p.addr != block_addr {
                    self.flush_pending()?;
                }
            }

            let to_write =
                (block_size as usize - off_in_block).min(bytes.len());

            // Initialise the per-block buffer if we don't already have one
            // for this block. For sub-block writes we *must* read the
            // existing decrypted bytes first; otherwise our flush_pending
            // erase-then-write would zero out the bytes we aren't touching.
            // Full-block writes don't need the read — the caller is
            // overwriting all of `data`.
            if self.pending.is_none() {
                let mut data = [0xFFu8; MAX_ENC_BLOCK_SIZE];
                let is_partial =
                    off_in_block != 0 || to_write < block_size as usize;
                if is_partial && self.hw.is_decrypt_mapped(block_addr) {
                    self.hw
                        .read_decrypted(block_addr, &mut data[..block_size as usize])?;
                }
                self.pending = Some(PendingBlock {
                    addr: block_addr,
                    data,
                });
            }

            let pending = self.pending.as_mut().expect("pending was just set");
            pending.data[off_in_block..off_in_block + to_write]
                .copy_from_slice(&bytes[..to_write]);

            current += to_write as u32;
            bytes = &bytes[to_write..];

            // Block is now fully populated → commit immediately so the next
            // block starts from a clean slate.
            if off_in_block + to_write == block_size as usize {
                self.flush_pending()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockFlash;
    use embedded_storage::{ReadStorage, Storage};

    const OTADATA_OFFSET: u32 = 0x18000;
    const OTADATA_SIZE: u32 = 0x2000;
    const OTA0_OFFSET: u32 = 0x20000;
    const OTA0_SIZE: u32 = 0x10000;

    fn make_storage() -> EncryptedOtaStorage<MockFlash> {
        let regions = [
            FlashRegion::new(OTADATA_OFFSET, OTADATA_SIZE),
            FlashRegion::new(OTA0_OFFSET, OTA0_SIZE),
        ];
        EncryptedOtaStorage::new(MockFlash::new(0x100000), &regions).unwrap()
    }

    #[test]
    fn full_block_write_round_trip() {
        let mut s = make_storage();
        let plaintext = [0x42u8; 32];
        s.write(OTA0_OFFSET, &plaintext).unwrap();
        s.flush().unwrap();

        let mut got = [0u8; 32];
        s.read(OTA0_OFFSET, &mut got).unwrap();
        assert_eq!(got, plaintext, "decrypt of just-encrypted block must round-trip");
    }

    #[test]
    fn plain_read_returns_ciphertext_not_plaintext() {
        // The OTA partition is encrypted on-write; a plain SPI read should
        // see something other than the plaintext we wrote.
        let mut s = make_storage();
        let plaintext = [0xAAu8; 32];
        s.write(OTA0_OFFSET, &plaintext).unwrap();
        s.flush().unwrap();

        let mut raw = [0u8; 32];
        // Address outside any mapped region routes to plain read.
        let unmapped_addr = OTA0_OFFSET + OTA0_SIZE; // beyond the mapped ota_0
        let _ = s.read(unmapped_addr, &mut raw); // just to hit plain path

        // Sanity: we can't directly inspect ciphertext for the encrypted
        // region without using hw_mut, but verify the decrypt path returns
        // plaintext (proves encryption was engaged).
        let mut decrypted = [0u8; 32];
        s.read(OTA0_OFFSET, &mut decrypted).unwrap();
        assert_eq!(decrypted, plaintext);

        // Now check: hw's plain read at the same flash offset must NOT match
        // plaintext (because it was written encrypted).
        let mut raw_at_ota0 = [0u8; 32];
        s.hw_mut().read_plain(OTA0_OFFSET, &mut raw_at_ota0).unwrap();
        assert_ne!(raw_at_ota0, plaintext, "raw flash must hold ciphertext");
    }

    #[test]
    fn three_subblock_writes_to_same_block_coalesce() {
        // Simulates esp-hal-ota's `set_target_ota_boot_partition`: three
        // 4-byte writes into the same 32-byte otadata block at offsets
        // 0, 24, 28. The non-touched bytes (4..24) must come back as the
        // existing decrypted plaintext at the time of the first sub-block
        // write — that's what makes mark-valid preserve seq/label/crc.
        //
        // We don't pre-write a full block here; production-mark-valid runs
        // on a fresh storage instance (one per call), so its `current_sector`
        // tracker is empty and the destination sector gets erased on the
        // first sub-block write.
        let mut s = make_storage();

        // Capture what the decrypted view of the label area looks like
        // *before* any writes — this is what our partial-write read-modify
        // path is supposed to preserve verbatim.
        let mut original_label = [0u8; 20];
        s.read(OTADATA_OFFSET + 4, &mut original_label).unwrap();

        let new_seq: u32 = 7;
        let new_state: u32 = 1; // PendingVerify
        let new_crc: u32 = 0xDEADBEEF;
        s.write(OTADATA_OFFSET, &new_seq.to_le_bytes()).unwrap();
        s.write(OTADATA_OFFSET + 24, &new_state.to_le_bytes()).unwrap();
        s.write(OTADATA_OFFSET + 28, &new_crc.to_le_bytes()).unwrap();
        s.flush().unwrap();

        let mut got = [0u8; 32];
        s.read(OTADATA_OFFSET, &mut got).unwrap();

        assert_eq!(&got[0..4], &new_seq.to_le_bytes(), "seq should be updated");
        assert_eq!(&got[4..24], &original_label, "label should be preserved");
        assert_eq!(&got[24..28], &new_state.to_le_bytes(), "state should be updated");
        assert_eq!(&got[28..32], &new_crc.to_le_bytes(), "crc should be updated");
    }

    #[test]
    fn mark_valid_pattern_on_freshly_minted_storage_preserves_other_fields() {
        // Tighter regression for the production mark-valid flow: write a
        // valid-looking otadata entry, drop the storage, recreate from the
        // same backing flash, then write *only* the state field — exactly
        // what `mark_valid` does. The seq, label, and crc bytes must come
        // back unchanged.
        use crate::FlashRegion;

        let mut hw = MockFlash::new(0x100000);
        let regions = [
            FlashRegion::new(OTADATA_OFFSET, OTADATA_SIZE),
            FlashRegion::new(OTA0_OFFSET, OTA0_SIZE),
        ];
        for r in &regions {
            hw.map_decrypt_region(r.offset, r.size).unwrap();
        }
        hw.unlock().unwrap();
        // Hand-craft an "OTA library wrote a valid entry" state by writing
        // a full block via storage1.
        let mut block = [0u8; 32];
        block[0..4].copy_from_slice(&7u32.to_le_bytes());
        block[4..24].copy_from_slice(&[0xC1u8; 20]);
        block[24..28].copy_from_slice(&1u32.to_le_bytes()); // PendingVerify
        block[28..32].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());

        let mut s1 = EncryptedOtaStorage::new(hw, &regions).unwrap();
        s1.write(OTADATA_OFFSET, &block).unwrap();
        s1.flush().unwrap();
        let hw = s1.into_hw();

        // Production: mark-valid runs on a freshly-constructed storage, so
        // `current_sector` is None and the destination sector gets erased on
        // the first sub-block write — exactly what we need for the encrypt-
        // write of the read-modified block to land cleanly.
        let mut s2 = EncryptedOtaStorage::new(hw, &regions).unwrap();
        let valid_state: u32 = 2;
        s2.write(OTADATA_OFFSET + 24, &valid_state.to_le_bytes()).unwrap();
        s2.flush().unwrap();

        let mut got = [0u8; 32];
        s2.read(OTADATA_OFFSET, &mut got).unwrap();

        assert_eq!(&got[0..4], &7u32.to_le_bytes(), "seq must be preserved");
        assert_eq!(&got[4..24], &[0xC1u8; 20], "label must be preserved");
        assert_eq!(&got[24..28], &valid_state.to_le_bytes(), "state must be updated");
        assert_eq!(
            &got[28..32],
            &0xDEADBEEFu32.to_le_bytes(),
            "crc must be preserved"
        );
    }

    #[test]
    fn writing_to_other_otadata_slot_preserves_first() {
        // Critical for rollback: writing a new entry to slot 2 must NOT
        // disturb slot 1's contents.
        let mut s = make_storage();
        let slot1_addr = OTADATA_OFFSET;
        let slot2_addr = OTADATA_OFFSET + (OTADATA_SIZE >> 1);

        let slot1_data = [0xA5u8; 32];
        s.write(slot1_addr, &slot1_data).unwrap();
        s.flush().unwrap();

        let slot2_data = [0x5Au8; 32];
        s.write(slot2_addr, &slot2_data).unwrap();
        s.flush().unwrap();

        let mut got1 = [0u8; 32];
        s.read(slot1_addr, &mut got1).unwrap();
        assert_eq!(got1, slot1_data, "slot 1 must be untouched after slot 2 write");

        let mut got2 = [0u8; 32];
        s.read(slot2_addr, &mut got2).unwrap();
        assert_eq!(got2, slot2_data);
    }

    #[test]
    fn sequential_chunks_across_sector_boundary() {
        // OTA chunks come in arbitrarily-sized pieces and can span sector
        // boundaries. Verify that two consecutive 2 KiB writes that together
        // fill a 4 KiB sector and start on a new one all land correctly.
        let mut s = make_storage();
        let chunk1 = [0x11u8; 2048];
        let chunk2 = [0x22u8; 2048];
        let chunk3 = [0x33u8; 2048]; // crosses into next sector

        s.write(OTA0_OFFSET, &chunk1).unwrap();
        s.write(OTA0_OFFSET + 2048, &chunk2).unwrap();
        s.write(OTA0_OFFSET + 4096, &chunk3).unwrap();
        s.flush().unwrap();

        let mut got = [0u8; 6144];
        s.read(OTA0_OFFSET, &mut got).unwrap();
        assert_eq!(&got[..2048], &chunk1);
        assert_eq!(&got[2048..4096], &chunk2);
        assert_eq!(&got[4096..], &chunk3);
    }

    #[test]
    fn out_of_bounds_write_errors() {
        let mut s = make_storage();
        let buf = [0u8; 32];
        let res = s.write(0xFFFF0, &buf);
        assert_eq!(res, Err(FlashError::OutOfBounds));
    }

    #[test]
    fn flush_with_no_pending_is_noop() {
        let mut s = make_storage();
        s.flush().unwrap();
        s.flush().unwrap();
    }

    #[test]
    fn read_outside_mapped_region_uses_plain_path() {
        // Write something via plain path (write_plain on hw) at an unmapped
        // address, then read via the storage — should come back as the same
        // bytes (plain → plain round-trip).
        let mut s = make_storage();
        let scratch_addr = 0x2000; // not in any mapped region
        let scratch_data = b"plain bytes";
        s.hw_mut().unlock().unwrap();
        s.hw_mut().erase_sector(scratch_addr / 4096).unwrap();
        s.hw_mut().write_plain(scratch_addr, scratch_data).unwrap();

        let mut got = [0u8; 11];
        s.read(scratch_addr, &mut got).unwrap();
        assert_eq!(&got, scratch_data);
    }
}
