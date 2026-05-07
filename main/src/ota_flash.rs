//! ESP32-C3 implementation of the OTA [`FlashWriter`] trait
//!
//! Wraps `esp-hal-ota` to provide partition discovery, chunk writing,
//! CRC verification, and boot target management.

#[cfg(not(feature = "secure-boot"))]
use esp_hal_ota::OtaConfiguratuion;
#[cfg(feature = "secure-boot")]
use esp_hal_ota::PartitionInfo;
use esp_hal_ota::Ota;
#[cfg(not(feature = "secure-boot"))]
use esp_hal_ota::OtaImgState;
use ota::{FlashWriter, OtaError};

#[cfg(feature = "secure-boot")]
type OtaStorage = secure_flash::EncryptedOtaStorage<secure_flash::esp32c3::Esp32C3>;
#[cfg(not(feature = "secure-boot"))]
use esp_storage::FlashStorage as OtaStorage;

// Regions whose bytes the OTA flow needs to read decrypted: otadata + both
// OTA app slots. Must match `secure_partition_info()` and `partitions_secure.csv`.
#[cfg(feature = "secure-boot")]
const SECURE_OTA_REGIONS: &[secure_flash::FlashRegion] = &[
    secure_flash::FlashRegion::new(0x18000, 0x2000),  // otadata
    secure_flash::FlashRegion::new(0x20000, 0x170000), // ota_0
    secure_flash::FlashRegion::new(0x190000, 0x170000), // ota_1
];

#[cfg(feature = "secure-boot")]
fn new_ota_storage() -> Result<OtaStorage, secure_flash::FlashError> {
    secure_flash::EncryptedOtaStorage::new(
        secure_flash::esp32c3::Esp32C3::default(),
        SECURE_OTA_REGIONS,
    )
}

#[cfg(not(feature = "secure-boot"))]
fn new_ota_storage() -> Result<OtaStorage, core::convert::Infallible> {
    Ok(OtaStorage::new())
}

#[cfg(not(feature = "secure-boot"))]
const PARTITION_TABLE_OFFSET: u32 = 0x8000;

// Partition layout for secure-boot mode. Must match `main/partitions_secure.csv`.
// The partition table at 0x10000 is encrypted and not readable through plain
// SPI ROM calls, so the OTA library is told the layout up front.
#[cfg(feature = "secure-boot")]
fn secure_partition_info() -> PartitionInfo {
    let mut ota_partitions = [(0u32, 0u32); 16];
    ota_partitions[0] = (0x20000, 0x170000); // ota_0
    ota_partitions[1] = (0x190000, 0x170000); // ota_1
    PartitionInfo {
        ota_partitions,
        ota_partitions_count: 2,
        otadata_offset: 0x18000,
        otadata_size: 0x2000,
    }
}

fn new_ota(flash: OtaStorage) -> Result<Ota<OtaStorage>, esp_hal_ota::OtaError> {
    #[cfg(feature = "secure-boot")]
    {
        Ota::with_partition_info(flash, secure_partition_info())
    }
    #[cfg(not(feature = "secure-boot"))]
    {
        Ota::with_configuration(
            flash,
            OtaConfiguratuion::new().with_partition_table_offset(PARTITION_TABLE_OFFSET),
        )
    }
}

/// ESP32-C3 OTA flash writer backed by `esp-hal-ota`
pub struct EspFlashWriter {
    ota: Ota<OtaStorage>,
}

impl EspFlashWriter {
    /// Create a new flash writer.
    pub fn new() -> Result<Self, OtaError> {
        let flash = new_ota_storage().map_err(|e| {
            log::error!("Failed to construct OTA flash storage: {:?}", e);
            OtaError::FinalizeError
        })?;
        let ota = new_ota(flash).map_err(|e| {
            log::error!("Failed to initialize OTA: {:?}", e);
            OtaError::FinalizeError
        })?;
        Ok(Self { ota })
    }
}

