//! Main wifi processor

use core::ffi::CStr;
use core::str::FromStr;

use serde::Serialize;

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
use esp_storage::FlashStorage;
use esp_wifi::wifi::{ClientConfiguration, Configuration, WifiController};
use heapless::String;
use nfc::{MAX_NFCDATA_SIZE, NFCData};
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

/// Get started URL displayed as QR code when WiFi connects
const GET_STARTED_URL: &str = "https://babu12345.github.io/portrait_v2_ios/get_started";

/// Support URL displayed as QR code when WiFi disconnects
const SUPPORT_URL: &str = "https://babu12345.github.io/portrait_v2_ios/support";

const MQTT_BROKER_CSTR: &CStr = c"avbh2adibwzla-ats.iot.us-east-2.amazonaws.com";
const MQTT_PORT: u16 = 8883; // TLS port
/// Client ID and UUID for the device: Update this 6-character alphanumeric code for each board
pub const MQTT_CLIENT_ID: &str = "000001";
/// Max size in bytes of the data being sent via AWS
pub const MQTT_BUFFER_SIZE: usize = 7_000;
/// Maximum number of dynamic topic subscriptions
const MAX_DYNAMIC_TOPICS: usize = 4;

/// Storage size for MQTT topics (must fit in MqttTopics storage area)
const MQTT_TOPICS_STORAGE_SIZE: usize = 512;

// Static buffers for MQTT to avoid stack overflow
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};

// Static buffers for MQTT protected by mutexes to prevent concurrent access
static MQTT_TCP_RX_BUFFER: Mutex<CriticalSectionRawMutex, [u8; MQTT_BUFFER_SIZE]> =
    Mutex::new([0u8; MQTT_BUFFER_SIZE]);
static MQTT_TCP_TX_BUFFER: Mutex<CriticalSectionRawMutex, [u8; MQTT_BUFFER_SIZE]> =
    Mutex::new([0u8; MQTT_BUFFER_SIZE]);

static CHUNK_META: Mutex<CriticalSectionRawMutex, ChunkMeta> = Mutex::new(ChunkMeta::new());

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

/// Display mode for the main task
#[derive(Debug, Clone, Copy, PartialEq)]
enum DisplayMode {
    /// Display custom text from NFC (for low bandwidth)
    CustomText,
    /// Display QR code from URL via NFC (for low bandwidth)
    QRCode,
    /// Live secure updates via MQTT
    LiveUpdates,
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

/// Serializable MQTT topics for persistent storage
#[derive(serde::Serialize, serde::Deserialize)]
struct MqttTopicsData {
    topics: heapless::Vec<String<64>, MAX_DYNAMIC_TOPICS>,
}

impl MqttTopicsData {
    fn to_bytes(&self, out: &mut [u8]) -> Result<usize, &'static str> {
        bincode::serde::encode_into_slice(self, out, bincode::config::standard())
            .map_err(|_| "Failed to serialize MQTT topics")
    }

