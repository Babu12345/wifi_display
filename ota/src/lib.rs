//! Hardware-agnostic OTA firmware update crate
//!
//! Core logic for over-the-air updates: message parsing, download orchestration,
//! flash writing, and validation. All hardware interaction goes through traits,
//! making the entire update state machine testable on host.
#![no_std]
#![deny(missing_docs)]

mod message;
mod state;
mod update;

pub use message::{OtaAck, OtaStatus, OtaTrigger};
pub use state::{OtaState, OtaProgress};
pub use update::{OtaError, FlashWriter, HttpClient, OtaManager};