impl FlashWriter for EspFlashWriter {
    fn begin(&mut self, size: u32, crc32: u32) -> Result<(), OtaError> {
        self.ota.ota_begin(size, crc32).map_err(|e| {
            log::error!("OTA begin failed: {:?}", e);
            OtaError::FlashWriteError
        })
    }

    fn write_chunk(&mut self, data: &[u8]) -> Result<(), OtaError> {
        self.ota.ota_write_chunk(data).map_err(|e| {
            log::error!("OTA write chunk failed: {:?}", e);
            OtaError::FlashWriteError
        })?;
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), OtaError> {
        // Secure-boot builds: verify and rollback both work because
        // `EncryptedOtaStorage::read` routes reads of otadata and OTA
        // partitions through cache-MMU mappings that decrypt on the fly,
        // and `Storage::write` does per-block read-modify-encrypt-write so
        // the non-target slot in otadata is left untouched. That preservation
        // is what makes bootloader rollback work — when the new firmware
        // fails to mark valid, the bootloader needs the OTHER slot to still
        // hold the previous valid entry pointing to the previous firmware.
        self.ota.ota_flush(true, true).map_err(|e| {
            log::error!("OTA finalize failed: {:?}", e);
            OtaError::FinalizeError
        })?;

        // Commit the final partial block of firmware and the otadata block,
        // both of which are buffered in the encryption-aware storage.
        #[cfg(feature = "secure-boot")]
        {
            self.ota.flash_mut().flush().map_err(|e| {
                log::error!("OTA flush failed: {:?}", e);
                OtaError::FinalizeError
            })?;
        }

        Ok(())
    }

    fn is_pending_verification(&self) -> bool {
        // With CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y in the bootloader, an
        // app on its first boot after OTA sees state `EspOtaImgPendingVerify`
        // (the bootloader transitions from `New` before handing off). We treat
        // both as "needs to be marked valid".
        //
        // We deliberately bypass `esp_hal_ota::Ota::get_ota_image_state` here:
        // that path constructs an `OtaImgState` enum from raw flash bytes via
        // an unsafe `core::ptr::read`, which is UB if the bytes don't match a
        // valid discriminant — e.g. if the cache MMU read momentarily produces
        // garbage. UB on an enum with `derive(PartialEq, Debug)` has caused
        // panics in this code path before. Reading the state field as a `u32`
        // is safe regardless of what the bytes are.
        #[cfg(feature = "secure-boot")]
        {
            use embedded_storage::ReadStorage;
            let Ok(mut flash) = new_ota_storage() else {
                return false;
            };
            // otadata has two 32-byte select entries, each in its own 4 KB
            // sector. The state field lives at byte offset 24 of each entry.
            const STATE_NEW: u32 = 0; // EspOtaImgNew
            const STATE_PENDING: u32 = 1; // EspOtaImgPendingVerify
            let pinfo = secure_partition_info();
            for slot_offset in [
                pinfo.otadata_offset,
                pinfo.otadata_offset + (pinfo.otadata_size >> 1),
            ] {
                let mut state_bytes = [0u8; 4];
                if flash.read(slot_offset + 24, &mut state_bytes).is_err() {
                    continue;
                }
                let state = u32::from_le_bytes(state_bytes);
                if state == STATE_NEW || state == STATE_PENDING {
                    return true;
                }
            }
            false
        }
        #[cfg(not(feature = "secure-boot"))]
        {
            // In dev mode `new_ota_storage` is infallible.
            let Ok(flash) = new_ota_storage();
            let mut ota = match new_ota(flash) {
                Ok(ota) => ota,
                Err(_) => return false,
            };
            matches!(
                ota.get_ota_image_state(),
                Ok(OtaImgState::EspOtaImgNew) | Ok(OtaImgState::EspOtaImgPendingVerify)
            )
        }
    }