    fn from_bytes(payload: &[u8]) -> Result<Self, &'static str> {
        bincode::serde::decode_from_slice(payload, bincode::config::standard())
            .map(|(data, _)| data)
            .map_err(|_| "Failed to deserialize MQTT topics")
    }
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
) {
    let tls = esp_mbedtls::Tls::new(sha).unwrap().with_hardware_rsa(rsa);

    let mut storage_data = [0u8; MAX_NFCDATA_SIZE];
    let mut storage = PersistentStorage::new(FlashStorage::default(), &mut storage_data);

    let mut ssid: Option<String<32>> = None;
    let mut password: Option<String<64>> = None;

    let mut previously_connected = true;
    let mut display_mode = load_display_mode();

    // Check reset reason - skip status messages on brownout to preserve last displayed image
    let reason = reset_reason();
    log::info!("Reset reason: {:?}", reason);
    let skip_status_messages = matches!(reason, Some(SocResetReason::SysBrownOut));

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
                display_mode = set_display_mode(DisplayMode::CustomText);

                match load_display_text(&mut storage) {
                    Ok(text) => {
                        log::info!("Queueing custom text for display: {}", text.as_str());
                        queue_text_display(display_channel, &text);
                    }
                    Err(e) => {
                        log::error!("Failed to load display text: {}", e);
                        display_mode = set_display_mode(DisplayMode::LiveUpdates);
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
                display_mode = set_display_mode(DisplayMode::QRCode);

                match load_display_url(&mut storage) {
                    Ok(url) => {
                        log::info!("Queueing QR code for display: {}", url.as_str());
                        queue_qr_display(display_channel, url.as_str());
                    }
                    Err(e) => {
                        log::error!("Failed to load URL: {}", e);
                        display_mode = set_display_mode(DisplayMode::LiveUpdates);
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
                display_mode = set_display_mode(DisplayMode::LiveUpdates);
                false
            }
            Some(NotificationType::WifiCredentials) => {
                log::info!("New WiFi credentials received via NFC, connecting to WiFi and MQTT");
                display_mode = set_display_mode(DisplayMode::LiveUpdates);
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
            match load_wifi_credentials(&mut storage) {
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

                // Show reconnected message if we were previously disconnected
                // Skip on brownout/power-on to preserve last displayed image
                if !previously_connected && !skip_status_messages {
                    queue_text_with_qr_display(
                        display_channel,
                        WIFI_CONNECTED_MSG,
                        GET_STARTED_URL,
                    );
                    log::info!("Queued WiFi reconnected message for display");
                }
                previously_connected = true;
            }
            Err(e) => {
                log::error!("Failed to connect to WiFi with error: {e:?}");

                // Stop WiFi BEFORE displaying to avoid SPI/state conflicts
                log::info!("Stopping WiFi before error display...");
                controller.disconnect().ok();
                Timer::after(Duration::from_millis(TRANSITION_DELAY_MS)).await;
                controller.stop().ok();
                Timer::after(Duration::from_millis(TRANSITION_DELAY_MS)).await;
                log::info!("WiFi stopped");

                // Now safe to display error message with QR code for support
                // Skip on brownout/power-on to preserve last displayed image
                if previously_connected && !skip_status_messages {
                    queue_text_with_qr_display(display_channel, WIFI_DISCONNECTED_MSG, SUPPORT_URL);
                    log::info!("Queued WiFi error message with QR for display");
                }
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
            )
            .await
            {
                Ok(notif) => {
                    log::info!("MQTT session ended due to notification: {notif:?}");
                    // Process the notification that caused the exit
                    match notif {
                        NotificationType::DisplayText => {
                            display_mode = set_display_mode(DisplayMode::CustomText);
                        }
                        NotificationType::DisplayURL => {
                            display_mode = set_display_mode(DisplayMode::QRCode);
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
                Err(e) => {
                    log::error!("MQTT error: {}", e);
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
) -> Result<NotificationType, &'static str> {
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
        .dns_query(
            MQTT_BROKER_CSTR
                .to_str()
                .map_err(|_| "Unable to convert broker name to a string")?,
            embassy_net::dns::DnsQueryType::A,
        )
        .await
        .map_err(|_| "MQTT DNS lookup failed")?
        .first()
        .ok_or("No MQTT broker IP found")?
        .clone();

    log::info!("MQTT broker IP: {}", broker_ip);

    // Use static buffers to avoid stack overflow
    let mut rx_buffer = MQTT_TCP_RX_BUFFER.lock().await;
    let mut tx_buffer = MQTT_TCP_TX_BUFFER.lock().await;
    let mut socket = TcpSocket::new(stack.clone(), &mut *rx_buffer, &mut *tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(MQTT_TIMEOUT_SECS as u64 + SOCKET_TIMEOUT_HEADROOM_SECS)));

    // Connect to MQTT broker
    log::info!("Connecting to MQTT broker...");
    socket
        .connect((broker_ip, MQTT_PORT))
        .await
        .inspect_err(|e| log::error!("Error: {e:?}"))
        .map_err(|_| "Failed to connect to MQTT broker")?;

    log::info!("Connected to MQTT broker, starting TLS handshake...");

    // TLS certificates configuration
    // Load certificates from src/certificates/ directory
    const CA_CERT: &str = concat!(include_str!("../certificates/ca1.pem"), "\0");
    const CLIENT_CERT: &str = concat!(include_str!("../certificates/cert.pem.crt"), "\0");
    const PRIVATE_KEY: &str = concat!(include_str!("../certificates/private_key.pem.key"), "\0");

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
    .inspect_err(|e| log::error!("Error: {e:?}"))
    .map_err(|_| "Failed to create TLS session")?;

    // Perform TLS handshake
    log::info!("Performing TLS handshake...");
    session
        .connect()
        .await
        .map_err(|_| "TLS handshake failed")?;

    log::info!("TLS connection established");

    let mut rng_ref = rng_ref.borrow_mut();
    let mut rng = rng_ref.as_rngcore();

    let mut config = ClientConfig::new(
        rust_mqtt::client::client_config::MqttVersion::MQTTv5,
        &mut rng,
    );
    config.add_client_id(MQTT_CLIENT_ID);
    config.max_packet_size = MQTT_BUFFER_SIZE as u32;
    config.keep_alive = MQTT_TIMEOUT_SECS;

    // Allocate MQTT buffers on the stack - task arena size must be large enough
    let mut recv_buffer = [0u8; MQTT_BUFFER_SIZE];
    let mut write_buffer = [0u8; MQTT_BUFFER_SIZE];

    // 3 reserved topics (raw, config, ping) + MAX_DYNAMIC_TOPICS (4) = 7
    let mut client = MqttClient::<_, 7, _>::new(
        session,
        &mut write_buffer,
        MQTT_BUFFER_SIZE,
        &mut recv_buffer,
        MQTT_BUFFER_SIZE,
        config,
    );

    // Connect to broker
    match client.connect_to_broker().await {
        Ok(()) => log::info!("Connected to MQTT broker"),
        Err(e) => {
            log::error!("MQTT connection error: {:?}", e);
            return Err("Failed to connect to MQTT broker");
        }
    }

    // Construct topic paths using client ID
    let mut raw_topic = String::<64>::new();
    core::fmt::write(&mut raw_topic, format_args!("{}/root/raw", MQTT_CLIENT_ID))
        .map_err(|_| "Failed to format raw topic")?;

    let mut config_topic = String::<64>::new();
    core::fmt::write(
        &mut config_topic,
        format_args!("{}/root/config", MQTT_CLIENT_ID),
    )
    .map_err(|_| "Failed to format config topic")?;

    let mut response_topic = String::<64>::new();
    core::fmt::write(
        &mut response_topic,
        format_args!("{}/root/response", MQTT_CLIENT_ID),
    )
    .map_err(|_| "Failed to format response topic")?;

    let mut ping_topic = String::<64>::new();
    core::fmt::write(
        &mut ping_topic,
        format_args!("{}/root/ping", MQTT_CLIENT_ID),
    )
    .map_err(|_| "Failed to format ping topic")?;

    let reserved_topics: [&str; 3] = [
        raw_topic.as_str(),
        config_topic.as_str(),
        ping_topic.as_str(),
    ];

    // Load saved dynamic topics from storage
    let mut dynamic_topics = load_mqtt_topics();

    // Pending subscription changes (applied at start of loop)
    let mut pending_subscribe: Option<String<64>> = None;
    let mut pending_unsubscribe: Option<String<64>> = None;
    let mut pending_unsubscribe_all = false;
    let mut topics_changed = false;

    // Rate limiting for live updates - load saved value or default to 0
    let mut min_update_interval_secs: u16 = load_min_update_interval().unwrap_or(0);
    // Track last update Unix timestamp (persisted to flash, survives reboots)
    let mut last_update_unix_ts: Option<u64> = load_last_update_timestamp();
    if let Some(ts) = last_update_unix_ts {
        log::info!("Loaded last update Unix timestamp: {}", ts);
    }

    // Load and apply saved max_cycles setting
    if let Some(cycles) = load_max_cycles() {
        queue_set_max_cycles(display_channel, cycles);
    }

    // Subscribe to raw binary data topic
    match client.subscribe_to_topic(raw_topic.as_str()).await {
        Ok(_) => log::info!("Subscribed to topic: {}", raw_topic.as_str()),
        Err(e) => {
            log::error!("MQTT subscription error: {:?}", e);
            return Err("Failed to subscribe to MQTT topic");
        }
    }

    // Subscribe to config topic
    match client.subscribe_to_topic(config_topic.as_str()).await {
        Ok(_) => log::info!("Subscribed to topic: {}", config_topic.as_str()),
        Err(e) => {
            log::error!("MQTT config subscription error: {:?}", e);
            return Err("Failed to subscribe to config topic");
        }
    }

    // Subscribe to ping topic (for connection testing)
    match client.subscribe_to_topic(ping_topic.as_str()).await {
        Ok(_) => log::info!("Subscribed to topic: {}", ping_topic.as_str()),
        Err(e) => {
            log::error!("MQTT ping subscription error: {:?}", e);
            return Err("Failed to subscribe to ping topic");
        }
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
            save_mqtt_topics(&dynamic_topics);
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
                                            save_last_update_timestamp(ts);
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
                                    save_min_update_interval(interval);
                                    log::info!(
                                        "Set minimum update interval to {} seconds",
                                        interval
                                    );
                                }

                                // Handle max_cycles setting
                                if let Some(cycles) = config.max_cycles {
                                    log::info!("Setting display max cycles to {}", cycles);
                                    queue_set_max_cycles(display_channel, cycles);
                                    save_max_cycles(cycles);
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
                                            save_last_update_timestamp(ts);
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
                        save_last_update_timestamp(ts);
                    }
                    client.disconnect().await.ok();
                    return Err("MQTT receive error");
                }
            }
        }
    }
}

/// Check if a topic is a reserved core topic that cannot be dynamically subscribed/unsubscribed
fn is_reserved_topic(topic: &str, reserved: &[&str]) -> bool {
    reserved.iter().any(|r| *r == topic)
}

/// Load WiFi credentials from storage
fn load_wifi_credentials(
    storage: &mut PersistentStorage<FlashStorage>,
) -> Result<(String<32>, String<64>), &'static str> {
    let data = storage
        .read(storage::storage::StorageContents::WifiCredentials)
        .map_err(|_| "Failed to read WiFi credentials from storage")?;

    let wifi_data = NFCData::from_bytes(data).map_err(|_| "Failed to parse WiFi credentials")?;

    match wifi_data {
        NFCData::Wifi(ssid_data, password_data) => {
            let ssid = String::from_str(ssid_data.as_str())
                .map_err(|_| "Failed to convert SSID to String")?;
            let password = String::from_str(password_data.as_str())
                .map_err(|_| "Failed to convert password to String")?;
            Ok((ssid, password))
        }
        _ => Err("Storage contains non-WiFi credential data"),
    }
}

/// Load display text from storage
fn load_display_text(
    storage: &mut PersistentStorage<FlashStorage>,
) -> Result<String<MAX_NFCDATA_SIZE>, &'static str> {
    let data = storage
        .read(storage::storage::StorageContents::DisplayText)
        .map_err(|_| "Failed to read display text from storage")?;

    let text_data = NFCData::from_bytes(data).map_err(|_| "Failed to parse display text")?;

    match text_data {
        NFCData::Text(text) => String::from_str(text.as_str())
            .map_err(|_| "Failed to convert text to String or text too long"),
        _ => Err("Storage contains non-text data"),
    }
}

/// Load display URL from storage
fn load_display_url(
    storage: &mut PersistentStorage<FlashStorage>,
) -> Result<String<MAX_NFCDATA_SIZE>, &'static str> {
    let data = storage
        .read(storage::storage::StorageContents::DisplayURL)
        .map_err(|_| "Failed to read display URL from storage")?;

    let url_data = NFCData::from_bytes(data).map_err(|_| "Failed to parse display URL")?;

    match url_data {
        NFCData::Uri(url) => String::from_str(url.as_str())
            .map_err(|_| "Failed to convert URL to String or URL too long"),
        _ => Err("Storage contains non-URL data"),
    }
}

