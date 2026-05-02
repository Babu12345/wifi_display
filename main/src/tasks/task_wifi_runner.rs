//! Main wifi processor

use core::ffi::CStr;

use serde::Serialize;

use crate::nvs::{self, DisplayMode, MAX_DYNAMIC_TOPICS};
use crate::tasks::task_display_handler::{
    DISPLAY_CHANNEL_SIZE, DisplayMessage, append_to_display_buffer, queue_frame_ready,
    queue_qr_display, queue_set_max_cycles, queue_text_display, queue_text_with_qr_display,
    reset_display_buffer,
};
use crate::{AsyncStack, NotificationType};
use crate::{NUM_NFC_CHANGE_RECEIVERS, NUM_NOTIFICATION_RECEIVERS};
use embassy_futures::select::{Either3, select, select3};
use embassy_net::{Runner, Stack};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, watch::Receiver};
use embassy_time::{Duration, Timer};
use esp_wifi::wifi::{WifiDevice, WifiStaDevice};
use rand_core::CryptoRngCore;

use core::cell::RefCell;
use esp_hal::peripherals;
use esp_hal::reset::reset_reason;
use esp_hal::rng::Trng;
use esp_hal::rtc_cntl::SocResetReason;
use crate::AppFlashStorage as FlashStorage;
use esp_wifi::wifi::{ClientConfiguration, Configuration, WifiController};
use heapless::String;
use nfc::MAX_NFCDATA_SIZE;
use storage::storage::PersistentStorage;
/// WIFI SSID
pub const DEFAULT_SSID: &str = "HONESTWIFI-2325-2G";
/// WIFI Password
pub const DEFAULT_PASSWORD: &str = "9526070855!";

// =============================================================================
// TIMING CONFIGURATION
// =============================================================================
// These values are tuned for power savings with ESP_WIFI_CONFIG_LISTEN_INTERVAL=10
// Increase delays if you see ImplementationSpecificError with higher listen intervals
// NOTE: If ImplementationSpecificError persists, also increase UNSUBSCRIBE_DELAY_MS
// in display_mqtt_utils layer

/// How long to wait after WiFi/MQTT errors before retrying
const RETRY_DELAY_SECS: u64 = 5;

/// Short delay for state transitions (WiFi stop/start, config changes)
const TRANSITION_DELAY_MS: u64 = 200;

/// Delay after processing config messages to allow radio to settle
const CONFIG_PROCESS_DELAY_MS: u64 = 50;

/// Main loop refresh interval when not connected to MQTT
const REFRESH_INTERVAL_SECS: u64 = 60;

/// MQTT connection and keep-alive timeout
const MQTT_TIMEOUT_SECS: u16 = 300;

/// Extra seconds added to socket timeout beyond keep-alive for headroom
const SOCKET_TIMEOUT_HEADROOM_SECS: u64 = 30;

/// MQTT ping interval — ping at 1/3 of keep-alive for reliability
static MQTT_PING_INTERVAL_SECS: u64 = (MQTT_TIMEOUT_SECS as u64) / 3;

/// Buffer time subtracted from rate limiting calculations
const RATE_LIMIT_BUFFER_SECS: u64 = 4;

// =============================================================================

const DEFAULT_QOS: rust_mqtt::packet::v5::publish_packet::QualityOfService =
    rust_mqtt::packet::v5::publish_packet::QualityOfService::QoS0;

/// WiFi disconnection error message displayed with QR code for support
const WIFI_DISCONNECTED_MSG: &str = "WiFi Disconnected\n\n\
1. Check router\n   is online\n\
2. Use 2.4GHz or dual-band supported network\n\
3. Check password\n\n\
Tap NFC on the back\n\
using the app to update or scan QR for help";

/// WiFi connected message showing available features
const WIFI_CONNECTED_MSG: &str = "WiFi Connected!\n\n\
Features:\n\
- Upload images\n\
- Display QR codes\n\
- Bible verses\n\
- Clock display\n\
- Weather updates\n\
- Stock prices\n\
- And more!\n\n\
Scan QR to start";

/// "Get started" landing page URL — shown on WiFi connect.
const GET_STARTED_URL: &str = env!("GET_STARTED_URL");

/// Support/help website URL — scanning opens it in the phone's browser.
const SUPPORT_URL: &str = env!("SUPPORT_URL");

/// App Store URL for Paper Portrait Connect — scanning opens the listing
/// (installs if new, launches App Store if already installed).
const APP_STORE_URL: &str = env!("APP_STORE_URL");

// Note: these are paired with a QR to GET_STARTED_URL / SUPPORT_URL on the
// right half of the display. Text rendering gets a 210 px column at
// Large10x20 (~21 chars/line), so keep lines short and rely on manual \n's
// rather than auto-wrap to avoid mid-word breaks.

/// Shown while firmware is downloading. Paired with the App Store QR so
/// users can share the app with friends while they wait.
const OTA_UPDATING_MSG: &str = "Installing\nupdate...\n\n\
Keep WiFi on\nand don't unplug.\n\n\
Scan to share\nwith a friend.";

/// Shown after a successful OTA boot. Paired with the App Store QR so
/// users can share the app with friends.
const OTA_COMPLETE_MSG: &str = "All set!\n\n\
Next display\ncoming soon,\nor refresh now\nfrom the app.\n\n\
Scan to share\nwith a friend.";

/// Shown if the OTA download or verify fails before reboot. Paired with
/// the help website QR.
const OTA_FAILED_MSG: &str = "Update didn't\nfinish.\n\n\
Your device is\nstill fine.\n\n\
Scan to visit\nour help site.";

const MQTT_BROKER: &str = env!("MQTT_BROKER");
/// MQTT broker as a CStr for TLS servername (requires null terminator)
const MQTT_BROKER_CSTR: &CStr = {
    const BYTES: &[u8] = concat!(env!("MQTT_BROKER"), "\0").as_bytes();
    match CStr::from_bytes_with_nul(BYTES) {
        Ok(s) => s,
        Err(_) => panic!("MQTT_BROKER contains interior null byte"),
    }
};
const MQTT_PORT: u16 = {
    let bytes = env!("MQTT_PORT").as_bytes();
    let mut result: u16 = 0;
    let mut i = 0;
    while i < bytes.len() {
        result = result * 10 + (bytes[i] - b'0') as u16;
        i += 1;
    }
    result
};
// MQTT client ID is derived from the chip's eFuse MAC at runtime (see
// `crate::device_client_id`) so every device is unique without per-device
// provisioning and a single OTA binary can serve the whole fleet.
/// Broadcast OTA topic: every device subscribes here so one MQTT publish can
/// hotfix the entire fleet. Not tied to any client ID. Uses the `public/*`
/// namespace which is already covered by the existing AWS IoT policy.
pub const OTA_BROADCAST_TOPIC: &str = "public/ota";
/// Max size in bytes of the data being sent via AWS
pub const MQTT_BUFFER_SIZE: usize = 7_000;
// Static buffers for MQTT to avoid stack overflow
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};

