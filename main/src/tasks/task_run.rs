//! Main wifi processor

use core::ffi::CStr;
use core::str::FromStr;

use crate::NUM_NOTIFICATION_RECEIVERS;
use crate::{AsyncStack, NotificationType, spi::SpiV2};
use display::{Display, EPD417, EPD417_SIZE, OFF};
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, watch::Receiver};
use embassy_time::{Duration, Timer, WithTimeout};
use rand_core::CryptoRngCore;

use core::cell::RefCell;
use esp_hal::peripherals;
use esp_hal::rng::Trng;
use esp_hal::{
    Async,
    gpio::{Input, Output},
};
use esp_storage::FlashStorage;
use esp_wifi::wifi::{ClientConfiguration, Configuration, WifiController};
use heapless::String;
use nfc::{MAX_NFCDATA_SIZE, NFCData};
use storage::storage::PersistentStorage;
use text::{Alignment, FontSize, Text};
/// WIFI SSID
pub const DEFAULT_SSID: &str = "HONESTWIFI-2325-2G";
/// WIFI Password
pub const DEFAULT_PASSWORD: &str = "9526070855!";

const REFRESH_INTERVAL_SECS: u64 = 60;

const MQTT_BROKER_CSTR: &CStr = c"avbh2adibwzla-ats.iot.us-east-2.amazonaws.com";
const MQTT_PORT: u16 = 8883; // TLS port
const MQTT_CLIENT_ID: &str = "client1";
const MQTT_TOPIC1: &str = "example/test";
const MQTT_TOPIC2: &str = "example/test1";
const MQTT_TIMEOUT_SECS: u16 = 60;

// Static buffers for MQTT to avoid stack overflow
// Using raw static mut since MQTT function is called multiple times (StaticCell can only init once)
static mut MQTT_TCP_RX_BUFFER: [u8; 2048] = [0u8; 2048];
static mut MQTT_TCP_TX_BUFFER: [u8; 2048] = [0u8; 2048];
static mut MQTT_RECV_BUFFER: [u8; 2048] = [0u8; 2048];
static mut MQTT_WRITE_BUFFER: [u8; 2048] = [0u8; 2048];

const DISPLAY_TEXT_BUFFER_LENGTH: usize = 512;

/// Display mode for the main task
#[derive(Debug, Clone, Copy, PartialEq)]
enum DisplayMode {
    /// Display custom text from NFC
    CustomText,
    /// Display QR code from URL
    QRCode,
    /// Live secure updates via MQTT
    LiveUpdates,
}