/// Load min_update_interval setting from flash storage (stored as u32, 4 bytes)
fn load_min_update_interval() -> Option<u16> {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.read(storage::storage::StorageContents::MinUpdateInterval) {
        Ok(data) => {
            // Check if storage is empty (0xFF means uninitialized)
            if data[0] == 0xFF && data[1] == 0xFF && data[2] == 0xFF && data[3] == 0xFF {
                log::info!("No saved min_update_interval found");
                return None;
            }
            let interval = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            log::info!(
                "Loaded min_update_interval from storage: {} seconds",
                interval
            );
            Some(interval as u16)
        }
        Err(e) => {
            log::error!("Failed to read min_update_interval: {:?}", e);
            None
        }
    }
}

/// Load dynamic topics from flash storage using bincode
fn load_mqtt_topics() -> heapless::Vec<String<64>, MAX_DYNAMIC_TOPICS> {
    let mut storage_buf = [0u8; MQTT_TOPICS_STORAGE_SIZE];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.read(storage::storage::StorageContents::MqttTopics) {
        Ok(data) => {
            // Check if storage is empty (0xFF means uninitialized)
            if data[0] == 0xFF {
                log::info!("No saved MQTT topics found");
                return heapless::Vec::new();
            }

            match MqttTopicsData::from_bytes(data) {
                Ok(mqtt_data) => {
                    log::info!("Loaded {} MQTT topics from storage", mqtt_data.topics.len());
                    mqtt_data.topics
                }
                Err(e) => {
                    log::warn!("Failed to parse MQTT topics: {}", e);
                    heapless::Vec::new()
                }
            }
        }
        Err(e) => {
            log::error!("Failed to read MQTT topics: {:?}", e);
            heapless::Vec::new()
        }
    }
}