// Static buffers for MQTT protected by mutexes to prevent concurrent access
static MQTT_TCP_RX_BUFFER: Mutex<CriticalSectionRawMutex, [u8; MQTT_BUFFER_SIZE]> =
    Mutex::new([0u8; MQTT_BUFFER_SIZE]);
static MQTT_TCP_TX_BUFFER: Mutex<CriticalSectionRawMutex, [u8; MQTT_BUFFER_SIZE]> =
    Mutex::new([0u8; MQTT_BUFFER_SIZE]);

static CHUNK_META: Mutex<CriticalSectionRawMutex, ChunkMeta> = Mutex::new(ChunkMeta::new());

/// Pending OTA trigger payload, set by the MQTT handler and picked up by the
/// outer loop after the MQTT session is torn down (freeing TLS heap).
static PENDING_OTA: Mutex<CriticalSectionRawMutex, Option<([u8; 512], usize)>> = Mutex::new(None);

/// Reasons the MQTT handler can exit with an error.
#[derive(Debug, thiserror::Error)]
enum MqttSessionError {
    /// OTA trigger received — the caller should tear down TLS and run the
    /// download from the outer loop where the heap is free.
    #[error("OTA update requested")]
    OtaRequested,
    /// DNS resolution for the MQTT broker failed.
    #[error("DNS lookup failed: {0:?}")]
    DnsLookupFailed(embassy_net::dns::Error),
    /// TCP connection to the MQTT broker failed.
    #[error("TCP connect failed: {0:?}")]
    TcpConnectFailed(embassy_net::tcp::ConnectError),
    /// TLS session creation or handshake failed.
    #[error("TLS failed: {0:?}")]
    TlsFailed(esp_mbedtls::TlsError),
    /// MQTT broker rejected the connection.
    #[error("MQTT connect rejected: {0}")]
    MqttConnectFailed(rust_mqtt::packet::v5::reason_codes::ReasonCode),
    /// A topic subscription was rejected by the broker.
    #[error("subscription rejected: {0}")]
    SubscriptionFailed(rust_mqtt::packet::v5::reason_codes::ReasonCode),
    /// Topic name formatting overflowed the buffer.
    #[error("topic format error")]
    TopicFormatError,
    /// An error occurred while receiving or processing messages.
    #[error("MQTT receive error")]
    ReceiveError,
}

/// CA cert for OTA firmware downloads (Amazon Root CA 1 + Starfield G2,
/// covers ACM-issued certificates for www.paperportraitdisplay.com on
/// AWS Amplify Hosting).
const OTA_CA_CERT: &str = concat!(
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/",
        env!("OTA_CA_CERT_PATH")
    )),
    "\0"
);

/// Static decode buffer to avoid stack allocation on each chunk
static DECODE_BUF: Mutex<CriticalSectionRawMutex, [u8; MQTT_BUFFER_SIZE]> =
    Mutex::new([0u8; MQTT_BUFFER_SIZE]);

/// Chunk reassembly metadata (no buffer - uses display buffer directly)
struct ChunkMeta {
    total_chunks: usize,
    received_count: usize,
    offset: usize,
}

impl ChunkMeta {
    const fn new() -> Self {
        Self {
            total_chunks: 0,
            received_count: 0,
            offset: 0,
        }
    }

    fn reset(&mut self) {
        self.total_chunks = 0;
        self.received_count = 0;
        self.offset = 0;
    }

    fn is_complete(&self) -> bool {
        self.received_count > 0 && self.received_count == self.total_chunks
    }
}

/// Result of processing a chunk
struct ProcessChunkResult {
    /// Whether to send a response (ACK or error)
    send_response: bool,
    /// Whether the operation succeeded (true = success, false = error)
    success: bool,
    /// Whether to unsubscribe from all dynamic topics
    unsubscribe_all: bool,
    /// Unix timestamp from the chunk (if provided)
    timestamp: Option<u64>,
}

/// MQTT response status
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum MqttResponseStatus {
    Success,
    #[allow(unused)]
    Error,
}

/// Type-safe MQTT response message
#[derive(Debug, Clone, Copy, Serialize)]
struct MqttResponse {
    response: MqttResponseStatus,
}

/// Main application task - manages WiFi connection, MQTT communication, and display modes
///
/// Handles three display modes:
/// - LiveUpdates: Receives display frames via MQTT over TLS
/// - CustomText: Displays text stored via NFC (offline mode)
/// - QRCode: Displays QR codes from URLs stored via NFC (offline mode)
#[embassy_executor::task]
pub async fn task_wifi_runner(
    stack: Stack<'static>,
    mut runner: Runner<'static, WifiDevice<'static, WifiStaDevice>>,
    rng_ref: &'static RefCell<Trng<'static>>,
    controller: WifiController<'static>,
    notification: Receiver<'static, NoopRawMutex, NotificationType, NUM_NOTIFICATION_RECEIVERS>,
    nfc_change: Receiver<'static, NoopRawMutex, u32, NUM_NFC_CHANGE_RECEIVERS>,
    display_channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
    sha: peripherals::SHA,
    rsa: peripherals::RSA,
    client_id: &'static str,
) {
    // Spawn the network runner as a background future using join
    embassy_futures::join::join(
        runner.run(),
        task_wifi_runner_inner(
            stack,
            rng_ref,
            controller,
            notification,
            nfc_change,
            display_channel,
            sha,
            rsa,
            client_id,
        ),
    )
    .await;
}

