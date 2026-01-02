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