/// Load max_cycles setting from flash storage (stored as u16, 2 bytes)
fn load_max_cycles() -> Option<u8> {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.read(storage::storage::StorageContents::MaxCyclesBeforeFullRefresh) {
        Ok(data) => {
            // Check if storage is empty (0xFF means uninitialized)
            if data[0] == 0xFF && data[1] == 0xFF {
                log::info!("No saved max_cycles found");
                return None;
            }
            let cycles = u16::from_le_bytes([data[0], data[1]]);
            log::info!("Loaded max_cycles from storage: {}", cycles);
            if cycles == 0 {
                log::info!("Ignoring saved max_cycles of 0");
                return None;
            }
            Some(cycles as u8)
        }
        Err(e) => {
            log::error!("Failed to read max_cycles: {:?}", e);
            None
        }
    }
}

/// Save dynamic topics to flash storage using bincode
fn save_mqtt_topics(topics: &heapless::Vec<String<64>, MAX_DYNAMIC_TOPICS>) {
    let mut storage_buf = [0u8; MQTT_TOPICS_STORAGE_SIZE];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    let data = MqttTopicsData {
        topics: topics.clone(),
    };

    let mut encode_buf = [0u8; MQTT_TOPICS_STORAGE_SIZE];
    match data.to_bytes(&mut encode_buf) {
        Ok(len) => {
            match storage.write_bytes(
                storage::storage::StorageContents::MqttTopics,
                0,
                &encode_buf[..len],
            ) {
                Ok(_) => log::info!(
                    "Saved {} MQTT topics to storage ({} bytes)",
                    topics.len(),
                    len
                ),
                Err(e) => log::error!("Failed to write MQTT topics: {:?}", e),
            }
        }
        Err(e) => log::error!("Failed to serialize MQTT topics: {}", e),
    }
}