async fn task_wifi_runner_inner(
    stack: Stack<'static>,
    rng_ref: &'static RefCell<Trng<'static>>,
    mut controller: WifiController<'static>,
    mut notification: Receiver<'static, NoopRawMutex, NotificationType, NUM_NOTIFICATION_RECEIVERS>,
    mut nfc_change: Receiver<'static, NoopRawMutex, u32, NUM_NFC_CHANGE_RECEIVERS>,
    display_channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
    sha: peripherals::SHA,
    rsa: peripherals::RSA,
    client_id: &str,
) {
    let tls = esp_mbedtls::Tls::new(sha).unwrap().with_hardware_rsa(rsa);

    let mut storage_data = [0u8; MAX_NFCDATA_SIZE];
    let mut storage = PersistentStorage::new(FlashStorage::default(), &mut storage_data);

    let mut ssid: Option<String<32>> = None;
    let mut password: Option<String<64>> = None;

    // If the last thing we painted was the disconnect screen (persisted across
    // reboots), start as !connected so the first successful connect re-paints
    // WIFI_CONNECTED_MSG instead of leaving the stale error on the e-ink.
    let mut previously_connected = !nvs::load_wifi_error_flag();
    let mut display_mode = nvs::load_display_mode();

    // Check reset reason. ChipPowerOn covers clean power-up, chip-level
    // brownout, and super-WDT (ROM reports all three as 0x01); SysBrownOut
    // covers the slow VDD-sag case. In any of these, the e-ink is still
    // showing real content, so we don't want the first failed connect to
    // flash WIFI_DISCONNECTED_MSG over it. Cleared after the first attempt so
    // a genuinely broken WiFi still reports an error on subsequent retries.
    let reason = reset_reason();
    log::info!("Reset reason: {:?}", reason);
    let mut skip_error_on_first_attempt = matches!(
        reason,
        Some(SocResetReason::SysBrownOut) | Some(SocResetReason::ChipPowerOn)
    );

    // Main loop - refresh every REFRESH_INTERVAL_SECS seconds
    'process: loop {
        // Check for NFC notifications:
        // 1. notification.try_changed() detects new notification types
        // 2. nfc_change counter detects same-type writes (e.g., two different texts)
        let pending_notification = notification.try_changed().or_else(|| {
            nfc_change
                .try_changed()
                .and_then(|_| notification.try_get())
        });

        let credentials_updated = match pending_notification {
            Some(NotificationType::DisplayText) => {
                log::info!("DisplayText notification received (NFC)");
                display_mode = nvs::set_display_mode(DisplayMode::CustomText);

                match nvs::load_display_text(&mut storage) {
                    Ok(text) => {
                        log::info!("Queueing custom text for display: {}", text.as_str());
                        queue_text_display(display_channel, &text);
                    }
                    Err(e) => {
                        log::error!("Failed to load display text: {}", e);
                        display_mode = nvs::set_display_mode(DisplayMode::LiveUpdates);
                    }
                }

                // Only stop WiFi if it's actually running
                if matches!(controller.is_started(), Ok(true)) {
                    log::info!("Stopping WiFi to save power...");
                    controller.disconnect_async().await.ok();
                    controller.stop_async().await.ok();
                }
                continue 'process;
            }
            Some(NotificationType::DisplayURL) => {
                log::info!("DisplayURL notification received (NFC)");
                display_mode = nvs::set_display_mode(DisplayMode::QRCode);

                match nvs::load_display_url(&mut storage) {
                    Ok(url) => {
                        log::info!("Queueing QR code for display: {}", url.as_str());
                        queue_qr_display(display_channel, url.as_str());
                    }
                    Err(e) => {
                        log::error!("Failed to load URL: {}", e);
                        display_mode = nvs::set_display_mode(DisplayMode::LiveUpdates);
                    }
                }

                // Only stop WiFi if it's actually running
                if matches!(controller.is_started(), Ok(true)) {
                    log::info!("Stopping WiFi to save power...");
                    controller.disconnect_async().await.ok();
                    controller.stop_async().await.ok();
                }
                continue 'process;
            }
            Some(NotificationType::LiveSecureUpdates) => {
                log::info!("LiveSecureUpdates notification received - switching to MQTT mode");
                display_mode = nvs::set_display_mode(DisplayMode::LiveUpdates);
                false
            }
            Some(NotificationType::WifiCredentials) => {
                log::info!("New WiFi credentials received via NFC, connecting to WiFi and MQTT");
                display_mode = nvs::set_display_mode(DisplayMode::LiveUpdates);
                true
            }
            None => false,
        };

        // Check display mode - skip WiFi for NFC-based display modes (low bandwidth)
        match display_mode {
            DisplayMode::LiveUpdates => {
                log::info!("Display mode is LiveUpdates, proceeding to WiFi");
            }
            DisplayMode::CustomText | DisplayMode::QRCode => {
                log::info!(
                    "In NFC display mode ({:?}), waiting for NFC changes...",
                    display_mode
                );
                // Async wait for either notification type change or nfc_change counter change
                // This is power-efficient - no polling, just waits for actual changes
                select(notification.changed(), nfc_change.changed()).await;
                continue 'process;
            }
        }

        // Check for new WiFi credentials from NFC or loads from the storage if this is the first boot
        if ssid.is_none() || credentials_updated {
            match nvs::load_wifi_credentials(&mut storage) {
                Ok((new_ssid, new_password)) => {
                    log::info!(
                        "Loaded WiFi credentials from storage: SSID='{}', password_len={}",
                        new_ssid.as_str(),
                        new_password.len()
                    );
                    ssid = Some(new_ssid);
                    password = Some(new_password);
                    log::info!("WiFi credentials updated successfully");

                    // Stop WiFi to apply new credentials
                    if matches!(controller.is_started(), Ok(true)) {
                        controller.stop_async().await.ok();
                        log::info!("WiFi stopped to apply new credentials");
                    }
                }
                Err(e) => {
                    ssid = Some(DEFAULT_SSID.try_into().unwrap());
                    password = Some(DEFAULT_PASSWORD.try_into().unwrap());
                    log::info!(
                        "No stored credentials found ({}), using defaults: SSID='{}', password_len={}",
                        e,
                        DEFAULT_SSID,
                        DEFAULT_PASSWORD.len()
                    );
                }
            }
        }

        // Configure and start WiFi
        log::info!("Starting WiFi...");
        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = Configuration::Client(ClientConfiguration {
                ssid: ssid
                    .clone()
                    .unwrap_or_else(|| DEFAULT_SSID.try_into().unwrap()),
                password: password
                    .clone()
                    .unwrap_or_else(|| DEFAULT_PASSWORD.try_into().unwrap()),
                ..Default::default()
            });
            if let Err(e) = controller.set_configuration(&client_config) {
                log::error!("Failed to set WiFi configuration: {:?}", e);
                Timer::after(Duration::from_secs(RETRY_DELAY_SECS)).await;
                continue 'process;
            }
            if let Err(e) = controller.start_async().await {
                log::error!("Failed to start WiFi: {:?}", e);
                Timer::after(Duration::from_secs(RETRY_DELAY_SECS)).await;
                continue 'process;
            }
            log::info!("WiFi started");
        }

        // Connect to WiFi
        log::info!("Connecting to WiFi...");
        match controller.connect_async().await {
            Ok(_) => {
                log::info!("WiFi connected");

                // Enable modem sleep for power saving while keeping WiFi connected
                // Maximum: wakes only at listen interval (more power savings, may miss broadcasts)
                // Minimum: wakes at every DTIM (less savings, won't miss broadcasts)
                match controller.set_power_saving(esp_wifi::config::PowerSaveMode::Maximum) {
                    Ok(_) => log::info!("Enabled Maximum power save mode (modem sleep)"),
                    Err(e) => log::warn!("Failed to set power save mode: {:?}", e),
                }

                // Show reconnected message only if the last thing painted was
                // the disconnect screen. The WifiErrorFlag (persisted) is the
                // authoritative source here — reset-reason is not, because a
                // power cycle comes back as ChipPowerOn regardless of state.
                if !previously_connected {
                    queue_text_with_qr_display(
                        display_channel,
                        WIFI_CONNECTED_MSG,
                        GET_STARTED_URL,
                    );
                    log::info!("Queued WiFi reconnected message for display");
                    nvs::save_wifi_error_flag(false);
                }
                previously_connected = true;
                skip_error_on_first_attempt = false;
            }
            Err(e) => {
                log::error!("Failed to connect to WiFi with error: {e:?}");

                // Stop WiFi BEFORE displaying to avoid SPI/state conflicts
                log::info!("Stopping WiFi before error display...");
                controller.disconnect().ok();
                Timer::after(Duration::from_millis(TRANSITION_DELAY_MS)).await;
                log::info!("WiFi stopped");

                // Now safe to display error message with QR code for support.
                // Skip on the first attempt after a brownout/power-on so a
                // transient reboot doesn't flash the error over good content;
                // subsequent retries in the same boot still paint it.
                if previously_connected && !skip_error_on_first_attempt {
                    queue_text_with_qr_display(display_channel, WIFI_DISCONNECTED_MSG, SUPPORT_URL);
                    log::info!("Queued WiFi error message with QR for display");
                    nvs::save_wifi_error_flag(true);
                }
                skip_error_on_first_attempt = false;
                Timer::after(Duration::from_secs(RETRY_DELAY_SECS)).await;
                previously_connected = false;
                continue 'process;
            }
        }

        // Handle MQTT live updates if in LiveUpdates mode
        if display_mode == DisplayMode::LiveUpdates {
            match handle_live_mqtt_updates(
                &stack,
                rng_ref,
                &mut notification,
                display_channel,
                &tls,
                client_id,
            )
            .await
            {
                Ok(notif) => {
                    log::info!("MQTT session ended due to notification: {notif:?}");
                    // Process the notification that caused the exit
                    match notif {
                        NotificationType::DisplayText => {
                            display_mode = nvs::set_display_mode(DisplayMode::CustomText);
                        }
                        NotificationType::DisplayURL => {
                            display_mode = nvs::set_display_mode(DisplayMode::QRCode);
                        }
                        NotificationType::LiveSecureUpdates => {
                            // Already in LiveUpdates mode, ignore
                        }
                        NotificationType::WifiCredentials => {
                            // Force reload of WiFi credentials on next iteration
                            ssid = None;
                        }
                    }
                }
                Err(MqttSessionError::OtaRequested) => {
                    log::info!("OTA requested, running download after MQTT teardown");

                    // Show the user that something is happening — e-ink takes
                    // a few seconds to render and we're about to do a long
                    // download, so it'll be on screen well before reboot.
                    queue_text_with_qr_display(display_channel, OTA_UPDATING_MSG, APP_STORE_URL);

                    let pending = PENDING_OTA.lock().await.take();
                    if let Some((buf, n)) = pending {
                        let ok = match crate::ota_flash::EspFlashWriter::new() {
                            Ok(flash) => {
                                let mut mgr = ota::OtaManager::new(flash);
                                crate::ota_http::download_and_flash(
                                    &stack,
                                    &tls,
                                    OTA_CA_CERT.as_bytes(),
                                    &mut mgr,
                                    &buf[..n],
                                )
                                .await
                                .is_ok()
                            }
                            Err(e) => {
                                log::error!("Failed to init OTA flash: {:?}", e);
                                false
                            }
                        };
                        if ok {
                            log::info!("OTA success, rebooting into new firmware");
                        } else {
                            log::error!("OTA failed, rebooting into old firmware");
                            // Update the user before reboot. The message will
                            // persist on the e-ink until the old firmware
                            // reconnects and pushes new content.
                            queue_text_with_qr_display(
                                display_channel,
                                OTA_FAILED_MSG,
                                SUPPORT_URL,
                            );
                            // Give the display task time to render the failure
                            // message before we reset and lose it.
                            Timer::after(Duration::from_secs(5)).await;
                        }
                        esp_hal::reset::software_reset();
                    }
                }
                Err(e) => {
                    log::error!("MQTT error: {:?}", e);
                }
            }

            // Disconnect and stop WiFi to save power after MQTT session
            log::info!("Stopping WiFi after MQTT session...");
            controller.disconnect_async().await.ok();
            controller.stop_async().await.ok();
            log::info!("WiFi stopped");

            // Wait before next iteration
            Timer::after(Duration::from_secs(RETRY_DELAY_SECS)).await;
            continue 'process;
        }

        // Disconnect and stop WiFi to save power
        log::info!("Stopping WiFi to save power...");
        controller.disconnect_async().await.ok();
        controller.stop_async().await.ok();
        log::info!("WiFi stopped");

        // Wait REFRESH_INTERVAL_SECS seconds before next refresh
        Timer::after(Duration::from_secs(REFRESH_INTERVAL_SECS)).await;
    }
}

