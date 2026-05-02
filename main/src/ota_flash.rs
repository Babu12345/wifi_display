//! ESP32-C3 implementation of the OTA [`FlashWriter`] trait
//!
//! Wraps `esp-hal-ota` to provide partition discovery, chunk writing,
//! CRC verification, and boot target management.

use esp_hal_ota::{Ota, OtaConfiguratuion, OtaImgState};
use esp_storage::FlashStorage;
use ota::{FlashWriter, OtaError};

#[cfg(feature = "secure-boot")]
const PARTITION_TABLE_OFFSET: u32 = 0x10000;
#[cfg(not(feature = "secure-boot"))]
const PARTITION_TABLE_OFFSET: u32 = 0x8000;

fn new_ota(flash: FlashStorage) -> Result<Ota<FlashStorage>, esp_hal_ota::OtaError> {
    Ota::with_configuration(
        flash,
        OtaConfiguratuion::new().with_partition_table_offset(PARTITION_TABLE_OFFSET),
    )
}

/// ESP32-C3 OTA flash writer backed by `esp-hal-ota`
pub struct EspFlashWriter {
    ota: Ota<FlashStorage>,
}

impl EspFlashWriter {
    /// Create a new flash writer. Reads the partition table from flash.
    pub fn new() -> Result<Self, OtaError> {
        let flash = FlashStorage::new();
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
        // verify=true: re-read flash and check CRC
        // rollback=true: set state to EspOtaImgNew (bootloader will rollback if not marked valid)
        self.ota.ota_flush(true, true).map_err(|e| {
            log::error!("OTA finalize failed: {:?}", e);
            OtaError::FinalizeError
        })
    }

    fn is_pending_verification(&self) -> bool {
        // We need a mutable reference for get_ota_image_state, but the trait
        // requires &self. Use a conservative approach: check if the current
        // partition was recently written (state is New).
        // This is called once at boot before any mutation, so it's safe to
        // do a fresh read.
        let flash = FlashStorage::new();
        let mut ota = match new_ota(flash) {
            Ok(ota) => ota,
            Err(_) => return false,
        };
        matches!(
            ota.get_ota_image_state(),
            Ok(OtaImgState::EspOtaImgNew)
        )
    }

    fn mark_valid(&mut self) -> Result<(), OtaError> {
        self.ota.ota_mark_app_valid().map_err(|e| {
            log::error!("OTA mark valid failed: {:?}", e);
            OtaError::FinalizeError
        })
    }
}
