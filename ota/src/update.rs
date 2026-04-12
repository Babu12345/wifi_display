//! Core OTA update orchestration
//!
//! All hardware interaction is abstracted behind traits:
//! - [`FlashWriter`]: partition discovery, chunk writing, finalization
//! - [`HttpClient`]: HTTPS GET streaming
//!
//! [`OtaManager`] coordinates the full download-write-finalize flow.

use crate::message::{self, OtaAck, OtaStatus};
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
}

/// Abstraction over OTA flash partition operations.
///
/// On ESP32, this maps to `esp-ota` APIs. For testing, use a mock
/// that writes to an in-memory buffer.
pub trait FlashWriter {
    /// Begin an OTA session targeting the inactive partition.
    ///
    /// Called once before any writes. Implementations should find
    /// the inactive OTA slot and prepare it for writing.
    fn begin(&mut self) -> Result<(), OtaError>;

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

/// Abstraction over HTTPS GET downloads.
///
/// On ESP32, this uses `esp-mbedtls` over a TCP socket. For testing,
/// use a mock that yields data from an in-memory buffer.
pub trait HttpClient {
    /// Open an HTTPS GET connection to the given URL.
    ///
    /// Returns the content length reported by the server, if available.
    fn get(&mut self, url: &str) -> Result<Option<u32>, OtaError>;

    /// Read the next chunk of the response body into `buf`.
    ///
    /// Returns the number of bytes read, or 0 when the response is complete.
    fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize, OtaError>;
}

/// Orchestrates the OTA update process.
///
/// Generic over flash and HTTP implementations so the full update
/// flow can be tested on host without any ESP32 hardware.
pub struct OtaManager<F: FlashWriter, H: HttpClient> {
    flash: F,
    http: H,
    progress: OtaProgress,
}

impl<F: FlashWriter, H: HttpClient> OtaManager<F, H> {
    /// Create a new OTA manager with the given flash writer and HTTP client.
    pub fn new(flash: F, http: H) -> Self {
        Self {
            flash,
            http,
            progress: OtaProgress::new(0),
        }
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

    /// Perform a full OTA update from a raw MQTT trigger payload.
    ///
    /// This is the main entry point. It:
    /// 1. Parses the trigger JSON
    /// 2. Opens an HTTPS connection to download the firmware
    /// 3. Streams the response body in chunks, writing each to flash
    /// 4. Validates the total size
    /// 5. Finalizes the update (sets new boot partition)
    ///
    /// On success, returns an [`OtaAck`] with status `Success`.
    /// The caller should publish this via MQTT, then reboot.
    ///
    /// On failure, returns the error. The caller can build an error
    /// ACK from the trigger version if they still have it.
    pub fn perform_update<'a>(
        &mut self,
        trigger_json: &'a [u8],
    ) -> Result<OtaAck<'a>, OtaError> {
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
        self.progress = OtaProgress::new(trigger.size);
        self.progress.state = OtaState::Downloading;

        // Prepare flash
        self.flash.begin().map_err(|e| {
            self.progress.state = OtaState::Failed;
            e
        })?;

        // Start HTTP download
        let _content_length = self.http.get(trigger.url).map_err(|e| {
            self.progress.state = OtaState::Failed;
            e
        })?;

        // Stream chunks to flash
        let mut chunk_buf = [0u8; DEFAULT_CHUNK_SIZE];
        loop {
            let n = self.http.read_chunk(&mut chunk_buf).map_err(|e| {
                self.progress.state = OtaState::Failed;
                e
            })?;

            if n == 0 {
                break;
            }

            self.flash.write_chunk(&chunk_buf[..n]).map_err(|e| {
                self.progress.state = OtaState::Failed;
                e
            })?;

            self.progress.bytes_written += n as u32;
        }

        // Validate size
        if self.progress.bytes_written != trigger.size {
            self.progress.state = OtaState::Failed;
            return Err(OtaError::SizeMismatch {
                got: self.progress.bytes_written,
                expected: trigger.size,
            });
        }