    fn mark_valid(&mut self) -> Result<(), OtaError> {
        // On secure-boot builds we go around `esp_hal_ota::ota_mark_app_valid`
        // for the same enum-UB reason as `is_pending_verification`: the
        // library's path compares the existing state field as an `OtaImgState`
        // enum, which is UB if the bytes happen not to match a valid
        // discriminant. We just need to write `EspOtaImgValid` (= 2) into the
        // state field of the active otadata slot, then flush.
        #[cfg(feature = "secure-boot")]
        {
            use embedded_storage::Storage;
            const STATE_VALID: u32 = 2; // EspOtaImgValid

            // Pick the active slot the same way the OTA library does: whichever
            // has the higher seq AND whose seq maps to the currently-running
            // partition. We mirror that logic here by reading both slot seqs
            // raw. If the read fails we fall back to slot 1 (the lowest-offset
            // slot), which matches the library's tiebreak.
            let pinfo = secure_partition_info();
            let slot1_offset = pinfo.otadata_offset;
            let slot2_offset = pinfo.otadata_offset + (pinfo.otadata_size >> 1);
            let active_slot_offset = active_otadata_slot(
                self.ota.flash_mut(),
                slot1_offset,
                slot2_offset,
            )
            .unwrap_or(slot1_offset);

            self.ota
                .flash_mut()
                .write(active_slot_offset + 24, &STATE_VALID.to_le_bytes())
                .map_err(|e| {
                    log::error!("OTA mark valid write failed: {:?}", e);
                    OtaError::FinalizeError
                })?;

            // Our buffered encrypted writer holds the partial 4-byte write in
            // its per-block buffer; flush it so the new state actually reaches
            // flash before we return.
            self.ota.flash_mut().flush().map_err(|e| {
                log::error!("OTA mark valid flush failed: {:?}", e);
                OtaError::FinalizeError
            })?;

            log::info!("Marked current slot as valid!");
            return Ok(());
        }

        #[cfg(not(feature = "secure-boot"))]
        self.ota.ota_mark_app_valid().map_err(|e| {
            log::error!("OTA mark valid failed: {:?}", e);
            OtaError::FinalizeError
        })
    }
}

/// Pick which otadata slot holds the active (most recently committed) entry.
/// Returns `None` if neither slot's CRC validates. Operates on raw 4-byte
/// reads so the result doesn't depend on `EspOtaSelectEntry` layout — and
/// crucially doesn't construct any `OtaImgState` enum value from arbitrary
/// flash bytes.
#[cfg(feature = "secure-boot")]
fn active_otadata_slot<S: embedded_storage::ReadStorage>(
    flash: &mut S,
    slot1_offset: u32,
    slot2_offset: u32,
) -> Option<u32> {
    let read_seq_and_crc = |off: u32, flash: &mut S| -> Option<(u32, u32)> {
        let mut head = [0u8; 4];
        let mut tail = [0u8; 4];
        flash.read(off, &mut head).ok()?;
        flash.read(off + 28, &mut tail).ok()?;
        Some((u32::from_le_bytes(head), u32::from_le_bytes(tail)))
    };

    let valid = |(seq, crc): (u32, u32)| {
        // CRC32 of `seq.to_le_bytes()` with init 0xFFFFFFFF, matching how
        // `esp_hal_ota::set_target_ota_boot_partition` computes it.
        esp_hal_ota::crc32::calc_crc32(&seq.to_le_bytes(), 0xFFFFFFFF) == crc
    };

    let s1 = read_seq_and_crc(slot1_offset, flash);
    let s2 = read_seq_and_crc(slot2_offset, flash);

    match (s1, s2) {
        (Some(a), Some(b)) if valid(a) && valid(b) => {
            if a.0 >= b.0 { Some(slot1_offset) } else { Some(slot2_offset) }
        }
        (Some(a), _) if valid(a) => Some(slot1_offset),
        (_, Some(b)) if valid(b) => Some(slot2_offset),
        _ => None,
    }
}