/// Runner for the main wifi processing task
#[embassy_executor::task]
pub async fn task_run(
    stack: Stack<'static>,
    rng_ref: &'static RefCell<Trng<'static>>,
    mut display: Display<
        'static,
        SpiV2<'static, Async>,
        Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        OFF,
    >,
    mut indicator: Output<'static>,
    mut controller: WifiController<'static>,
    mut notification: Receiver<'static, NoopRawMutex, NotificationType, NUM_NOTIFICATION_RECEIVERS>,
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
        // Check for DisplayText notification
        if notification
            .try_changed_and(|val| *val == NotificationType::DisplayText)
            .is_some()
        {
            log::info!("DisplayText notification received");
            display_mode = DisplayMode::CustomText;

            // Load and display custom text from storage
            match load_display_text(&mut storage) {
                Ok(text) => {
                    log::info!("Displaying custom text: {}", text.as_str());
                    match display_text(&mut display, &text).await {
                        Ok(_) => log::info!("Successfully displayed custom text"),
                        Err(e) => log::error!("Error displaying custom text: {:?}", e),
                    }
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

            // Skip MTA updates until DisplayMTAUpdates notification
            Timer::after(Duration::from_secs(5)).await;
            continue 'process;
        }

        // Check for DisplayURL notification
        if notification
            .try_changed_and(|val| *val == NotificationType::DisplayURL)
            .is_some()
        {
            log::info!("DisplayURL notification received");
            display_mode = DisplayMode::QRCode;

            // Load and display QR code from URL in storage
            match load_display_url(&mut storage) {
                Ok(url) => {
                    log::info!("Displaying QR code for URL: {}", url.as_str());
                    match display_qr_code(&mut display, url.as_str()).await {
                        Ok(_) => log::info!("Successfully displayed QR code"),
                        Err(e) => log::error!("Error displaying QR code: {:?}", e),
                    }
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

            // Skip MTA updates until DisplayMTAUpdates notification
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

        match display_mode {
            DisplayMode::LiveUpdates => {}
            DisplayMode::CustomText | DisplayMode::QRCode => {
                log::info!("Skipping wifi connection");
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
                if let Ok(text) = String::<DISPLAY_TEXT_BUFFER_LENGTH>::from_str(
                    "WiFi Connection\nFailed\n-------------------\nPlease tap with\nNFC to update",
                ) && previously_connected
                {
                    match display_text(&mut display, &text).await {
                        Ok(_) => log::info!("Successfully updated display with error message"),
                        Err(e) => log::error!("Error updating display: {:?}", e),
                    }
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
                &mut display,
                &mut indicator,
                &mut notification,
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
    display: &mut Display<
        'static,
        SpiV2<'static, Async>,
        Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        OFF,
    >,
    indicator: &mut Output<'static>,
    notification: &mut Receiver<
        'static,
        NoopRawMutex,
        NotificationType,
        NUM_NOTIFICATION_RECEIVERS,
    >,
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
    // SAFETY: This function is only called from one task, no concurrent access
    let (rx_buffer, tx_buffer) = unsafe {
        (
            &mut *core::ptr::addr_of_mut!(MQTT_TCP_RX_BUFFER),
            &mut *core::ptr::addr_of_mut!(MQTT_TCP_TX_BUFFER),
        )
    };
    let mut socket = TcpSocket::new(stack.clone(), rx_buffer, tx_buffer);
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
    config.max_packet_size = 2048; // Reduced from 2048 to save memory
    config.keep_alive = MQTT_TIMEOUT_SECS;

    // Use static buffers to avoid stack overflow
    // SAFETY: This function is only called from one task, no concurrent access
    let (recv_buffer, write_buffer) = unsafe {
        (
            &mut *core::ptr::addr_of_mut!(MQTT_RECV_BUFFER),
            &mut *core::ptr::addr_of_mut!(MQTT_WRITE_BUFFER),
        )
    };
    let write_len = write_buffer.len();
    let read_len = recv_buffer.len();

    let mut client = MqttClient::<_, 5, _>::new(
        session,
        write_buffer,
        write_len,
        recv_buffer,
        read_len,
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

    // Subscribe to topic
    match client.subscribe_to_topic(MQTT_TOPIC1).await {
        Ok(_) => log::info!("Subscribed to topic: {}", MQTT_TOPIC1),
        Err(e) => {
            log::error!("MQTT subscription error: {:?}", e);
            return Err("Failed to subscribe to MQTT topic");
        }
    }

    match client.subscribe_to_topic(MQTT_TOPIC2).await {
        Ok(_) => log::info!("Subscribed to topic: {}", MQTT_TOPIC2),
        Err(e) => {
            log::error!("MQTT subscription error: {:?}", e);
            return Err("Failed to subscribe to MQTT topic");
        }
    }

    // Main MQTT receive loop
    loop {
        client.send_ping().await.ok();
        // Check for any notification to exit MQTT mode
        // Capture the notification so we can return it for processing
        if let Some(notif) = notification.try_changed() {
            log::info!("Notification received: {:?}, exiting MQTT mode", notif);
            return Ok(notif);
        }

        // Receive messages
        match client
            .receive_message()
            .with_timeout(Duration::from_secs(1))
            .await
        {
            Ok(Ok((topic, payload))) => {
                log::info!("Received MQTT message on topic: {}", topic);

                match topic {
                    topic if topic == MQTT_TOPIC1 => {
                        // Parse payload as UTF-8 text
                        if let Ok(text) = core::str::from_utf8(payload) {
                            log::info!("Message: {}", text);

                            // Display the message
                            indicator.toggle();
                            match display_text(display, text).await {
                                Ok(_) => log::info!("Successfully displayed MQTT message"),
                                Err(e) => log::error!("Error displaying MQTT message: {:?}", e),
                            }
                            indicator.toggle();
                        } else {
                            log::error!("Invalid UTF-8 in MQTT payload");
                        }
                    }
                    topic if topic == MQTT_TOPIC2 => {
                        // Parse payload as UTF-8 text
                        if let Ok(text) = core::str::from_utf8(payload) {
                            log::info!("Message: {}", text);

                            // Display the message
                            indicator.toggle();
                            match display_qr_code(display, text).await {
                                Ok(_) => log::info!("Successfully displayed MQTT message as URL"),
                                Err(e) => {
                                    log::error!("Error displaying MQTT message as URL: {e:?}")
                                }
                            }
                            indicator.toggle();
                        } else {
                            log::error!("Invalid UTF-8 in MQTT payload");
                        }
                    }
                    _ => log::error!("Topic not found"),
                }
            }
            Ok(Err(e)) => {
                log::error!("MQTT receive error: {:?}", e);
                if e != ReasonCode::ImplementationSpecificError {
                    return Err("MQTT receive error");
                }
            }
            Err(_) => {
                // Timeout - continue waiting
                Timer::after(Duration::from_millis(100)).await;
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
) -> Result<String<DISPLAY_TEXT_BUFFER_LENGTH>, &'static str> {
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
) -> Result<String<DISPLAY_TEXT_BUFFER_LENGTH>, &'static str> {
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

/// Update the e-ink display with text
async fn display_text<'a>(
    display: &'a mut Display<
        'static,
        SpiV2<'static, Async>,
        Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        OFF,
    >,
    text: &str,
) -> Result<(), &'static str> {
    log::info!("Updating display");

    // Use a static buffer that we reuse each time
    static mut DISPLAY_TEXT_BUFFER: [u8; DISPLAY_TEXT_BUFFER_LENGTH] =
        [0; DISPLAY_TEXT_BUFFER_LENGTH];
    let static_text = unsafe {
        use core::ptr::addr_of_mut;
        let buf = &mut *addr_of_mut!(DISPLAY_TEXT_BUFFER);
        let text_bytes = text.as_bytes();
        let len = core::cmp::min(text_bytes.len(), buf.len());
        buf[..len].copy_from_slice(&text_bytes[..len]);
        core::str::from_utf8(&buf[..len]).map_err(|_| "Invalid UTF-8")?
    };

    let mut display_on = display
        .on()
        .await
        .map_err(|_| "Failed to turn on display")?;

    let mut frame = Text::new(static_text)
        .with_font_size(FontSize::ExtraLarge24)
        .with_max_width(400)
        .with_alignment(Alignment::Left)
        .with_position(1, 40)
        .to_frame::<400, 300, { 400 * 300 / 8 }>();

    display_on
        .update_and_save_frame::<FlashStorage>(&mut frame, true)
        .await
        .map_err(|_| "Failed to update display")?;

    display_on
        .off()
        .await
        .map_err(|_| "Failed to turn off display")?;

    Ok(())
}

/// Update the e-ink display with a QR code from URL
async fn display_qr_code<'a>(
    display: &'a mut Display<
        'static,
        SpiV2<'static, Async>,
        Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        OFF,
    >,
    url: &str,
) -> Result<(), &'static str> {
    log::info!("Updating display with QR code");

    // Store URL in static memory since Qr::new requires &'static str
    static mut URL_BUFFER: [u8; DISPLAY_TEXT_BUFFER_LENGTH] = [0; DISPLAY_TEXT_BUFFER_LENGTH];
    let static_url = unsafe {
        use core::ptr::addr_of_mut;
        let buf = &mut *addr_of_mut!(URL_BUFFER);
        let url_bytes = url.as_bytes();
        let len = core::cmp::min(url_bytes.len(), buf.len());
        buf[..len].copy_from_slice(&url_bytes[..len]);
        core::str::from_utf8(&buf[..len]).map_err(|_| "Invalid UTF-8")?
    };

    // Calculate the optimal scale that fits within display bounds
    const DISPLAY_WIDTH: u32 = 400;
    const DISPLAY_HEIGHT: u32 = 300;
    const MAX_SCALE: u32 = 7;
    const MIN_SCALE: u32 = 1;

    // Try maximum scale first - if it fits, we're done with one QR generation
    let qr = url::Qr::new(static_url).with_scale(MAX_SCALE);
    let (qr, qr_size) = if let Some(max_size) = qr.size() {
        if max_size <= DISPLAY_WIDTH && max_size <= DISPLAY_HEIGHT {
            // Maximum scale fits, use it!
            (qr, max_size)
        } else {
            // Need to calculate smaller scale
            // Generate at scale 1 to get base module count
            let base_qr = url::Qr::new(static_url).with_scale(MIN_SCALE);
            let base_size = base_qr.size().ok_or("Failed to generate QR code")?;

            // Calculate maximum scale that fits: scale = min(width, height) / base_size
            let max_width_scale = DISPLAY_WIDTH / base_size;
            let max_height_scale = DISPLAY_HEIGHT / base_size;
            let calculated_scale = core::cmp::min(max_width_scale, max_height_scale);

            // Verify we have a valid scale
            if calculated_scale < MIN_SCALE {
                return Err("QR code too large for display even at minimum scale");
            }

            let qr_size = base_size * calculated_scale;
            let qr = url::Qr::new(static_url).with_scale(calculated_scale);
            (qr, qr_size)
        }
    } else {
        return Err("Failed to generate QR code");
    };

    // Position QR code centered on display
    let x_pos = ((DISPLAY_WIDTH - qr_size) / 2) as i32;
    let y_pos = ((DISPLAY_HEIGHT - qr_size) / 2) as i32;

    let qr = qr.with_position(x_pos, y_pos);

    let mut display_on = display
        .on()
        .await
        .map_err(|_| "Failed to turn on display")?;

    let mut frame = qr
        .to_frame::<DISPLAY_WIDTH, DISPLAY_HEIGHT, { (DISPLAY_WIDTH * DISPLAY_HEIGHT / 8) as usize }>();

    display_on
        .update_and_save_frame::<FlashStorage>(&mut frame, true)
        .await
        .map_err(|_| "Failed to update display")?;

    display_on
        .off()
        .await
        .map_err(|_| "Failed to turn off display")?;

    Ok(())
}
