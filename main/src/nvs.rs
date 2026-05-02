//! Non-volatile storage helpers — every flash-backed load/save lives here.
//!
//! Each function wraps a single `StorageContents` region from the `storage`
//! crate (address map in `storage/src/storage.rs`). Encodings: single-byte
//! flags direct, multi-byte integers little-endian, complex data via
//! bincode / `NFCData::{to,from}_bytes` / `MqttTopicsData::{to,from}_bytes`.
//! 0xFF in the first byte is treated as "uninitialized" (flash erase value).

use core::str::FromStr;

use crate::AppFlashStorage as FlashStorage;
use heapless::{String, Vec};
use nfc::{MAX_NFCDATA_SIZE, NFCData};
use storage::storage::{PersistentStorage, StorageContents};

// =============================================================================
// Display mode
// =============================================================================

/// Display mode for the main task — persisted so the device comes back in the
/// same mode after a reboot.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum DisplayMode {
    /// Live secure updates via MQTT
    LiveUpdates = 0x00,
    /// Display custom text from NFC (for low bandwidth)
    CustomText = 0x01,
    /// Display QR code from URL via NFC (for low bandwidth)
    QRCode = 0x02,
}

impl DisplayMode {
    fn as_byte(self) -> u8 {
        self as u8
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x00 => Some(DisplayMode::LiveUpdates),
            0x01 => Some(DisplayMode::CustomText),
            0x02 => Some(DisplayMode::QRCode),
            _ => None,
        }
    }
}

/// Load display mode. Returns `LiveUpdates` if unset (0xFF) or invalid.
pub fn load_display_mode() -> DisplayMode {
    let mut storage_buf = [0u8; 4];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.read(StorageContents::DisplayMode) {
        Ok(data) => match DisplayMode::from_byte(data[0]) {
            Some(mode) => {
                log::info!("Loaded display mode: {:?}", mode);
                mode
            }
            None => {
                log::info!("No saved display mode, defaulting to LiveUpdates");
                DisplayMode::LiveUpdates
            }
        },
        Err(e) => {
            log::error!("Failed to read display mode: {:?}", e);
            DisplayMode::LiveUpdates
        }
    }
}

/// Persist the display mode. Returns the argument so callers can write
/// `display_mode = nvs::set_display_mode(DisplayMode::CustomText);`.
pub fn set_display_mode(mode: DisplayMode) -> DisplayMode {
    let mut storage_buf = [0u8; 4];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.write_bytes(StorageContents::DisplayMode, 0, &[mode.as_byte()]) {
        Ok(_) => log::info!("Saved display mode: {:?}", mode),
        Err(e) => log::error!("Failed to write display mode: {:?}", e),
    }

    mode
}

// =============================================================================
// NFC-written data (caller supplies the buffer via PersistentStorage)
// =============================================================================

/// Load WiFi credentials written by the NFC task.
pub fn load_wifi_credentials(
    storage: &mut PersistentStorage<FlashStorage>,
) -> Result<(String<32>, String<64>), &'static str> {
    let data = storage
        .read(StorageContents::WifiCredentials)
        .map_err(|_| "Failed to read WiFi credentials from storage")?;

    let wifi_data = NFCData::from_bytes(data).map_err(|_| "Failed to parse WiFi credentials")?;

    match wifi_data {
        NFCData::Wifi(ssid_data, password_data) => {
            let ssid = String::from_str(ssid_data.as_str())
                .map_err(|_| "Failed to convert SSID to String")?;
            let password = String::from_str(password_data.as_str())
                .map_err(|_| "Failed to convert password to String")?;
            Ok((ssid, password))
        }
        _ => Err("Storage contains non-WiFi credential data"),
    }
}

/// Load custom display text written by the NFC task.
pub fn load_display_text(
    storage: &mut PersistentStorage<FlashStorage>,
) -> Result<String<MAX_NFCDATA_SIZE>, &'static str> {
    let data = storage
        .read(StorageContents::DisplayText)
        .map_err(|_| "Failed to read display text from storage")?;

    let text_data = NFCData::from_bytes(data).map_err(|_| "Failed to parse display text")?;

    match text_data {
        NFCData::Text(text) => String::from_str(text.as_str())
            .map_err(|_| "Failed to convert text to String or text too long"),
        _ => Err("Storage contains non-text data"),
    }
}