/// Save max_cycles setting to flash storage (stored as u16, 2 bytes)
fn save_max_cycles(cycles: u8) {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    let bytes = (cycles as u16).to_le_bytes();
    match storage.write_bytes(
        storage::storage::StorageContents::MaxCyclesBeforeFullRefresh,
        0,
        &bytes,
    ) {
        Ok(_) => log::info!("Saved max_cycles to storage: {}", cycles),
        Err(e) => log::error!("Failed to write max_cycles: {:?}", e),
    }
}

/// Save min_update_interval setting to flash storage (stored as u32, 4 bytes)
fn save_min_update_interval(interval: u16) {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    let bytes = (interval as u32).to_le_bytes();
    match storage.write_bytes(
        storage::storage::StorageContents::MinUpdateInterval,
        0,
        &bytes,
    ) {
        Ok(_) => log::info!("Saved min_update_interval to storage: {} seconds", interval),
        Err(e) => log::error!("Failed to write min_update_interval: {:?}", e),
    }
}

/// Load last update timestamp from flash storage (stored as u64, 8 bytes)
/// Returns seconds since boot when last successful update occurred
fn load_last_update_timestamp() -> Option<u64> {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.read(storage::storage::StorageContents::LastUpdateTimestamp) {
        Ok(data) => {
            // Check if storage is empty (0xFF means uninitialized)
            if data[0] == 0xFF
                && data[1] == 0xFF
                && data[2] == 0xFF
                && data[3] == 0xFF
                && data[4] == 0xFF
                && data[5] == 0xFF
                && data[6] == 0xFF
                && data[7] == 0xFF
            {
                log::info!("No saved last_update_timestamp found");
                return None;
            }
            let timestamp = u64::from_le_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ]);
            log::info!(
                "Loaded last_update_timestamp from storage: {} seconds",
                timestamp
            );
            Some(timestamp)
        }
        Err(e) => {
            log::error!("Failed to read last_update_timestamp: {:?}", e);
            None
        }
    }
}

