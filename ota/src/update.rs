//! Core OTA update orchestration
//!
//! All flash interaction is abstracted behind the [`FlashWriter`] trait.
//! HTTP download is intentionally NOT abstracted here — it's async and
//! platform-specific. The caller drives the download loop and feeds
//! chunks to [`OtaManager`] via step-by-step methods.
//!
//! ```text
//! let trigger = mgr.begin_update(payload)?;
//! // ... async HTTP download loop (caller-owned) ...
//! mgr.write_chunk(&chunk)?;
//! // ... repeat until done ...
//! let ack = mgr.finalize_update()?;
//! // publish ack, reboot
//! ```

use crate::message::{self, OtaAck, OtaStatus, OtaTrigger};
use crate::state::{OtaProgress, OtaState};

/// Default chunk size for streaming downloads (4 KB)
pub const DEFAULT_CHUNK_SIZE: usize = 4096;

/// Errors that can occur during an OTA update
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaError {
    /// Failed to parse the trigger message
    InvalidTrigger,
    /// HTTP connection or download failed
    HttpError,
    /// Writing a chunk to flash failed
    FlashWriteError,
    /// Downloaded size doesn't match expected size
    SizeMismatch {
        /// Bytes actually downloaded
        got: u32,
        /// Bytes expected from trigger message
        expected: u32,
    },
    /// Failed to set new partition as boot target
    FinalizeError,
    /// An update is already in progress
    AlreadyInProgress,
    /// Method called out of order (e.g., write before begin)
    NotStarted,
}

/// Abstraction over OTA flash partition operations.
///
/// On ESP32, this wraps `esp-hal-ota`. For testing, use a mock
/// that writes to an in-memory buffer.
pub trait FlashWriter {
    /// Begin an OTA session targeting the inactive partition.
    ///
    /// Called once before any writes. Implementations should find
    /// the inactive OTA slot and prepare it for writing.
    ///
    /// - `size`: expected firmware size in bytes (for partition validation)
    /// - `crc32`: expected CRC32 of the firmware binary (for integrity check at finalize)
    fn begin(&mut self, size: u32, crc32: u32) -> Result<(), OtaError>;

    /// Write a chunk of firmware data to flash at the current offset.
    ///
    /// Called repeatedly during download. The implementation tracks
    /// the write offset internally.
    fn write_chunk(&mut self, data: &[u8]) -> Result<(), OtaError>;

    /// Finalize the OTA update: verify the written image and set
    /// the new partition as the boot target.
    ///
    /// After this returns `Ok(())`, the next reboot will load the
    /// new firmware.
    fn finalize(&mut self) -> Result<(), OtaError>;

    /// Check if the current boot is pending OTA validation.
    ///
    /// Returns `true` if the running firmware was loaded from a
    /// newly-written OTA partition that hasn't been confirmed yet.
    fn is_pending_verification(&self) -> bool;

    /// Mark the currently running OTA firmware as valid.
    ///
    /// Must be called after the new firmware successfully connects
    /// to WiFi + MQTT. If this is never called, the bootloader
    /// will roll back to the previous partition after N failed boots.
    fn mark_valid(&mut self) -> Result<(), OtaError>;
}

/// Orchestrates the OTA update process.
///
/// Provides step-by-step methods so the caller can drive the async
/// HTTP download loop while this struct handles flash writes and
/// validation synchronously.
///
/// Generic over the flash implementation so the full flow can be
/// tested on host without ESP32 hardware.
pub struct OtaManager<F: FlashWriter> {
    flash: F,
    progress: OtaProgress,
    expected_size: u32,
}

impl<F: FlashWriter> OtaManager<F> {
    /// Create a new OTA manager with the given flash writer.
    pub fn new(flash: F) -> Self {
        Self {
            flash,
            progress: OtaProgress::new(0),
            expected_size: 0,
        }
    }

    /// Get a reference to the underlying flash writer.
    pub fn flash(&self) -> &F {
        &self.flash
    }

    /// Get a mutable reference to the underlying flash writer.
    pub fn flash_mut(&mut self) -> &mut F {
        &mut self.flash
    }

    /// Get the current update progress.
    pub fn progress(&self) -> &OtaProgress {
        &self.progress
    }