/// Load custom display URL written by the NFC task.
pub fn load_display_url(
    storage: &mut PersistentStorage<FlashStorage>,
) -> Result<String<MAX_NFCDATA_SIZE>, &'static str> {
    let data = storage
        .read(StorageContents::DisplayURL)
        .map_err(|_| "Failed to read display URL from storage")?;

    let url_data = NFCData::from_bytes(data).map_err(|_| "Failed to parse display URL")?;

    match url_data {
        NFCData::Uri(url) => String::from_str(url.as_str())
            .map_err(|_| "Failed to convert URL to String or URL too long"),
        _ => Err("Storage contains non-URL data"),
    }
}

// =============================================================================
// MQTT topic subscriptions
// =============================================================================

/// Maximum number of dynamic topic subscriptions remembered across reboots.
pub const MAX_DYNAMIC_TOPICS: usize = 4;

/// Flash region allocated for serialized topics (must fit bincode output).
const MQTT_TOPICS_STORAGE_SIZE: usize = 512;

#[derive(serde::Serialize, serde::Deserialize)]
struct MqttTopicsData {
    topics: Vec<String<64>, MAX_DYNAMIC_TOPICS>,
}

impl MqttTopicsData {
    fn to_bytes(&self, out: &mut [u8]) -> Result<usize, &'static str> {
        bincode::serde::encode_into_slice(self, out, bincode::config::standard())
            .map_err(|_| "Failed to serialize MQTT topics")
    }

    fn from_bytes(payload: &[u8]) -> Result<Self, &'static str> {
        bincode::serde::decode_from_slice(payload, bincode::config::standard())
            .map(|(data, _)| data)
            .map_err(|_| "Failed to deserialize MQTT topics")
    }
}

/// Load the persisted dynamic MQTT topics. Returns empty on uninitialized flash.
pub fn load_mqtt_topics() -> Vec<String<64>, MAX_DYNAMIC_TOPICS> {
    let mut storage_buf = [0u8; MQTT_TOPICS_STORAGE_SIZE];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.read(StorageContents::MqttTopics) {
        Ok(data) => {
            if data[0] == 0xFF {
                log::info!("No saved MQTT topics found");
                return Vec::new();
            }

            match MqttTopicsData::from_bytes(data) {
                Ok(mqtt_data) => {
                    log::info!("Loaded {} MQTT topics from storage", mqtt_data.topics.len());
                    mqtt_data.topics
                }
                Err(e) => {
                    log::warn!("Failed to parse MQTT topics: {}", e);
                    Vec::new()
                }
            }
        }
        Err(e) => {
            log::error!("Failed to read MQTT topics: {:?}", e);
            Vec::new()
        }
    }
}

/// Save the current set of dynamic MQTT topics.
pub fn save_mqtt_topics(topics: &Vec<String<64>, MAX_DYNAMIC_TOPICS>) {
    let mut storage_buf = [0u8; MQTT_TOPICS_STORAGE_SIZE];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    let data = MqttTopicsData {
        topics: topics.clone(),
    };

    let mut encode_buf = [0u8; MQTT_TOPICS_STORAGE_SIZE];
    match data.to_bytes(&mut encode_buf) {
        Ok(len) => {
            match storage.write_bytes(StorageContents::MqttTopics, 0, &encode_buf[..len]) {
                Ok(_) => log::info!(
                    "Saved {} MQTT topics to storage ({} bytes)",
                    topics.len(),
                    len
                ),
                Err(e) => log::error!("Failed to write MQTT topics: {:?}", e),
            }
        }
        Err(e) => log::error!("Failed to serialize MQTT topics: {}", e),
    }
}

// =============================================================================
// Tunables
// =============================================================================

/// Load `min_update_interval` (seconds between display refreshes).
pub fn load_min_update_interval() -> Option<u32> {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.read(StorageContents::MinUpdateInterval) {
        Ok(data) => {
            if data[0] == 0xFF && data[1] == 0xFF && data[2] == 0xFF && data[3] == 0xFF {
                log::info!("No saved min_update_interval found");
                return None;
            }
            let interval = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            log::info!(
                "Loaded min_update_interval from storage: {} seconds",
                interval
            );
            Some(interval)
        }
        Err(e) => {
            log::error!("Failed to read min_update_interval: {:?}", e);
            None
        }
    }
}

