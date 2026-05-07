//! Compile-time-embedded AES-256 key for encrypted OTA firmware.
//!
//! The key file path is taken from the `OTA_AES_KEY_PATH` env var
//! (validated by `build.rs` to exist and contain 64 hex chars).
//! The file contents are embedded into the firmware via `include_str!`
//! at compile time — same pattern as `OTA_CA_CERT_PATH`.

const OTA_AES_KEY_HEX: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/",
    env!("OTA_AES_KEY_PATH")
));

/// Return the 32-byte AES-256 key embedded in this firmware.
pub fn ota_aes_key() -> [u8; 32] {
    let trimmed = OTA_AES_KEY_HEX.trim();
    let bytes = trimmed.as_bytes();
    debug_assert_eq!(bytes.len(), 64, "build.rs should have validated length");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (hex_nibble(bytes[i * 2]) << 4) | hex_nibble(bytes[i * 2 + 1]);
        i += 1;
    }
    out
}

const fn hex_nibble(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => panic!("non-hex char in OTA AES key (build.rs should have rejected this)"),
    }
}
