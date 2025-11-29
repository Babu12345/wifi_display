//! Process the required runner task for the wifi hardware

use embassy_net::Runner;
use esp_wifi::wifi::{WifiDevice, WifiStaDevice};

#[embassy_executor::task]
/// Establishes a wifi connection
pub async fn task_wifi_runner(mut runner: Runner<'static, WifiDevice<'static, WifiStaDevice>>) {
    runner.run().await;
}