/// Save `min_update_interval`.
pub fn save_min_update_interval(interval: u32) {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    let bytes = interval.to_le_bytes();
    match storage.write_bytes(StorageContents::MinUpdateInterval, 0, &bytes) {
        Ok(_) => log::info!("Saved min_update_interval to storage: {} seconds", interval),
        Err(e) => log::error!("Failed to write min_update_interval: {:?}", e),
    }
}

/// Load `max_cycles` (refreshes between a full e-ink wipe). A stored `0` is
/// treated as "unset" since zero would disable full refreshes entirely.
pub fn load_max_cycles() -> Option<u8> {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.read(StorageContents::MaxCyclesBeforeFullRefresh) {
        Ok(data) => {
            if data[0] == 0xFF && data[1] == 0xFF {
                log::info!("No saved max_cycles found");
                return None;
            }
            let cycles = u16::from_le_bytes([data[0], data[1]]);
            log::info!("Loaded max_cycles from storage: {}", cycles);
            if cycles == 0 {
                log::info!("Ignoring saved max_cycles of 0");
                return None;
            }
            Some(cycles as u8)
        }
        Err(e) => {
            log::error!("Failed to read max_cycles: {:?}", e);
            None
        }
    }
}

/// Save `max_cycles`. Stored as u16 little-endian.
pub fn save_max_cycles(cycles: u8) {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    let bytes = (cycles as u16).to_le_bytes();
    match storage.write_bytes(StorageContents::MaxCyclesBeforeFullRefresh, 0, &bytes) {
        Ok(_) => log::info!("Saved max_cycles to storage: {}", cycles),
        Err(e) => log::error!("Failed to write max_cycles: {:?}", e),
    }
}

/// Load the last successful update timestamp (Unix seconds) — used for rate
/// limiting across reconnects.
pub fn load_last_update_timestamp() -> Option<u64> {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.read(StorageContents::LastUpdateTimestamp) {
        Ok(data) => {
            if data[..8].iter().all(|b| *b == 0xFF) {
                log::info!("No saved last_update_timestamp found");
                return None;
            }
            let timestamp = u64::from_le_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ]);
            log::info!(
                "Loaded last_update_timestamp from storage: {} seconds",
                timestamp
            );
            Some(timestamp)
        }
        Err(e) => {
            log::error!("Failed to read last_update_timestamp: {:?}", e);
            None
        }
    }
}

/// Save the last successful update timestamp.
pub fn save_last_update_timestamp(timestamp_secs: u64) {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    let bytes = timestamp_secs.to_le_bytes();
    match storage.write_bytes(StorageContents::LastUpdateTimestamp, 0, &bytes) {
        Ok(_) => log::info!(
            "Saved last_update_timestamp to storage: {} seconds",
            timestamp_secs
        ),
        Err(e) => log::error!("Failed to write last_update_timestamp: {:?}", e),
    }
}

// =============================================================================
// WiFi error flag — "last paint on the e-ink was the disconnect screen"
// =============================================================================

/// Load the flag. Returns true iff the stored byte is `0x01`; `0xFF` (fresh
/// flash) or anything else is treated as false.
pub fn load_wifi_error_flag() -> bool {
    let mut storage_buf = [0u8; 4];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.read(StorageContents::WifiErrorFlag) {
        Ok(data) => data[0] == 0x01,
        Err(e) => {
            log::error!("Failed to read wifi error flag: {:?}", e);
            false
        }
    }
}

/// Persist whether the disconnect screen is currently the last thing painted.
pub fn save_wifi_error_flag(set: bool) {
    let mut storage_buf = [0u8; 4];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    let byte = if set { 0x01 } else { 0x00 };
    match storage.write_bytes(StorageContents::WifiErrorFlag, 0, &[byte]) {
        Ok(_) => log::info!("Saved wifi error flag: {}", set),
        Err(e) => log::error!("Failed to write wifi error flag: {:?}", e),
    }
}

/// Clear the flag only if currently set. Call after queuing any non-WiFi-
/// status paint so the flag tracks what's actually on the e-ink. Read-first
/// avoids redundant flash writes on every MQTT frame (~100k write endurance).
pub fn clear_wifi_error_flag_if_set() {
    if load_wifi_error_flag() {
        save_wifi_error_flag(false);
    }
}
