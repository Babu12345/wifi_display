//! Shared and compartmentalized functions for the async_main

#![no_std]
#![deny(missing_docs)]

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

/// Initialize the ESP32 logger capbilities
pub fn initalize_logger() {
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
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Signal notification
pub enum NotificationType {
    /// Wifi credentials
    WifiCredentials,
    /// Text for displaying
    DisplayText,
    /// Display URL
    DisplayURL,
    /// Live updates
    LiveSecureUpdates,
}
