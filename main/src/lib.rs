//! Shared and compartmentalized functions for the async_main

#![no_std]
#![deny(missing_docs)]

pub mod nvs;
pub mod ota_flash;
pub mod ota_http;
pub mod spi;
pub mod tasks;

use core::{
    future::poll_fn,
    task::{Context, Poll},
};

use embassy_net::{Stack, StaticConfigV4};
use esp_hal::{clock::CpuClock, peripherals::Peripherals};

/// Number of receivers for sending notifications
pub const NUM_NOTIFICATION_RECEIVERS: usize = 1;

/// Number of receivers for NFC data change counter
pub const NUM_NFC_CHANGE_RECEIVERS: usize = 1;

/// Initialize the ESP32 logger capabilities
/// In production mode, only warnings and errors are logged to save power
pub fn initalize_logger() {
    #[cfg(feature = "production")]
    esp_println::logger::init_logger(log::LevelFilter::Warn);

    #[cfg(not(feature = "production"))]
    esp_println::logger::init_logger(log::LevelFilter::Info);
}

/// Initialize the peripherals
pub fn initialize_peripherals() -> Peripherals {
    let peripherals = esp_hal::init({
        let mut config = esp_hal::Config::default();
        config.cpu_clock = CpuClock::_80MHz;
        config
    });
    peripherals
}

/// Read the chip's unique 48-bit eFuse MAC and format it as a 12-character
/// uppercase hex string (e.g. `"80F1B2ECB820"`). Used as the MQTT client ID
/// and registration code so every device is identifiable without any
/// per-device provisioning and OTA can ship one binary to the whole fleet.
pub fn device_client_id() -> heapless::String<12> {
    use core::fmt::Write;
    let mac = esp_hal::efuse::Efuse::read_base_mac_address();
    let mut s = heapless::String::<12>::new();
    for byte in &mac {
        let _ = write!(&mut s, "{:02X}", byte);
    }
    s
}

/// Extra functions (mainly async) for the stack
pub trait AsyncStack {
    /// Waiting for an uplink
    fn wait_for_uplink(&self) -> impl Future<Output = ()>;
    /// Waiting for a connection
    fn wait_for_ipaddress(&self) -> impl Future<Output = StaticConfigV4>;
}

impl<'stack> AsyncStack for Stack<'stack> {
    async fn wait_for_uplink(&self) {
        let check_fn = |cx: &mut Context<'_>| match self.is_link_up() {
            true => Poll::Ready(()),
            false => {
                cx.waker().wake_by_ref();
                Poll::<()>::Pending
            }
        };
        poll_fn(check_fn).await;
    }

    async fn wait_for_ipaddress(&self) -> StaticConfigV4 {
        let check_fn = |cx: &mut Context<'_>| match self.config_v4() {
            Some(config) => Poll::Ready(config),
            None => {
                cx.waker().wake_by_ref();
                Poll::<StaticConfigV4>::Pending
            }
        };
        poll_fn(check_fn).await
    }
}

#[macro_export]
/// Makes an object static even after the start of the program.
/// When you are okay with using a nightly compiler it's better to use https://docs.rs/static_cell/2.1.0/static_cell/macro.make_static.html
macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Signal notification
pub enum NotificationType {
    /// Wifi credentials
    WifiCredentials,
    /// Text for displaying (via NFC)
    DisplayText,
    /// Display URL (via NFC)
    DisplayURL,
    /// Live updates (via MQTT)
    LiveSecureUpdates,
}