/// Handle live MQTT updates from secure authenticated service
/// Returns the notification that caused the exit, if any
async fn handle_live_mqtt_updates<'a>(
    stack: &Stack<'static>,
    rng_ref: &'static RefCell<Trng<'static>>,
    notification: &mut Receiver<
        'static,
        NoopRawMutex,
        NotificationType,
        NUM_NOTIFICATION_RECEIVERS,
    >,
    display_channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
    tls: &'a esp_mbedtls::Tls<'a>,
    client_id: &str,
) -> Result<NotificationType, MqttSessionError> {
    use embassy_net::tcp::TcpSocket;
    use esp_mbedtls::{Certificates, Mode, TlsVersion, X509, asynch::Session};
    use rust_mqtt::client::{client::MqttClient, client_config::ClientConfig};
    use rust_mqtt::packet::v5::reason_codes::ReasonCode;

    log::info!("Starting MQTT live updates mode");

    // Wait for network stack to be ready
    stack.wait_for_uplink().await;
    log::info!("Stack is uplinked for MQTT");

    let config = stack.wait_for_ipaddress().await;
    log::info!("Acquired IP address for MQTT: {}", config.address);

    // DNS lookup for MQTT broker
    let broker_ip = stack
        .dns_query(MQTT_BROKER, embassy_net::dns::DnsQueryType::A)
        .await
        .map_err(MqttSessionError::DnsLookupFailed)?
        .first()
        .ok_or(MqttSessionError::DnsLookupFailed(
            embassy_net::dns::Error::Failed,
        ))?
        .clone();

    log::info!("MQTT broker IP: {}", broker_ip);

    // Use static buffers to avoid stack overflow
    let mut rx_buffer = MQTT_TCP_RX_BUFFER.lock().await;
    let mut tx_buffer = MQTT_TCP_TX_BUFFER.lock().await;
    let mut socket = TcpSocket::new(stack.clone(), &mut *rx_buffer, &mut *tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(
        MQTT_TIMEOUT_SECS as u64 + SOCKET_TIMEOUT_HEADROOM_SECS,
    )));

    // Connect to MQTT broker
    log::info!("Connecting to MQTT broker...");
    socket
        .connect((broker_ip, MQTT_PORT))
        .await
        .map_err(MqttSessionError::TcpConnectFailed)?;

    log::info!("Connected to MQTT broker, starting TLS handshake...");

    // TLS certificates configuration (paths relative to crate root, set in .env)
    const CA_CERT: &str = concat!(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/",
            env!("CA_CERT_PATH")
        )),
        "\0"
    );
    const CLIENT_CERT: &str = concat!(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/",
            env!("CLIENT_CERT_PATH")
        )),
        "\0"
    );
    const PRIVATE_KEY: &str = concat!(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/",
            env!("PRIVATE_KEY_PATH")
        )),
        "\0"
    );

    let certificates = Certificates {
        ca_chain: X509::pem(CA_CERT.as_bytes()).ok(),
        certificate: X509::pem(CLIENT_CERT.as_bytes()).ok(),
        private_key: X509::pem(PRIVATE_KEY.as_bytes()).ok(),
        password: None,
    };

    // Create TLS session
    let mut session = Session::new(
        &mut socket,
        Mode::Client {
            servername: MQTT_BROKER_CSTR,
        },
        TlsVersion::Tls1_3,
        certificates,
        tls.reference(),
    )
    .map_err(MqttSessionError::TlsFailed)?;

    // Perform TLS handshake
    log::info!("Performing TLS handshake...");
    session
        .connect()
        .await
        .map_err(MqttSessionError::TlsFailed)?;

    log::info!("TLS connection established");

    let mut rng_ref = rng_ref.borrow_mut();
    let mut rng = rng_ref.as_rngcore();

    let mut config = ClientConfig::new(
        rust_mqtt::client::client_config::MqttVersion::MQTTv5,
        &mut rng,
    );
    config.add_client_id(client_id);
    config.max_packet_size = MQTT_BUFFER_SIZE as u32;
    config.keep_alive = MQTT_TIMEOUT_SECS;

    // Allocate MQTT buffers on the stack - task arena size must be large enough
    let mut recv_buffer = [0u8; MQTT_BUFFER_SIZE];
    let mut write_buffer = [0u8; MQTT_BUFFER_SIZE];

    // 4 reserved topics (raw, config, ping, ota) + MAX_DYNAMIC_TOPICS (4) = 8
    let mut client = MqttClient::<_, 8, _>::new(
        session,
        &mut write_buffer,
        MQTT_BUFFER_SIZE,
        &mut recv_buffer,
        MQTT_BUFFER_SIZE,
        config,
    );

    // Connect to broker
    client
        .connect_to_broker()
        .await
        .map_err(MqttSessionError::MqttConnectFailed)?;
    log::info!("Connected to MQTT broker");

    // Construct topic paths using client ID
    let mut raw_topic = String::<64>::new();
    core::fmt::write(&mut raw_topic, format_args!("{}/root/raw", client_id))
        .map_err(|_| MqttSessionError::TopicFormatError)?;

    let mut config_topic = String::<64>::new();
    core::fmt::write(&mut config_topic, format_args!("{}/root/config", client_id))
        .map_err(|_| MqttSessionError::TopicFormatError)?;

    let mut response_topic = String::<64>::new();
    core::fmt::write(
        &mut response_topic,
        format_args!("{}/root/response", client_id),
    )
    .map_err(|_| MqttSessionError::TopicFormatError)?;

    let mut ping_topic = String::<64>::new();
    core::fmt::write(&mut ping_topic, format_args!("{}/root/ping", client_id))
        .map_err(|_| MqttSessionError::TopicFormatError)?;

    let mut ota_topic = String::<64>::new();
    core::fmt::write(&mut ota_topic, format_args!("{}/root/ota", client_id))
        .map_err(|_| MqttSessionError::TopicFormatError)?;

    let reserved_topics: [&str; 5] = [
        raw_topic.as_str(),
        config_topic.as_str(),
        ping_topic.as_str(),
        ota_topic.as_str(),
        OTA_BROADCAST_TOPIC,
    ];

    // Load saved dynamic topics from storage
    let mut dynamic_topics = nvs::load_mqtt_topics();

    // Pending subscription changes (applied at start of loop)
    let mut pending_subscribe: Option<String<64>> = None;
    let mut pending_unsubscribe: Option<String<64>> = None;
    let mut pending_unsubscribe_all = false;
    let mut topics_changed = false;

    // Rate limiting for live updates - load saved value or default to 0
    let mut min_update_interval_secs: u32 = nvs::load_min_update_interval().unwrap_or(0);
    // Track last update Unix timestamp (persisted to flash, survives reboots)
    let mut last_update_unix_ts: Option<u64> = nvs::load_last_update_timestamp();
    if let Some(ts) = last_update_unix_ts {
        log::info!("Loaded last update Unix timestamp: {}", ts);
    }

    // Load and apply saved max_cycles setting
    if let Some(cycles) = nvs::load_max_cycles() {
        queue_set_max_cycles(display_channel, cycles);
    }

    // Subscribe to raw binary data topic
    match client.subscribe_to_topic(raw_topic.as_str()).await {
        Ok(_) => log::info!("Subscribed to topic: {}", raw_topic.as_str()),
        Err(e) => return Err(MqttSessionError::SubscriptionFailed(e)),
    }

    // Subscribe to config topic
    match client.subscribe_to_topic(config_topic.as_str()).await {
        Ok(_) => log::info!("Subscribed to topic: {}", config_topic.as_str()),
        Err(e) => return Err(MqttSessionError::SubscriptionFailed(e)),
    }

    // Subscribe to ping topic (for connection testing)
    match client.subscribe_to_topic(ping_topic.as_str()).await {
        Ok(_) => log::info!("Subscribed to topic: {}", ping_topic.as_str()),
        Err(e) => return Err(MqttSessionError::SubscriptionFailed(e)),
    }

    // Subscribe to OTA topic (for firmware updates, per-device)
    match client.subscribe_to_topic(ota_topic.as_str()).await {
        Ok(_) => log::info!("Subscribed to topic: {}", ota_topic.as_str()),
        Err(e) => return Err(MqttSessionError::SubscriptionFailed(e)),
    }

    // Subscribe to broadcast OTA topic (fleet-wide hotfixes)
    match client.subscribe_to_topic(OTA_BROADCAST_TOPIC).await {
        Ok(_) => log::info!("Subscribed to topic: {}", OTA_BROADCAST_TOPIC),
        Err(e) => return Err(MqttSessionError::SubscriptionFailed(e)),
    }

    // If we just booted from a new OTA partition, show the user a success
    // message BEFORE marking valid. This way, if the display subsystem is
    // broken in the new firmware, mark_valid is never called and the
    // bootloader rolls back on the next reset.
    if is_ota_boot() {
        queue_text_with_qr_display(display_channel, OTA_COMPLETE_MSG, APP_STORE_URL);
        mark_ota_valid();
    }

    // Subscribe to saved dynamic topics
    for topic in dynamic_topics.iter() {
        match client.subscribe_to_topic(topic.as_str()).await {
            Ok(_) => log::info!("Restored subscription to: {}", topic.as_str()),
            Err(e) => log::error!(
                "Failed to restore subscription to {}: {:?}",
                topic.as_str(),
                e
            ),
        }
    }

    // Main MQTT receive loop
    loop {
        // Apply pending subscription changes before receiving messages
        // Process unsubscribe_all first, then unsubscribe, then subscribe
        if pending_unsubscribe_all {
            pending_unsubscribe_all = false;
            log::info!(
                "Unsubscribing from all {} dynamic topics",
                dynamic_topics.len()
            );
            while let Some(topic) = dynamic_topics.pop() {
                // Note: unsubscribe may return ImplementationSpecificError if a message arrives
                // during the operation. This is benign - the broker still processes the unsubscribe.
                match client.unsubscribe_from_topic(topic.as_str()).await {
                    Ok(_) => {
                        log::info!("Unsubscribed from: {}", topic.as_str());
                    }
                    Err(e) => {
                        log::info!(
                            "Unsubscribe from {} returned {:?} (topic still removed)",
                            topic.as_str(),
                            e
                        );
                    }
                }
                topics_changed = true;
            }
        }
        if let Some(topic) = pending_unsubscribe.take() {
            if let Some(pos) = dynamic_topics
                .iter()
                .position(|dt| dt.as_str() == topic.as_str())
            {
                match client.unsubscribe_from_topic(topic.as_str()).await {
                    Ok(_) => {
                        log::info!("Unsubscribed from: {}", topic.as_str());
                    }
                    Err(e) => {
                        log::info!(
                            "Unsubscribe from {} returned {:?} (topic still removed)",
                            topic.as_str(),
                            e
                        );
                    }
                }
                dynamic_topics.remove(pos);
                topics_changed = true;
            }
        }
        if let Some(topic) = pending_subscribe.take() {
            match client.subscribe_to_topic(topic.as_str()).await {
                Ok(_) => {
                    log::info!("Subscribed to dynamic topic: {}", topic.as_str());
                    dynamic_topics.push(topic).ok();
                    topics_changed = true;
                }
                Err(e) => log::error!("Failed to subscribe: {:?}", e),
            }
        }

        // Save topics to storage if changed
        if topics_changed {
            topics_changed = false;
            nvs::save_mqtt_topics(&dynamic_topics);
        }

        // Use select to await notification, ping timer, or MQTT message concurrently
        match select3(
            notification.changed(),
            Timer::after(Duration::from_secs(MQTT_PING_INTERVAL_SECS)),
            client.receive_message(),
        )
        .await
        {
            // Notification received - exit MQTT mode
            Either3::First(notif) => {
                log::info!("Notification received: {:?}, exiting MQTT mode", notif);
                client.disconnect().await.ok();
                return Ok(notif);
            }
            // Ping timer - send keep-alive
            Either3::Second(_) => {
                client.send_ping().await.ok();
            }
            // MQTT message received
            Either3::Third(Ok((topic, payload))) => {
                log::info!(
                    "Received MQTT message on topic: {} ({} bytes)",
                    topic,
                    payload.len()
                );

                match topic {
                    t if t == raw_topic.as_str() => {
                        // Process chunk and send response if needed
                        match process_chunk(payload, display_channel).await {
                            Ok(result) => {
                                if result.send_response {
                                    // Update last_update_unix_ts on success
                                    if result.success {
                                        if let Some(ts) = result.timestamp {
                                            last_update_unix_ts = Some(ts);
                                            nvs::save_last_update_timestamp(ts);
                                        }
                                    }
                                    let response = MqttResponse {
                                        response: if result.success {
                                            MqttResponseStatus::Success
                                        } else {
                                            MqttResponseStatus::Error
                                        },
                                    };
                                    let mut response_buf = [0u8; 32];
                                    let len =
                                        serde_json_core::to_slice(&response, &mut response_buf)
                                            .expect("Failed to serialize response");
                                    if let Err(e) = client
                                        .send_message(
                                            response_topic.as_str(),
                                            &response_buf[..len],
                                            DEFAULT_QOS,
                                            true,
                                        )
                                        .await
                                    {
                                        log::warn!("Failed to publish response: {:?}", e);
                                    }
                                }
                                if result.unsubscribe_all {
                                    log::info!(
                                        "Queuing unsubscribe from all dynamic topics (from raw payload)"
                                    );
                                    pending_unsubscribe_all = true;
                                }
                            }
                            Err(e) => log::error!("Failed to process chunk: {}", e),
                        }
                    }
                    t if t == config_topic.as_str() => {
                        // Handle config messages for dynamic subscriptions
                        match decoding::parse_config(payload) {
                            Ok(config) => {
                                // Queue subscribe request (applied at start of next loop)
                                if let Some(new_topic) = config.subscribe {
                                    if is_reserved_topic(new_topic, &reserved_topics) {
                                        log::warn!(
                                            "Cannot subscribe to reserved topic: {}",
                                            new_topic
                                        );
                                    } else if !config.unsubscribe_all
                                        && dynamic_topics.iter().any(|dt| dt.as_str() == new_topic)
                                    {
                                        // Skip "already subscribed" check if unsubscribe_all is set
                                        // since topics will be cleared before subscribing
                                        log::info!("Already subscribed to: {}", new_topic);
                                    } else if !config.unsubscribe_all
                                        && dynamic_topics.len() >= MAX_DYNAMIC_TOPICS
                                    {
                                        // Skip capacity check if unsubscribe_all is set
                                        // since topics will be cleared before subscribing
                                        log::warn!(
                                            "Max dynamic topics reached, cannot subscribe to: {}",
                                            new_topic
                                        );
                                    } else if let Ok(topic_str) = String::<64>::try_from(new_topic)
                                    {
                                        log::info!("Queuing subscription to: {}", new_topic);
                                        pending_subscribe = Some(topic_str);
                                    }
                                }

                                // Queue unsubscribe request (applied at start of next loop)
                                if let Some(remove_topic) = config.unsubscribe {
                                    if is_reserved_topic(remove_topic, &reserved_topics) {
                                        log::warn!(
                                            "Cannot unsubscribe from reserved topic: {}",
                                            remove_topic
                                        );
                                    } else if dynamic_topics
                                        .iter()
                                        .any(|dt| dt.as_str() == remove_topic)
                                    {
                                        if let Ok(topic_str) = String::<64>::try_from(remove_topic)
                                        {
                                            log::info!(
                                                "Queuing unsubscription from: {}",
                                                remove_topic
                                            );
                                            pending_unsubscribe = Some(topic_str);
                                        }
                                    }
                                }

                                // Queue unsubscribe all request
                                if config.unsubscribe_all {
                                    log::info!("Queuing unsubscribe from all dynamic topics");
                                    pending_unsubscribe_all = true;
                                }

                                // Handle min_update_interval setting
                                if let Some(interval) = config.min_update_interval {
                                    min_update_interval_secs = interval;
                                    nvs::save_min_update_interval(interval);
                                    log::info!(
                                        "Set minimum update interval to {} seconds",
                                        interval
                                    );
                                }

                                // Handle max_cycles setting
                                if let Some(cycles) = config.max_cycles {
                                    log::info!("Setting display max cycles to {}", cycles);
                                    queue_set_max_cycles(display_channel, cycles);
                                    nvs::save_max_cycles(cycles);
                                }

                                // Send response if required
                                if config.requires_response {
                                    let response = MqttResponse {
                                        response: MqttResponseStatus::Success,
                                    };
                                    let mut response_buf = [0u8; 32];
                                    let len =
                                        serde_json_core::to_slice(&response, &mut response_buf)
                                            .expect("Failed to serialize response");
                                    if let Err(e) = client
                                        .send_message(
                                            response_topic.as_str(),
                                            &response_buf[..len],
                                            DEFAULT_QOS,
                                            true,
                                        )
                                        .await
                                    {
                                        log::warn!("Failed to publish config ACK: {:?}", e);
                                    }
                                }
                            }
                            Err(e) => log::error!("Failed to parse config payload: {}", e),
                        }

                        Timer::after(Duration::from_millis(CONFIG_PROCESS_DELAY_MS)).await;
                    }
                    t if t == ping_topic.as_str() => {
                        // Handle ping requests - respond with success to test connection
                        log::info!("Ping received, sending response");
                        let response = MqttResponse {
                            response: MqttResponseStatus::Success,
                        };
                        let mut response_buf = [0u8; 32];
                        let len = serde_json_core::to_slice(&response, &mut response_buf)
                            .expect("Failed to serialize ping response");
                        if let Err(e) = client
                            .send_message(
                                response_topic.as_str(),
                                &response_buf[..len],
                                DEFAULT_QOS,
                                true,
                            )
                            .await
                        {
                            log::warn!("Failed to publish ping response: {:?}", e);
                        } else {
                            log::info!("Ping response sent successfully");
                        }
                    }
                    t if t == ota_topic.as_str() || t == OTA_BROADCAST_TOPIC => {
                        log::info!("OTA update triggered via MQTT ({})", t);

                        // Copy payload off the client's receive buffer before any
                        // further use of `client` (payload borrows from it).
                        let mut buf = [0u8; 512];
                        let n = payload.len().min(buf.len());
                        buf[..n].copy_from_slice(&payload[..n]);
                        *PENDING_OTA.lock().await = Some((buf, n));

                        // Send optimistic ACK before tearing down the MQTT session.
                        // Server treats this as "accepted"; final success is inferred
                        // from the device reconnecting with the new version after reboot.
                        let response = MqttResponse {
                            response: MqttResponseStatus::Success,
                        };
                        let mut response_buf = [0u8; 64];
                        if let Ok(len) = serde_json_core::to_slice(&response, &mut response_buf) {
                            client
                                .send_message(
                                    response_topic.as_str(),
                                    &response_buf[..len],
                                    DEFAULT_QOS,
                                    true,
                                )
                                .await
                                .ok();
                        }

                        // Give AWS IoT a moment to retain the QoS 0 ACK before
                        // we tear down the connection — otherwise the publish
                        // and DISCONNECT race and the broker can drop the ACK,
                        // leaving the Lambda waiting until it times out.
                        Timer::after(Duration::from_millis(300)).await;

                        // Return to the outer loop. `client` (and its TLS session)
                        // gets dropped here, freeing ~30 KB of heap for the OTA
                        // TLS session that runs next.
                        client.disconnect().await.ok();
                        return Err(MqttSessionError::OtaRequested);
                    }
                    t if dynamic_topics.iter().any(|dt| dt.as_str() == t) => {
                        // Handle messages from dynamically subscribed topics (live updates)
                        log::info!("Received live update from: {}", t);

                        // Check rate limiting BEFORE processing the chunk
                        let should_process = match decoding::parse_chunk_metadata(payload) {
                            Ok(metadata) => {
                                let incoming_ts = metadata.timestamp;
                                if min_update_interval_secs == 0 {
                                    true // No rate limiting
                                } else if let (Some(last_ts), Some(current_ts)) =
                                    (last_update_unix_ts, incoming_ts)
                                {
                                    let elapsed_secs = current_ts.saturating_sub(last_ts);
                                    let required_secs = (min_update_interval_secs as u64)
                                        .saturating_sub(RATE_LIMIT_BUFFER_SECS);
                                    if elapsed_secs >= required_secs {
                                        true
                                    } else {
                                        log::info!(
                                            "Rate limited: {} secs since last update, need {} secs",
                                            elapsed_secs,
                                            required_secs
                                        );
                                        false
                                    }
                                } else {
                                    true // First update or no timestamp in chunk
                                }
                            }
                            Err(e) => {
                                log::error!(
                                    "Failed to parse chunk metadata for rate limiting: {}",
                                    e
                                );
                                true // Allow processing on parse error
                            }
                        };

                        if !should_process {
                            continue; // Skip processing this chunk entirely
                        }

                        // Process chunk only if not rate limited
                        match process_chunk(payload, display_channel).await {
                            Ok(result) => {
                                if result.send_response {
                                    // Update last_update_unix_ts on success
                                    if result.success {
                                        if let Some(ts) = result.timestamp {
                                            last_update_unix_ts = Some(ts);
                                            // Persist the timestamp so rate limiting survives reboots
                                            nvs::save_last_update_timestamp(ts);
                                        }
                                    }
                                    let response = MqttResponse {
                                        response: if result.success {
                                            MqttResponseStatus::Success
                                        } else {
                                            MqttResponseStatus::Error
                                        },
                                    };
                                    let mut response_buf = [0u8; 32];
                                    let len =
                                        serde_json_core::to_slice(&response, &mut response_buf)
                                            .expect("Failed to serialize response");
                                    client
                                        .send_message(
                                            response_topic.as_str(),
                                            &response_buf[..len],
                                            DEFAULT_QOS,
                                            true,
                                        )
                                        .await
                                        .ok();
                                }
                                if result.unsubscribe_all {
                                    log::info!(
                                        "Queuing unsubscribe from all dynamic topics (from live update)"
                                    );
                                    pending_unsubscribe_all = true;
                                }
                            }
                            Err(e) => log::error!("Failed to process live update: {}", e),
                        }
                    }
                    _ => log::warn!("Received message on unexpected topic: {}", topic),
                }
            }
            // MQTT receive error
            Either3::Third(Err(e)) => {
                // ImplementationSpecificError typically means no message available, ignore it
                log::error!("MQTT receive error: {:?}", e);
                if e != ReasonCode::ImplementationSpecificError {
                    // Save timestamp before disconnecting so rate limiting persists across reconnects
                    if let Some(ts) = last_update_unix_ts {
                        nvs::save_last_update_timestamp(ts);
                    }
                    client.disconnect().await.ok();
                    return Err(MqttSessionError::ReceiveError);
                }
            }
        }
    }
}

