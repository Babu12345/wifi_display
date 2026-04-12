//! OTA update state tracking
//!
//! Tracks progress through the update lifecycle so callers can
//! report status or make decisions (e.g., skip duplicate triggers).

/// Current state of an OTA update
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaState {
    /// No update in progress
    Idle,
    /// Downloading firmware from URL
    Downloading,
    /// Download complete, finalizing flash
    Finalizing,
    /// Update written and set as boot target, ready to reboot
    Complete,
    /// Update failed
    Failed,
}

/// Progress information during an OTA download
#[derive(Debug, Clone, Copy)]
pub struct OtaProgress {
    /// Bytes written to flash so far
    pub bytes_written: u32,
    /// Total expected bytes
    pub total_bytes: u32,
    /// Current state
    pub state: OtaState,
}

impl OtaProgress {
    /// Create a new progress tracker for an update of `total_bytes`
    pub fn new(total_bytes: u32) -> Self {
        Self {
            bytes_written: 0,
            total_bytes,
            state: OtaState::Idle,
        }
    }

    /// Percentage complete (0-100)
    pub fn percent(&self) -> u8 {
        if self.total_bytes == 0 {
            return 0;
        }
        let pct = (self.bytes_written as u64 * 100) / self.total_bytes as u64;
        pct.min(100) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_percent() {
        let mut p = OtaProgress::new(1000);
        assert_eq!(p.percent(), 0);

        p.bytes_written = 500;
        assert_eq!(p.percent(), 50);

        p.bytes_written = 1000;
        assert_eq!(p.percent(), 100);
    }

    #[test]
    fn test_progress_percent_zero_total() {
        let p = OtaProgress::new(0);
        assert_eq!(p.percent(), 0);
    }

    #[test]
    fn test_progress_percent_overflow_clamped() {
        let mut p = OtaProgress::new(100);
        p.bytes_written = 200; // more than total
        assert_eq!(p.percent(), 100);
    }

    #[test]
    fn test_initial_state() {
        let p = OtaProgress::new(5000);
        assert_eq!(p.state, OtaState::Idle);
        assert_eq!(p.bytes_written, 0);
        assert_eq!(p.total_bytes, 5000);
    }
}
