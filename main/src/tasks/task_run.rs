//! Main wifi processor

use core::ffi::CStr;
use core::str::FromStr;

use serde::Serialize;

use crate::NUM_NOTIFICATION_RECEIVERS;
use crate::tasks::task_display_handler::{
    DISPLAY_CHANNEL_SIZE, DisplayMessage, append_to_display_buffer, queue_frame_ready,
    queue_qr_display, queue_set_max_cycles, queue_text_display, reset_display_buffer,
};
use crate::{AsyncStack, NotificationType};
use embassy_futures::select::{Either3, select3};
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, watch::Receiver};
use embassy_time::{Duration, Timer};
use rand_core::CryptoRngCore;

use core::cell::RefCell;
use esp_hal::peripherals;
use esp_hal::rng::Trng;
use esp_storage::FlashStorage;
use esp_wifi::wifi::{ClientConfiguration, Configuration, WifiController};
use heapless::String;
use nfc::{MAX_NFCDATA_SIZE, NFCData};
use storage::storage::PersistentStorage;
/// WIFI SSID
pub const DEFAULT_SSID: &str = "HONESTWIFI-2325-2G";
/// WIFI Password
pub const DEFAULT_PASSWORD: &str = "9526070855!";

const REFRESH_INTERVAL_SECS: u64 = 60;

const MQTT_BROKER_CSTR: &CStr = c"avbh2adibwzla-ats.iot.us-east-2.amazonaws.com";
const MQTT_PORT: u16 = 8883; // TLS port
// Client ID: Update this 6-character alphanumeric code for each board
const MQTT_CLIENT_ID: &str = "client1";
const MQTT_TIMEOUT_SECS: u16 = 120;
/// Max size in bytes of the data being sent via AWS
pub const MQTT_BUFFER_SIZE: usize = 7_500;

// Static buffers for MQTT to avoid stack overflow
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};

// Static buffers for MQTT protected by mutexes to prevent concurrent access
// OPTIMIZATION: Only need TCP buffers - MQTT client will reuse these for send/receive
static MQTT_TCP_RX_BUFFER: Mutex<CriticalSectionRawMutex, [u8; MQTT_BUFFER_SIZE]> =
    Mutex::new([0u8; MQTT_BUFFER_SIZE]);
static MQTT_TCP_TX_BUFFER: Mutex<CriticalSectionRawMutex, [u8; MQTT_BUFFER_SIZE]> =
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

static CHUNK_META: Mutex<CriticalSectionRawMutex, ChunkMeta> = Mutex::new(ChunkMeta::new());

/// Static decode buffer to avoid stack allocation on each chunk
static DECODE_BUF: Mutex<CriticalSectionRawMutex, [u8; MQTT_BUFFER_SIZE]> =
    Mutex::new([0u8; MQTT_BUFFER_SIZE]);

/// Process a raw binary chunk from MQTT payload
/// Decodes, accumulates, and returns true if all chunks are received
async fn process_chunk(
    payload: &[u8],
    display_channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
) -> Result<bool, &'static str> {
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

    if let Some(written) = append_to_display_buffer(&decode_buf[..decoded_len], chunk_meta.offset) {
        chunk_meta.offset += written;
        chunk_meta.received_count += 1;
    }

    // Check if all chunks received
    if chunk_meta.is_complete() {
        log::info!(
            "All {} chunks received. Queuing {} bytes for display",
            chunk_meta.total_chunks,
            chunk_meta.offset
        );
        queue_frame_ready(display_channel);
        chunk_meta.reset();
        return Ok(true);
    }

    Ok(metadata.requires_response)
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

/// Maximum number of dynamic topic subscriptions
const MAX_DYNAMIC_TOPICS: usize = 4;

/// Check if a topic is a reserved core topic that cannot be dynamically subscribed/unsubscribed
fn is_reserved_topic(topic: &str, raw_topic: &str, config_topic: &str) -> bool {
    topic == raw_topic || topic == config_topic
}