/// Check if this boot is from a freshly-written OTA partition that hasn't
/// been marked valid yet.
fn is_ota_boot() -> bool {
    use ota::FlashWriter;
    match crate::ota_flash::EspFlashWriter::new() {
        Ok(flash) => flash.is_pending_verification(),
        Err(_) => false,
    }
}

/// Mark the currently running OTA firmware as valid so the bootloader won't
/// roll back. Called only after we've verified WiFi + MQTT + OTA subscription
/// + display all work correctly.
fn mark_ota_valid() {
    use ota::FlashWriter;
    match crate::ota_flash::EspFlashWriter::new() {
        Ok(mut flash) => match flash.mark_valid() {
            Ok(()) => log::info!("OTA firmware validated on first boot"),
            Err(e) => log::error!("OTA boot validation failed: {:?}", e),
        },
        Err(e) => log::warn!("Could not mark OTA valid: {:?}", e),
    }
}

/// Check if a topic is a reserved core topic that cannot be dynamically subscribed/unsubscribed
fn is_reserved_topic(topic: &str, reserved: &[&str]) -> bool {
    reserved.iter().any(|r| *r == topic)
}

/// Process a raw binary chunk from MQTT payload
/// Decodes, accumulates, and returns processing result
async fn process_chunk(
    payload: &[u8],
    display_channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
) -> Result<ProcessChunkResult, &'static str> {
    // Decode chunk data and get metadata
    let mut decode_buf = DECODE_BUF.lock().await;
    let (decoded_len, metadata) = decoding::decode_chunk(payload, &mut *decode_buf)?;

    // Update chunk metadata and write to display buffer
    let mut chunk_meta = CHUNK_META.lock().await;

    if metadata.chunk_index == 0 {
        reset_display_buffer();
        chunk_meta.reset();
        chunk_meta.total_chunks = metadata.total_chunks;
    }

    // Try to append to display buffer
    let append_result = append_to_display_buffer(&decode_buf[..decoded_len], chunk_meta.offset);
    if let Some(written) = append_result {
        chunk_meta.offset += written;
        chunk_meta.received_count += 1;
    } else {
        // Buffer was locked - return error if response required
        return Ok(ProcessChunkResult {
            send_response: metadata.requires_response,
            success: false,
            unsubscribe_all: false,
            timestamp: metadata.timestamp,
        });
    }

    // Check if all chunks received
    if chunk_meta.is_complete() {
        log::info!(
            "All {} chunks received. Queuing {} bytes for display",
            chunk_meta.total_chunks,
            chunk_meta.offset
        );
        let queued = queue_frame_ready(display_channel);
        chunk_meta.reset();
        return Ok(ProcessChunkResult {
            send_response: true,
            success: queued,
            unsubscribe_all: metadata.unsubscribe_all,
            timestamp: metadata.timestamp,
        });
    }

    Ok(ProcessChunkResult {
        send_response: metadata.requires_response,
        success: true,
        unsubscribe_all: false, // Only unsubscribe after all chunks received
        timestamp: metadata.timestamp,
    })
}
