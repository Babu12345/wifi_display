//! Stores tasks to be run concurrently in the main function

pub mod task_display_handler;
pub mod task_nfc;
pub mod task_wifi_runner;

/// Custom slice trait for trimming or extending slices
pub trait MatchSliceLengths<const N: usize> {
    /// Match size of the output
    fn match_size(self, padding: u8) -> [u8; N];
}

impl<const N: usize> MatchSliceLengths<N> for &[u8] {
    fn match_size(self, padding: u8) -> [u8; N] {
        let mut buffer = [padding; N];
        let array_size = self.len();
        if N >= array_size {
            buffer[..array_size].copy_from_slice(self);
            return buffer;
        }
        buffer.copy_from_slice(&self[..N]);
        buffer
    }
}

/// Full prefix for registration response data (REG:C:)
pub const REGISTRATION_PREFIX: &str = "REG:C:";
/// Suffix for registration response data
pub const REGISTRATION_SUFFIX: &str = ";;";

/// Format a registration code response for NFC exchange
/// Returns format: REG:C:<code>;; (similar to WIFI:S:ssid;P:password;;)
pub fn format_registration_response(code: &str) -> heapless::String<64> {
    let mut response = heapless::String::<64>::new();
    let _ = core::fmt::write(
        &mut response,
        format_args!("{}{}{}", REGISTRATION_PREFIX, code, REGISTRATION_SUFFIX),
    );
    response
}

/// Check if text is a registration response (not meant for display)
pub fn is_registration_response(text: &str) -> bool {
    text.starts_with(REGISTRATION_PREFIX) && text.ends_with(REGISTRATION_SUFFIX)
}