/// Storage size for MQTT topics (must fit in MqttTopics storage area)
const MQTT_TOPICS_STORAGE_SIZE: usize = 512;

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
                Ok(_) => log::info!("Saved {} MQTT topics to storage ({} bytes)", topics.len(), len),
                Err(e) => log::error!("Failed to write MQTT topics: {:?}", e),
            }
        }
        Err(e) => log::error!("Failed to serialize MQTT topics: {}", e),
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

/// Main application task - manages WiFi connection, MQTT communication, and display modes
///
/// Handles three display modes:
/// - LiveUpdates: Receives display frames via MQTT over TLS
/// - CustomText: Displays text stored via NFC (offline mode)
/// - QRCode: Displays QR codes from URLs stored via NFC (offline mode)
#[embassy_executor::task]
pub async fn task_run(
    stack: Stack<'static>,
    rng_ref: &'static RefCell<Trng<'static>>,
    mut controller: WifiController<'static>,
    mut notification: Receiver<'static, NoopRawMutex, NotificationType, NUM_NOTIFICATION_RECEIVERS>,
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
    let mut display_mode = DisplayMode::LiveUpdates;

    // Main loop - refresh every REFRESH_INTERVAL_SECS seconds
    'process: loop {
        // Check for DisplayText notification (NFC - low bandwidth mode)
        if notification
            .try_changed_and(|val| *val == NotificationType::DisplayText)
            .is_some()
        {
            log::info!("DisplayText notification received (NFC)");
            display_mode = DisplayMode::CustomText;

            // Load and queue custom text for display
            match load_display_text(&mut storage) {
                Ok(text) => {
                    log::info!("Queueing custom text for display: {}", text.as_str());
                    queue_text_display(display_channel, &text);
                }
                Err(e) => {
                    log::error!("Failed to load display text: {}", e);
                    display_mode = DisplayMode::LiveUpdates;
                }
            }

            // Stop WiFi to save power while displaying custom text
            log::info!("Stopping WiFi while displaying custom text...");
            controller.disconnect_async().await.ok();
            controller.stop_async().await.ok();

            Timer::after(Duration::from_secs(5)).await;
            continue 'process;
        }

        // Check for DisplayURL notification (NFC - low bandwidth mode)
        if notification
            .try_changed_and(|val| *val == NotificationType::DisplayURL)
            .is_some()
        {
            log::info!("DisplayURL notification received (NFC)");
            display_mode = DisplayMode::QRCode;

            // Load and queue QR code for display
            match load_display_url(&mut storage) {
                Ok(url) => {
                    log::info!("Queueing QR code for display: {}", url.as_str());
                    queue_qr_display(display_channel, url.as_str());
                }
                Err(e) => {
                    log::error!("Failed to load URL: {}", e);
                    display_mode = DisplayMode::LiveUpdates;
                }
            }

            // Stop WiFi to save power while displaying QR code
            log::info!("Stopping WiFi while displaying QR code...");
            controller.disconnect_async().await.ok();
            controller.stop_async().await.ok();

            Timer::after(Duration::from_secs(5)).await;
            continue 'process;
        }

        // Check for LiveSecureUpdates notification
        if notification
            .try_changed_and(|val| *val == NotificationType::LiveSecureUpdates)
            .is_some()
        {
            log::info!("LiveSecureUpdates notification received - switching to MQTT mode");
            display_mode = DisplayMode::LiveUpdates;
        }

        // Check display mode - skip WiFi for NFC-based display modes (low bandwidth)
        match display_mode {
            DisplayMode::LiveUpdates => {}
            DisplayMode::CustomText | DisplayMode::QRCode => {
                log::info!("In NFC display mode, skipping wifi connection");
                Timer::after(Duration::from_secs(5)).await;
                continue 'process;
            }
        }

        // Check for new WiFi credentials from NFC or loads from the storage if this is the first boot
        if ssid.is_none()
            || notification
                .try_changed_and(|val| *val == NotificationType::WifiCredentials)
                .inspect(|notif| log::info!("New credentials received via NFC: {notif:?}"))
                .is_some()
        {
            match load_wifi_credentials(&mut storage) {
                Ok((new_ssid, new_password)) => {
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
                    log::info!("No stored credentials found ({}), will use defaults", e);
                }
            }
        }

        // Start WiFi if not already started
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
            controller.set_configuration(&client_config).unwrap();
            controller.start_async().await.unwrap();
            log::info!("WiFi started");
        }

        // Connect to WiFi
        log::info!("Connecting to WiFi...");
        match controller.connect_async().await {
            Ok(_) => {
                log::info!("WiFi connected");
                previously_connected = true;
            }
            Err(e) => {
                log::error!("Failed to connect to WiFi with error: {e:?}");
                if let Ok(text) = String::<512>::from_str(
                    "WiFi Connection\nFailed\n-------------------\nPlease tap with\nNFC to update",
                ) && previously_connected
                {
                    queue_text_display(display_channel, &text);
                    log::info!("Queued WiFi error message for display");
                }
                Timer::after(Duration::from_secs(5)).await;
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
                        NotificationType::DisplayText => display_mode = DisplayMode::CustomText,
                        NotificationType::DisplayURL => display_mode = DisplayMode::QRCode,
                        NotificationType::LiveSecureUpdates => {
                            // Already in LiveUpdates mode, ignore
                        }
                        NotificationType::WifiCredentials => {
                            // Will be handled in the WiFi credentials check on next iteration
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
            Timer::after(Duration::from_secs(5)).await;
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
    socket.set_timeout(Some(Duration::from_secs(MQTT_TIMEOUT_SECS as u64)));

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

    // OPTIMIZATION: Allocate MQTT buffers on the stack (task stack has space for this)
    // These replace the previously static MQTT_RECV_BUFFER and MQTT_WRITE_BUFFER
    let mut recv_buffer = [0u8; MQTT_BUFFER_SIZE];
    let mut write_buffer = [0u8; MQTT_BUFFER_SIZE];

    let mut client = MqttClient::<_, 3, _>::new(
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

    // Load saved dynamic topics from storage
    let mut dynamic_topics = load_mqtt_topics();

    // Pending subscription changes (applied at start of loop)
    let mut pending_subscribe: Option<String<64>> = None;
    let mut pending_unsubscribe: Option<String<64>> = None;
    let mut pending_unsubscribe_all = false;
    let mut topics_changed = false;

    // Rate limiting for live updates
    let mut min_update_interval_secs: u16 = 0; // 0 = no limit
    let mut last_update_instant: Option<embassy_time::Instant> = None;

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

    // Subscribe to saved dynamic topics
    for topic in dynamic_topics.iter() {
        match client.subscribe_to_topic(topic.as_str()).await {
            Ok(_) => log::info!("Restored subscription to: {}", topic.as_str()),
            Err(e) => log::error!("Failed to restore subscription to {}: {:?}", topic.as_str(), e),
        }
    }

    // Main MQTT receive loop
    loop {
        // Apply pending subscription changes before receiving messages
        // Process unsubscribe_all first, then unsubscribe, then subscribe
        if pending_unsubscribe_all {
            pending_unsubscribe_all = false;
            log::info!("Unsubscribing from all {} dynamic topics", dynamic_topics.len());
            while let Some(topic) = dynamic_topics.pop() {
                match client.unsubscribe_from_topic(topic.as_str()).await {
                    Ok(_) => {
                        log::info!("Unsubscribed from: {}", topic.as_str());
                        topics_changed = true;
                    }
                    Err(e) => log::error!("Failed to unsubscribe from {}: {:?}", topic.as_str(), e),
                }
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
                        dynamic_topics.remove(pos);
                        topics_changed = true;
                    }
                    Err(e) => log::error!("Failed to unsubscribe: {:?}", e),
                }
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
            Timer::after(Duration::from_secs((MQTT_TIMEOUT_SECS * 3 / 4).into())),
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
                        // Process chunk and send ACK if needed
                        match process_chunk(payload, display_channel).await {
                            Ok(send_ack) if send_ack => {
                                let response = MqttResponse {
                                    response: MqttResponseStatus::Success,
                                };
                                let mut response_buf = [0u8; 32];
                                let len = serde_json_core::to_slice(&response, &mut response_buf)
                                    .expect("Failed to serialize response");
                                if let Err(e) = client
                                    .send_message(
                                        response_topic.as_str(),
                                        &response_buf[..len],
                                        rust_mqtt::packet::v5::publish_packet::QualityOfService::QoS0,
                                        false,
                                    )
                                    .await
                                {
                                    log::warn!("Failed to publish ACK: {:?}", e);
                                }
                            }
                            Ok(_) => {} // No ACK needed
                            Err(e) => log::error!("Failed to process chunk: {}", e),
                        }
                    }
                    t if t == config_topic.as_str() => {
                        // Handle config messages for dynamic subscriptions
                        match decoding::parse_config(payload) {
                            Ok(config) => {
                                // Queue subscribe request (applied at start of next loop)
                                if let Some(new_topic) = config.subscribe {
                                    if is_reserved_topic(
                                        new_topic,
                                        raw_topic.as_str(),
                                        config_topic.as_str(),
                                    ) {
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
                                    if is_reserved_topic(
                                        remove_topic,
                                        raw_topic.as_str(),
                                        config_topic.as_str(),
                                    ) {
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
                                    log::info!(
                                        "Set minimum update interval to {} seconds",
                                        interval
                                    );
                                }

                                // Handle max_cycles setting
                                if let Some(cycles) = config.max_cycles {
                                    log::info!("Setting display max cycles to {}", cycles);
                                    queue_set_max_cycles(display_channel, cycles);
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
                                            rust_mqtt::packet::v5::publish_packet::QualityOfService::QoS0,
                                            false,
                                        )
                                        .await
                                    {
                                        log::warn!("Failed to publish config ACK: {:?}", e);
                                    }
                                }
                            }
                            Err(e) => log::error!("Failed to parse config payload: {}", e),
                        }
                    }
                    t if dynamic_topics.iter().any(|dt| dt.as_str() == t) => {
                        // Handle messages from dynamically subscribed topics (live updates)
                        log::info!("Received live update from: {}", t);

                        // Check rate limiting
                        let now = embassy_time::Instant::now();
                        let should_process = if min_update_interval_secs == 0 {
                            true // No rate limiting
                        } else if let Some(last) = last_update_instant {
                            let elapsed_secs = now.duration_since(last).as_secs();
                            if elapsed_secs >= min_update_interval_secs as u64 {
                                true
                            } else {
                                log::info!(
                                    "Rate limited: {} secs since last update, need {} secs",
                                    elapsed_secs,
                                    min_update_interval_secs
                                );
                                false
                            }
                        } else {
                            true // First update
                        };

                        if should_process {
                            match process_chunk(payload, display_channel).await {
                                Ok(send_ack) if send_ack => {
                                    last_update_instant = Some(now);
                                    let response = MqttResponse {
                                        response: MqttResponseStatus::Success,
                                    };
                                    let mut response_buf = [0u8; 32];
                                    let len =
                                        serde_json_core::to_slice(&response, &mut response_buf)
                                            .expect("Failed to serialize response");
                                    client
                                        .send_message(
                                            response_topic.as_str(),
                                            &response_buf[..len],
                                            rust_mqtt::packet::v5::publish_packet::QualityOfService::QoS0,
                                            false,
                                        )
                                        .await
                                        .ok();
                                }
                                Ok(_) => {}
                                Err(e) => log::error!("Failed to process live update: {}", e),
                            }
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
                    client.disconnect().await.ok();
                    return Err("MQTT receive error");
                }
            }
        }
    }
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
