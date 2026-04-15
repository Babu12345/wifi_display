//! Hardware-agnostic OTA firmware update crate
//!
//! Core logic for over-the-air updates: message parsing, flash writing
//! orchestration, and validation. Flash interaction goes through the
//! [`FlashWriter`] trait; HTTP download is caller-driven (async).
//!
//! ```text
//! // 1. Parse trigger and prepare flash
//! let trigger = mgr.begin_update(mqtt_payload)?;
//!
//! // 2. Caller drives async HTTP download
//! let url = ota::url::parse_https_url(trigger.url)?;
//! loop {
//!     let n = http_read(&mut buf).await?;
//!     if n == 0 { break; }
//!     mgr.write_chunk(&buf[..n])?;
//! }
//!
//! // 3. Finalize (CRC verify + set boot target)
//! let ack = mgr.finalize_update(trigger.version)?;
//! ```
#![no_std]
#![deny(missing_docs)]

mod message;
mod state;
mod update;
/// URL parsing for firmware download endpoints
pub mod url;

pub use message::{parse_trigger, OtaAck, OtaStatus, OtaTrigger};
pub use state::{OtaProgress, OtaState};
pub use update::{OtaError, OtaManager, FlashWriter, DEFAULT_CHUNK_SIZE};