    /// Check if the current boot needs OTA validation and mark it valid.
    ///
    /// Call this after WiFi + MQTT connect succeeds on every boot.
    /// It's a no-op if there's no pending verification.
    pub fn confirm_boot_if_needed(&mut self) -> Result<bool, OtaError> {
        if self.flash.is_pending_verification() {
            self.flash.mark_valid()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Step 1: Parse the MQTT trigger and prepare flash for writing.
    ///
    /// Returns the parsed trigger so the caller can use the URL to
    /// start the HTTP download.
    pub fn begin_update<'a>(
        &mut self,
        trigger_json: &'a [u8],
    ) -> Result<OtaTrigger<'a>, OtaError> {
        // Reject if already in progress
        if self.progress.state == OtaState::Downloading
            || self.progress.state == OtaState::Finalizing
        {
            return Err(OtaError::AlreadyInProgress);
        }

        // Parse trigger
        let trigger =
            message::parse_trigger(trigger_json).map_err(|_| OtaError::InvalidTrigger)?;

        // Initialize progress
        self.expected_size = trigger.size;
        self.progress = OtaProgress::new(trigger.size);
        self.progress.state = OtaState::Downloading;

        // Prepare flash
        self.flash.begin(trigger.size, trigger.crc32).map_err(|e| {
            self.progress.state = OtaState::Failed;
            e
        })?;

        Ok(trigger)
    }

    /// Step 2: Write a downloaded chunk to flash.
    ///
    /// Call this for each chunk received from the HTTP response body.
    /// Returns the total bytes written so far.
    pub fn write_chunk(&mut self, data: &[u8]) -> Result<u32, OtaError> {
        if self.progress.state != OtaState::Downloading {
            return Err(OtaError::NotStarted);
        }

        self.flash.write_chunk(data).map_err(|e| {
            self.progress.state = OtaState::Failed;
            e
        })?;

        self.progress.bytes_written += data.len() as u32;
        Ok(self.progress.bytes_written)
    }

