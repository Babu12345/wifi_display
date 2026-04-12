//! OTA message types for MQTT communication
//!
//! Handles parsing the trigger message from the server and
//! formatting ACK/NACK responses.

use serde::Deserialize;

/// OTA trigger message received via MQTT on `{client_id}/root/ota`
///
/// Example payload:
/// ```json
/// {"url": "https://fw.example.com/v1.2.0/firmware.bin", "version": "1.2.0", "size": 1258000}
/// ```
#[derive(Deserialize, Debug, Clone)]
pub struct OtaTrigger<'a> {
    /// HTTPS URL to download the signed firmware binary
    #[serde(borrow)]
    pub url: &'a str,
    /// Human-readable version string
    #[serde(borrow)]
    pub version: &'a str,
    /// Expected firmware size in bytes
    pub size: u32,
}

/// OTA completion status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaStatus {
    /// Update downloaded and written successfully, ready to reboot
    Success,
    /// Update failed
    Error,
}

/// ACK message to publish back via MQTT after an OTA attempt
#[derive(Debug)]
pub struct OtaAck<'a> {
    /// Whether the update succeeded or failed
    pub status: OtaStatus,
    /// Version string from the trigger (for correlation)
    pub version: &'a str,
}

impl<'a> OtaAck<'a> {
    /// Format the ACK as a JSON byte string into the provided buffer.
    ///
    /// Returns the number of bytes written, or `None` if the buffer is too small.
    pub fn write_json(&self, buf: &mut [u8]) -> Option<usize> {
        let status_str = match self.status {
            OtaStatus::Success => "success",
            OtaStatus::Error => "error",
        };

        // Build JSON manually to avoid alloc: {"ota_status":"...","version":"..."}
        let mut pos = 0;
        let parts: &[&[u8]] = &[
            b"{\"ota_status\":\"",
            status_str.as_bytes(),
            b"\",\"version\":\"",
            self.version.as_bytes(),
            b"\"}",
        ];

        for part in parts {
            if pos + part.len() > buf.len() {
                return None;
            }
            buf[pos..pos + part.len()].copy_from_slice(part);
            pos += part.len();
        }

        Some(pos)
    }
}

/// Parse an OTA trigger message from raw JSON bytes (e.g., MQTT payload).
///
/// Handles payloads that may be null-terminated (common with MQTT buffers).
pub fn parse_trigger(json_data: &[u8]) -> Result<OtaTrigger<'_>, &'static str> {
    let json_end = json_data
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(json_data.len());

    let (parsed, _) = serde_json_core::from_slice::<OtaTrigger>(&json_data[..json_end])
        .map_err(|_| "Failed to parse OTA trigger JSON")?;

    if parsed.url.is_empty() {
        return Err("OTA trigger URL is empty");
    }
    if parsed.size == 0 {
        return Err("OTA trigger size is zero");
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn test_parse_trigger_valid() {
        let json = br#"{"url":"https://fw.example.com/v1.2.0/firmware.bin","version":"1.2.0","size":1258000}"#;
        let trigger = parse_trigger(json).unwrap();
        assert_eq!(trigger.url, "https://fw.example.com/v1.2.0/firmware.bin");
        assert_eq!(trigger.version, "1.2.0");
        assert_eq!(trigger.size, 1258000);
    }

    #[test]
    fn test_parse_trigger_null_terminated() {
        let json = *b"{\"url\":\"https://example.com/fw.bin\",\"version\":\"0.1\",\"size\":100}\0\0\0";
        let trigger = parse_trigger(&json).unwrap();
        assert_eq!(trigger.url, "https://example.com/fw.bin");
        assert_eq!(trigger.size, 100);
    }

    #[test]
    fn test_parse_trigger_empty_url() {
        let json = br#"{"url":"","version":"1.0","size":100}"#;
        let result = parse_trigger(json);
        assert_eq!(result.unwrap_err(), "OTA trigger URL is empty");
    }

    #[test]
    fn test_parse_trigger_zero_size() {
        let json = br#"{"url":"https://example.com/fw.bin","version":"1.0","size":0}"#;
        let result = parse_trigger(json);
        assert_eq!(result.unwrap_err(), "OTA trigger size is zero");
    }

    #[test]
    fn test_parse_trigger_invalid_json() {
        let json = b"{not json}";
        assert!(parse_trigger(json).is_err());
    }

    #[test]
    fn test_parse_trigger_missing_field() {
        let json = br#"{"url":"https://example.com/fw.bin","version":"1.0"}"#;
        assert!(parse_trigger(json).is_err());
    }

    #[test]
    fn test_ota_ack_success_json() {
        let ack = OtaAck {
            status: OtaStatus::Success,
            version: "1.2.0",
        };
        let mut buf = [0u8; 128];
        let len = ack.write_json(&mut buf).unwrap();
        let json = core::str::from_utf8(&buf[..len]).unwrap();
        assert_eq!(json, r#"{"ota_status":"success","version":"1.2.0"}"#);
    }

    #[test]
    fn test_ota_ack_error_json() {
        let ack = OtaAck {
            status: OtaStatus::Error,
            version: "2.0.0",
        };
        let mut buf = [0u8; 128];
        let len = ack.write_json(&mut buf).unwrap();
        let json = core::str::from_utf8(&buf[..len]).unwrap();
        assert_eq!(json, r#"{"ota_status":"error","version":"2.0.0"}"#);
    }

    #[test]
    fn test_ota_ack_buffer_too_small() {
        let ack = OtaAck {
            status: OtaStatus::Success,
            version: "1.0.0",
        };
        let mut buf = [0u8; 5]; // way too small
        assert!(ack.write_json(&mut buf).is_none());
    }
}
