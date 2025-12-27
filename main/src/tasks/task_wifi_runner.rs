//! WiFi network stack runner

use embassy_net::Runner;
use esp_wifi::wifi::{WifiDevice, WifiStaDevice};

/// Runs the embassy-net network stack for WiFi
///
/// This task must be spawned for the network stack to process packets.
#[embassy_executor::task]
pub async fn task_wifi_runner(mut runner: Runner<'static, WifiDevice<'static, WifiStaDevice>>) {
    runner.run().await;
}