        // Finalize
        self.progress.state = OtaState::Finalizing;
        self.flash.finalize().map_err(|e| {
            self.progress.state = OtaState::Failed;
            e
        })?;

        self.progress.state = OtaState::Complete;

        Ok(OtaAck {
            status: OtaStatus::Success,
            version: trigger.version,
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
        fn begin(&mut self) -> Result<(), OtaError> {
            self.data.clear();
            self.begun = true;
            self.finalized = false;
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

    /// Mock HTTP client that serves data from an in-memory buffer
    struct MockHttp {
        response_data: Vec<u8>,
        cursor: usize,
        fail_on_connect: bool,
    }

    impl MockHttp {
        fn new(data: Vec<u8>) -> Self {
            Self {
                response_data: data,
                cursor: 0,
                fail_on_connect: false,
            }
        }
    }

    impl HttpClient for MockHttp {
        fn get(&mut self, _url: &str) -> Result<Option<u32>, OtaError> {
            if self.fail_on_connect {
                return Err(OtaError::HttpError);
            }
            self.cursor = 0;
            Ok(Some(self.response_data.len() as u32))
        }

        fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize, OtaError> {
            let remaining = self.response_data.len() - self.cursor;
            if remaining == 0 {
                return Ok(0);
            }
            let n = remaining.min(buf.len());
            buf[..n].copy_from_slice(&self.response_data[self.cursor..self.cursor + n]);
            self.cursor += n;
            Ok(n)
        }
    }

    fn make_trigger_json(url: &str, version: &str, size: u32) -> Vec<u8> {
        std::format!(
            r#"{{"url":"{}","version":"{}","size":{}}}"#,
            url, version, size
        )
        .into_bytes()
    }

    #[test]
    fn test_successful_update() {
        let firmware = vec![0xAB; 1024];
        let trigger = make_trigger_json("https://fw.example.com/v1.bin", "1.0.0", 1024);

        let flash = MockFlash::new();
        let http = MockHttp::new(firmware.clone());
        let mut mgr = OtaManager::new(flash, http);

        let ack = mgr.perform_update(&trigger).unwrap();
        assert_eq!(ack.status, OtaStatus::Success);
        assert_eq!(ack.version, "1.0.0");

        assert_eq!(mgr.progress().state, OtaState::Complete);
        assert_eq!(mgr.progress().bytes_written, 1024);
        assert_eq!(mgr.progress().percent(), 100);

        assert!(mgr.flash.begun);
        assert!(mgr.flash.finalized);
        assert_eq!(mgr.flash.data, firmware);
    }

    #[test]
    fn test_size_mismatch() {
        // Server sends 512 bytes but trigger says 1024
        let firmware = vec![0xAB; 512];
        let trigger = make_trigger_json("https://example.com/fw.bin", "1.0.0", 1024);

        let flash = MockFlash::new();
        let http = MockHttp::new(firmware);
        let mut mgr = OtaManager::new(flash, http);

        let err = mgr.perform_update(&trigger).unwrap_err();
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
        let http = MockHttp::new(vec![]);
        let mut mgr = OtaManager::new(flash, http);

        let err = mgr.perform_update(b"not json").unwrap_err();
        assert_eq!(err, OtaError::InvalidTrigger);
    }

    #[test]
    fn test_http_connection_failure() {
        let trigger = make_trigger_json("https://example.com/fw.bin", "1.0.0", 100);
        let flash = MockFlash::new();
        let mut http = MockHttp::new(vec![]);
        http.fail_on_connect = true;

        let mut mgr = OtaManager::new(flash, http);
        let err = mgr.perform_update(&trigger).unwrap_err();
        assert_eq!(err, OtaError::HttpError);
        assert_eq!(mgr.progress().state, OtaState::Failed);
    }

    #[test]
    fn test_flash_write_failure() {
        let firmware = vec![0xAB; 100];
        let trigger = make_trigger_json("https://example.com/fw.bin", "1.0.0", 100);
        let mut flash = MockFlash::new();
        flash.fail_on_write = true;
        let http = MockHttp::new(firmware);

        let mut mgr = OtaManager::new(flash, http);
        let err = mgr.perform_update(&trigger).unwrap_err();
        assert_eq!(err, OtaError::FlashWriteError);
        assert_eq!(mgr.progress().state, OtaState::Failed);
    }

    #[test]
    fn test_finalize_failure() {
        let firmware = vec![0xAB; 100];
        let trigger = make_trigger_json("https://example.com/fw.bin", "1.0.0", 100);
        let mut flash = MockFlash::new();
        flash.fail_on_finalize = true;
        let http = MockHttp::new(firmware);

        let mut mgr = OtaManager::new(flash, http);
        let err = mgr.perform_update(&trigger).unwrap_err();
        assert_eq!(err, OtaError::FinalizeError);
        assert_eq!(mgr.progress().state, OtaState::Failed);
    }

    #[test]
    fn test_large_firmware_multiple_chunks() {
        // Firmware larger than DEFAULT_CHUNK_SIZE to exercise chunked reading
        let firmware = vec![0xCD; DEFAULT_CHUNK_SIZE * 3 + 500];
        let size = firmware.len() as u32;
        let trigger = make_trigger_json("https://example.com/fw.bin", "2.0.0", size);

        let flash = MockFlash::new();
        let http = MockHttp::new(firmware.clone());
        let mut mgr = OtaManager::new(flash, http);

        let ack = mgr.perform_update(&trigger).unwrap();
        assert_eq!(ack.status, OtaStatus::Success);
        assert_eq!(mgr.flash.data, firmware);
        assert_eq!(mgr.progress().bytes_written, size);
    }

    #[test]
    fn test_confirm_boot_when_pending() {
        let flash = MockFlash::with_pending_verification();
        let http = MockHttp::new(vec![]);
        let mut mgr = OtaManager::new(flash, http);

        let confirmed = mgr.confirm_boot_if_needed().unwrap();
        assert!(confirmed);
        assert!(mgr.flash.validated);
        assert!(!mgr.flash.pending_verification);
    }

    #[test]
    fn test_confirm_boot_when_not_pending() {
        let flash = MockFlash::new();
        let http = MockHttp::new(vec![]);
        let mut mgr = OtaManager::new(flash, http);

        let confirmed = mgr.confirm_boot_if_needed().unwrap();
        assert!(!confirmed);
        assert!(!mgr.flash.validated);
    }

    #[test]
    fn test_ack_json_from_successful_update() {
        let firmware = vec![0x01; 50];
        let trigger = make_trigger_json("https://example.com/fw.bin", "3.1.0", 50);

        let flash = MockFlash::new();
        let http = MockHttp::new(firmware);
        let mut mgr = OtaManager::new(flash, http);

        let ack = mgr.perform_update(&trigger).unwrap();
        let mut buf = [0u8; 128];
        let len = ack.write_json(&mut buf).unwrap();
        let json = core::str::from_utf8(&buf[..len]).unwrap();
        assert_eq!(json, r#"{"ota_status":"success","version":"3.1.0"}"#);
    }

    #[test]
    fn test_progress_during_download() {
        let firmware = vec![0xFF; 8192]; // 2 full chunks
        let trigger = make_trigger_json("https://example.com/fw.bin", "1.0.0", 8192);

        let flash = MockFlash::new();
        let http = MockHttp::new(firmware);
        let mut mgr = OtaManager::new(flash, http);

        // Before update
        assert_eq!(mgr.progress().state, OtaState::Idle);

        let _ = mgr.perform_update(&trigger).unwrap();

        // After update
        assert_eq!(mgr.progress().state, OtaState::Complete);
        assert_eq!(mgr.progress().percent(), 100);
    }
}