    /// Step 3: Validate size, verify CRC, and set the new partition as boot target.
    ///
    /// Call this after the HTTP download is complete (reader returned 0 bytes).
    /// On success, returns an [`OtaAck`] to publish via MQTT before rebooting.
    pub fn finalize_update<'a>(
        &mut self,
        version: &'a str,
    ) -> Result<OtaAck<'a>, OtaError> {
        if self.progress.state != OtaState::Downloading {
            return Err(OtaError::NotStarted);
        }

        // Validate size
        if self.progress.bytes_written != self.expected_size {
            self.progress.state = OtaState::Failed;
            return Err(OtaError::SizeMismatch {
                got: self.progress.bytes_written,
                expected: self.expected_size,
            });
        }

        // Finalize (CRC check + set boot target)
        self.progress.state = OtaState::Finalizing;
        self.flash.finalize().map_err(|e| {
            self.progress.state = OtaState::Failed;
            e
        })?;

        self.progress.state = OtaState::Complete;

        Ok(OtaAck {
            status: OtaStatus::Success,
            version,
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// Mock flash writer that stores data in memory
    struct MockFlash {
        data: Vec<u8>,
        begun: bool,
        finalized: bool,
        pending_verification: bool,
        validated: bool,
        fail_on_write: bool,
        fail_on_finalize: bool,
        last_size: u32,
        last_crc32: u32,
    }

    impl MockFlash {
        fn new() -> Self {
            Self {
                data: Vec::new(),
                begun: false,
                finalized: false,
                pending_verification: false,
                validated: false,
                fail_on_write: false,
                fail_on_finalize: false,
                last_size: 0,
                last_crc32: 0,
            }
        }

        fn with_pending_verification() -> Self {
            Self {
                pending_verification: true,
                ..Self::new()
            }
        }
    }

    impl FlashWriter for MockFlash {
        fn begin(&mut self, size: u32, crc32: u32) -> Result<(), OtaError> {
            self.data.clear();
            self.begun = true;
            self.finalized = false;
            self.last_size = size;
            self.last_crc32 = crc32;
            Ok(())
        }

        fn write_chunk(&mut self, data: &[u8]) -> Result<(), OtaError> {
            if self.fail_on_write {
                return Err(OtaError::FlashWriteError);
            }
            self.data.extend_from_slice(data);
            Ok(())
        }

        fn finalize(&mut self) -> Result<(), OtaError> {
            if self.fail_on_finalize {
                return Err(OtaError::FinalizeError);
            }
            self.finalized = true;
            Ok(())
        }

        fn is_pending_verification(&self) -> bool {
            self.pending_verification
        }

        fn mark_valid(&mut self) -> Result<(), OtaError> {
            self.validated = true;
            self.pending_verification = false;
            Ok(())
        }
    }

    fn make_trigger_json(url: &str, version: &str, size: u32) -> Vec<u8> {
        std::format!(
            r#"{{"url":"{}","version":"{}","size":{},"crc32":0}}"#,
            url, version, size
        )
        .into_bytes()
    }

    #[test]
    fn test_successful_update() {
        let firmware = vec![0xAB; 1024];
        let trigger_json = make_trigger_json("https://fw.example.com/v1.bin", "1.0.0", 1024);

        let flash = MockFlash::new();
        let mut mgr = OtaManager::new(flash);

        // Step 1: begin
        let trigger = mgr.begin_update(&trigger_json).unwrap();
        assert_eq!(trigger.version, "1.0.0");
        assert_eq!(trigger.url, "https://fw.example.com/v1.bin");

        // Step 2: write chunks (simulate 4KB chunked download)
        for chunk in firmware.chunks(512) {
            mgr.write_chunk(chunk).unwrap();
        }

        // Step 3: finalize
        let ack = mgr.finalize_update(trigger.version).unwrap();
        assert_eq!(ack.status, OtaStatus::Success);
        assert_eq!(ack.version, "1.0.0");

        assert_eq!(mgr.progress().state, OtaState::Complete);
        assert_eq!(mgr.progress().bytes_written, 1024);
        assert_eq!(mgr.progress().percent(), 100);

        assert!(mgr.flash().begun);
        assert!(mgr.flash().finalized);
        assert_eq!(mgr.flash().data, firmware);
        assert_eq!(mgr.flash().last_size, 1024);
    }

    #[test]
    fn test_size_mismatch() {
        let trigger_json = make_trigger_json("https://example.com/fw.bin", "1.0.0", 1024);
        let flash = MockFlash::new();
        let mut mgr = OtaManager::new(flash);

        let trigger = mgr.begin_update(&trigger_json).unwrap();

        // Only write 512 bytes instead of 1024
        mgr.write_chunk(&[0xAB; 512]).unwrap();

        let err = mgr.finalize_update(trigger.version).unwrap_err();
        assert_eq!(
            err,
            OtaError::SizeMismatch {
                got: 512,
                expected: 1024
            }
        );
        assert_eq!(mgr.progress().state, OtaState::Failed);
    }

    #[test]
    fn test_invalid_trigger() {
        let flash = MockFlash::new();
        let mut mgr = OtaManager::new(flash);

        let err = mgr.begin_update(b"not json").unwrap_err();
        assert_eq!(err, OtaError::InvalidTrigger);
    }

    #[test]
    fn test_flash_write_failure() {
        let trigger_json = make_trigger_json("https://example.com/fw.bin", "1.0.0", 100);
        let mut flash = MockFlash::new();
        flash.fail_on_write = true;

        let mut mgr = OtaManager::new(flash);
        mgr.begin_update(&trigger_json).unwrap();

        let err = mgr.write_chunk(&[0xAB; 100]).unwrap_err();
        assert_eq!(err, OtaError::FlashWriteError);
        assert_eq!(mgr.progress().state, OtaState::Failed);
    }

    #[test]
    fn test_finalize_failure() {
        let trigger_json = make_trigger_json("https://example.com/fw.bin", "1.0.0", 100);
        let mut flash = MockFlash::new();
        flash.fail_on_finalize = true;

        let mut mgr = OtaManager::new(flash);
        let trigger = mgr.begin_update(&trigger_json).unwrap();
        mgr.write_chunk(&[0xAB; 100]).unwrap();

        let err = mgr.finalize_update(trigger.version).unwrap_err();
        assert_eq!(err, OtaError::FinalizeError);
        assert_eq!(mgr.progress().state, OtaState::Failed);
    }

    #[test]
    fn test_large_firmware_multiple_chunks() {
        let firmware = vec![0xCD; DEFAULT_CHUNK_SIZE * 3 + 500];
        let size = firmware.len() as u32;
        let trigger_json = make_trigger_json("https://example.com/fw.bin", "2.0.0", size);

        let flash = MockFlash::new();
        let mut mgr = OtaManager::new(flash);

        let trigger = mgr.begin_update(&trigger_json).unwrap();

        for chunk in firmware.chunks(DEFAULT_CHUNK_SIZE) {
            mgr.write_chunk(chunk).unwrap();
        }

        let ack = mgr.finalize_update(trigger.version).unwrap();
        assert_eq!(ack.status, OtaStatus::Success);
        assert_eq!(mgr.flash().data, firmware);
        assert_eq!(mgr.progress().bytes_written, size);
    }

    #[test]
    fn test_confirm_boot_when_pending() {
        let flash = MockFlash::with_pending_verification();
        let mut mgr = OtaManager::new(flash);

        let confirmed = mgr.confirm_boot_if_needed().unwrap();
        assert!(confirmed);
        assert!(mgr.flash().validated);
        assert!(!mgr.flash().pending_verification);
    }

    #[test]
    fn test_confirm_boot_when_not_pending() {
        let flash = MockFlash::new();
        let mut mgr = OtaManager::new(flash);

        let confirmed = mgr.confirm_boot_if_needed().unwrap();
        assert!(!confirmed);
        assert!(!mgr.flash().validated);
    }

    #[test]
    fn test_ack_json_from_successful_update() {
        let trigger_json = make_trigger_json("https://example.com/fw.bin", "3.1.0", 50);

        let flash = MockFlash::new();
        let mut mgr = OtaManager::new(flash);

        let trigger = mgr.begin_update(&trigger_json).unwrap();
        mgr.write_chunk(&[0x01; 50]).unwrap();
        let ack = mgr.finalize_update(trigger.version).unwrap();

        let mut buf = [0u8; 128];
        let len = ack.write_json(&mut buf).unwrap();
        let json = core::str::from_utf8(&buf[..len]).unwrap();
        assert_eq!(json, r#"{"ota_status":"success","version":"3.1.0"}"#);
    }

    #[test]
    fn test_progress_tracking() {
        let trigger_json = make_trigger_json("https://example.com/fw.bin", "1.0.0", 1000);
        let flash = MockFlash::new();
        let mut mgr = OtaManager::new(flash);

        assert_eq!(mgr.progress().state, OtaState::Idle);

        let trigger = mgr.begin_update(&trigger_json).unwrap();
        assert_eq!(mgr.progress().state, OtaState::Downloading);
        assert_eq!(mgr.progress().percent(), 0);

        mgr.write_chunk(&[0xFF; 500]).unwrap();
        assert_eq!(mgr.progress().percent(), 50);

        mgr.write_chunk(&[0xFF; 500]).unwrap();
        assert_eq!(mgr.progress().percent(), 100);

        mgr.finalize_update(trigger.version).unwrap();
        assert_eq!(mgr.progress().state, OtaState::Complete);
    }

    #[test]
    fn test_write_before_begin() {
        let flash = MockFlash::new();
        let mut mgr = OtaManager::new(flash);

        let err = mgr.write_chunk(&[0xFF; 100]).unwrap_err();
        assert_eq!(err, OtaError::NotStarted);
    }

    #[test]
    fn test_finalize_before_begin() {
        let flash = MockFlash::new();
        let mut mgr = OtaManager::new(flash);

        let err = mgr.finalize_update("1.0.0").unwrap_err();
        assert_eq!(err, OtaError::NotStarted);
    }

    #[test]
    fn test_begin_passes_size_and_crc() {
        let trigger_json = br#"{"url":"https://example.com/fw.bin","version":"1.0.0","size":5000,"crc32":12345}"#;
        let flash = MockFlash::new();
        let mut mgr = OtaManager::new(flash);

        mgr.begin_update(trigger_json).unwrap();
        assert_eq!(mgr.flash().last_size, 5000);
        assert_eq!(mgr.flash().last_crc32, 12345);
    }
}