/// Save last update timestamp to flash storage (stored as u64, 8 bytes)
fn save_last_update_timestamp(timestamp_secs: u64) {
    let mut storage_buf = [0u8; 16];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    let bytes = timestamp_secs.to_le_bytes();
    match storage.write_bytes(
        storage::storage::StorageContents::LastUpdateTimestamp,
        0,
        &bytes,
    ) {
        Ok(_) => log::info!(
            "Saved last_update_timestamp to storage: {} seconds",
            timestamp_secs
        ),
        Err(e) => log::error!("Failed to write last_update_timestamp: {:?}", e),
    }
}

/// Load display mode from flash storage
/// Returns DisplayMode, defaulting to LiveUpdates if not set or invalid
fn load_display_mode() -> DisplayMode {
    let mut storage_buf = [0u8; 4];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    match storage.read(storage::storage::StorageContents::DisplayMode) {
        Ok(data) => {
            match data[0] {
                0x00 => {
                    log::info!("Loaded display mode: LiveUpdates");
                    DisplayMode::LiveUpdates
                }
                0x01 => {
                    log::info!("Loaded display mode: CustomText");
                    DisplayMode::CustomText
                }
                0x02 => {
                    log::info!("Loaded display mode: QRCode");
                    DisplayMode::QRCode
                }
                _ => {
                    // 0xFF (uninitialized) or invalid value
                    log::info!("No saved display mode, defaulting to LiveUpdates");
                    DisplayMode::LiveUpdates
                }
            }
        }
        Err(e) => {
            log::error!("Failed to read display mode: {:?}", e);
            DisplayMode::LiveUpdates
        }
    }
}

/// Set and persist display mode to flash storage
/// Returns the mode for assignment: `display_mode = set_display_mode(DisplayMode::CustomText);`
fn set_display_mode(mode: DisplayMode) -> DisplayMode {
    let mut storage_buf = [0u8; 4];
    let mut storage = PersistentStorage::new(FlashStorage::new(), &mut storage_buf);

    let byte = match mode {
        DisplayMode::LiveUpdates => 0x00,
        DisplayMode::CustomText => 0x01,
        DisplayMode::QRCode => 0x02,
    };

    match storage.write_bytes(storage::storage::StorageContents::DisplayMode, 0, &[byte]) {
        Ok(_) => log::info!("Saved display mode: {:?}", mode),
        Err(e) => log::error!("Failed to write display mode: {:?}", e),
    }

    mode
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
