//! ESP32-C3 implementation of the OTA [`FlashWriter`] trait
//!
//! Wraps `esp-hal-ota` to provide partition discovery, chunk writing,
//! CRC verification, and boot target management.

#[cfg(feature = "secure-boot")]
use esp_hal_ota::PartitionInfo;
use esp_hal_ota::{Ota, OtaImgState};
#[cfg(not(feature = "secure-boot"))]
use esp_hal_ota::OtaConfiguratuion;
use ota::{FlashWriter, OtaError};

#[cfg(feature = "secure-boot")]
use crate::encrypted_flash::EncryptedOtaStorage as OtaStorage;
#[cfg(not(feature = "secure-boot"))]
use esp_storage::FlashStorage as OtaStorage;

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
        let flash = OtaStorage::new();
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
        // verify=false on secure-boot builds: we can't read decrypted bytes
        // through the plain SPI ROM path to compute a CRC, so skip the
        // post-write verify. The bootloader still verifies the secure-boot
        // signature on next boot, so a corrupted write would fail to boot
        // anyway.
        // rollback=false on secure-boot builds: the rollback flag relies on
        // reading otadata back as plaintext, which we also can't do.
        #[cfg(feature = "secure-boot")]
        let (verify, rollback) = (false, false);
        #[cfg(not(feature = "secure-boot"))]
        let (verify, rollback) = (true, true);

        // On secure-boot builds, esp-hal-ota writes the new otadata entry
        // without erasing first. Encrypted writes need the destination block
        // to be 0xFF for our buffered-write trick to produce correct
        // ciphertext, so we erase both otadata slots up front. The library
        // will then read both as 0xFF, treat them as having seq=0, and write
        // a fresh entry with seq=1 (or 2) to the correct slot.
        #[cfg(feature = "secure-boot")]
        {
            let (offset, size) = self.ota.otadata_region();
            self.ota.flash_mut().erase_region(offset, size).map_err(|e| {
                log::error!("Failed to erase otadata: {:?}", e);
                OtaError::FinalizeError
            })?;
        }

        self.ota.ota_flush(verify, rollback).map_err(|e| {
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
        // We need a mutable reference for get_ota_image_state, but the trait
        // requires &self. Use a conservative approach: check if the current
        // partition was recently written (state is New).
        // This is called once at boot before any mutation, so it's safe to
        // do a fresh read.
        let flash = OtaStorage::new();
        let mut ota = match new_ota(flash) {
            Ok(ota) => ota,
            Err(_) => return false,
        };
        matches!(ota.get_ota_image_state(), Ok(OtaImgState::EspOtaImgNew))
    }

    fn mark_valid(&mut self) -> Result<(), OtaError> {
        self.ota.ota_mark_app_valid().map_err(|e| {
            log::error!("OTA mark valid failed: {:?}", e);
            OtaError::FinalizeError
        })
    }
}
