//! Display handler task for rate-limited display updates

use crate::{spi::SpiV2, tasks::MatchSliceLengths};
use display::{Display, EPD417, EPD417_SIZE};
use embassy_sync::{
    blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex},
    channel::Channel,
    mutex::Mutex,
};
use embassy_time::{Duration, Timer};
use esp_hal::{Async, gpio::Output};
use esp_storage::FlashStorage;
use text::{Alignment, FontSize, Text};

/// Size of the display message channel
pub const DISPLAY_CHANNEL_SIZE: usize = 5;
const DISPLAY_UPDATE_DELAY_MS: u64 = 200; // Minimum delay between display updates
const DISPLAY_TEXT_BUFFER_LENGTH: usize = 512;
const RAW_DISPLAY_BUFFER_SIZE: usize = 15360; // 15KB for raw binary display data

const DISPLAY_WIDTH: u32 = 400;
const DISPLAY_HEIGHT: u32 = 300;

const DISPLAY_SIZE_IN_BYTES: usize = (DISPLAY_WIDTH * DISPLAY_HEIGHT / 8) as usize;

/// Messages that can be sent to the display task
#[derive(Debug, Clone, Copy)]
pub enum DisplayMessage {
    /// Display text on the screen (via NFC)
    Text,
    /// Display a QR code from URL (via NFC)
    QRCode,
    /// Display raw binary data (via MQTT)
    RawBinary,
}


// Static buffer for all display data protected by mutex
// OPTIMIZATION: Single unified buffer for all display types since messages are processed sequentially
// - Text/URL use first DISPLAY_TEXT_BUFFER_LENGTH bytes (512)
// - Raw binary uses full RAW_DISPLAY_BUFFER_SIZE bytes (15,360)
static UNIFIED_DISPLAY_BUFFER: Mutex<CriticalSectionRawMutex, [u8; RAW_DISPLAY_BUFFER_SIZE]> =
    Mutex::new([0; RAW_DISPLAY_BUFFER_SIZE]);

/// Display handler task that processes display messages from a channel
/// This allows rate-limiting display updates without blocking data input
#[embassy_executor::task]
pub async fn task_display_handler(
    mut display: Display<
        'static,
        SpiV2<'static, Async>,
        esp_hal::gpio::Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        display::OFF,
    >,
    channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
    mut indicator: Output<'static>,
) {
    loop {
        // Wait for a display message
        let message = channel.receive().await;
        log::info!("Display task received message: {:?}", message);

        indicator.toggle();

        // Process the message
        match message {
            DisplayMessage::Text => {
                let buf = UNIFIED_DISPLAY_BUFFER.lock().await;
                // Only use first DISPLAY_TEXT_BUFFER_LENGTH bytes for text
                let text_slice = &buf[..DISPLAY_TEXT_BUFFER_LENGTH];
                let len = text_slice
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(text_slice.len());
                match core::str::from_utf8(&text_slice[..len]) {
                    Ok(text) if !text.is_empty() => match display_text(&mut display, text).await {
                        Ok(_) => log::info!("Successfully displayed text"),
                        Err(e) => log::error!("Error displaying text: {:?}", e),
                    },
                    Ok(_) => {} // Empty text, do nothing
                    Err(_) => {
                        log::error!("Invalid UTF-8 in text buffer");
                    }
                }
            }
            DisplayMessage::QRCode => {
                let buf = UNIFIED_DISPLAY_BUFFER.lock().await;
                // Only use first DISPLAY_TEXT_BUFFER_LENGTH bytes for URL
                let url_slice = &buf[..DISPLAY_TEXT_BUFFER_LENGTH];
                let len = url_slice
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(url_slice.len());
                match core::str::from_utf8(&url_slice[..len]) {
                    Ok(url) if !url.is_empty() => match display_qr_code(&mut display, url).await {
                        Ok(_) => log::info!("Successfully displayed QR code"),
                        Err(e) => log::error!("Error displaying QR code: {:?}", e),
                    },
                    Ok(_) => {} // Empty URL, do nothing
                    Err(_) => {
                        log::error!("Invalid UTF-8 in URL buffer");
                    }
                }
            }
            DisplayMessage::RawBinary => {
                let mut buf = UNIFIED_DISPLAY_BUFFER.lock().await;

                // Get the actual data length (find first zero or use full buffer)
                let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());

                if len > 0 {
                    log::info!("Processing raw binary data: {} bytes", len);
                    match display_raw_binary(&mut display, &mut buf[..len]).await {
                        Ok(_) => log::info!("Successfully displayed raw binary"),
                        Err(e) => log::error!("Error displaying raw binary: {:?}", e),
                    }
                } else {
                    log::warn!("Empty raw binary buffer, skipping display");
                }
            }
        }

        indicator.toggle();

        // Rate limit: wait before allowing next display update
        Timer::after(Duration::from_millis(DISPLAY_UPDATE_DELAY_MS)).await;
    }
}

/// Store text in the display task's buffer and send message to display it
pub fn queue_text_display(
    channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
    text: &str,
) {
    if let Ok(mut buf) = UNIFIED_DISPLAY_BUFFER.try_lock() {
        // Only clear and use first DISPLAY_TEXT_BUFFER_LENGTH bytes for text
        buf[..DISPLAY_TEXT_BUFFER_LENGTH].fill(0);
        let text_bytes = text.as_bytes();
        let len = core::cmp::min(text_bytes.len(), DISPLAY_TEXT_BUFFER_LENGTH);
        buf[..len].copy_from_slice(&text_bytes[..len]);
    } else {
        log::warn!("Display buffer locked, skipping update");
        return;
    }

    // Try to send, drop oldest message if channel is full
    if channel.try_send(DisplayMessage::Text).is_err() {
        log::warn!("Display channel full, message may be dropped");
    }
}

/// Store URL in the display task's buffer and send message to display it
pub fn queue_qr_display(
    channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
    url: &str,
) {
    if let Ok(mut buf) = UNIFIED_DISPLAY_BUFFER.try_lock() {
        // Only clear and use first DISPLAY_TEXT_BUFFER_LENGTH bytes for URL
        buf[..DISPLAY_TEXT_BUFFER_LENGTH].fill(0);
        let url_bytes = url.as_bytes();
        let len = core::cmp::min(url_bytes.len(), DISPLAY_TEXT_BUFFER_LENGTH);
        buf[..len].copy_from_slice(&url_bytes[..len]);
    } else {
        log::warn!("Display buffer locked, skipping update");
        return;
    }

    // Try to send, drop oldest message if channel is full
    if channel.try_send(DisplayMessage::QRCode).is_err() {
        log::warn!("Display channel full, message may be dropped");
    }
}

/// Store raw binary data in the display task's buffer and send message to display it (via MQTT)
pub fn queue_raw_display(
    channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
    data: &[u8],
) {
    if let Ok(mut buf) = UNIFIED_DISPLAY_BUFFER.try_lock() {
        // Use full buffer for raw binary data
        buf.fill(0);
        let len = core::cmp::min(data.len(), buf.len());
        buf[..len].copy_from_slice(&data[..len]);

        if data.len() > buf.len() {
            log::warn!(
                "Raw display data truncated: {} bytes received, {} bytes buffer",
                data.len(),
                buf.len()
            );
        }
    } else {
        log::warn!("Display buffer locked, skipping update");
        return;
    }

    // Try to send, drop oldest message if channel is full
    if channel.try_send(DisplayMessage::RawBinary).is_err() {
        log::warn!("Display channel full, message may be dropped");
    }
}

/// Update the e-ink display with text
async fn display_text<'a, 'b>(
    display: &'a mut Display<
        'static,
        SpiV2<'static, Async>,
        esp_hal::gpio::Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        display::OFF,
    >,
    text: &'b str,
) -> Result<(), &'static str> {
    log::info!("Updating display with text");

    let mut display_on = display
        .on()
        .await
        .map_err(|_| "Failed to turn on display")?;

    let mut frame = Text::new(text)
        .with_font_size(FontSize::ExtraLarge24)
        .with_max_width(400)
        .with_alignment(Alignment::Left)
        .with_position(1, 30)
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
async fn display_qr_code<'a, 'b>(
    display: &'a mut Display<
        'static,
        SpiV2<'static, Async>,
        esp_hal::gpio::Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        display::OFF,
    >,
    url: &'b str,
) -> Result<(), &'static str> {
    log::info!("Updating display with QR code");

    // Calculate the optimal scale that fits within display bounds
    const MAX_SCALE: u32 = 7;
    const MIN_SCALE: u32 = 1;

    // Try maximum scale first - if it fits, we're done with one QR generation
    let qr = url::Qr::new(url).with_scale(MAX_SCALE);
    let (qr, qr_size) = if let Some(max_size) = qr.size() {
        if max_size <= DISPLAY_WIDTH && max_size <= DISPLAY_HEIGHT {
            // Maximum scale fits, use it!
            (qr, max_size)
        } else {
            // Need to calculate smaller scale
            // Generate at scale 1 to get base module count
            let base_qr = url::Qr::new(url).with_scale(MIN_SCALE);
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
            let qr = url::Qr::new(url).with_scale(calculated_scale);
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

    let mut frame = qr.to_frame::<DISPLAY_WIDTH, DISPLAY_HEIGHT, DISPLAY_SIZE_IN_BYTES>();

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

/// Update the e-ink display with raw binary data from JSON payload
/// Expected JSON format: {"frame": "base64_encoded_string", "requires_response": true/false}
/// The base64 data is decoded in-place into the json_data buffer without additional allocations
/// Strategy: Move base64 string to end of buffer, then decode to beginning (avoids overlap)
/// The data is RLE compressed: [count, byte, count, byte, ...]
async fn display_raw_binary<'a>(
    display: &'a mut Display<
        'static,
        SpiV2<'static, Async>,
        esp_hal::gpio::Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        display::OFF,
    >,
    json_data: &mut [u8],
) -> Result<(), &'static str> {
    log::info!("Parsing JSON payload: {} bytes", json_data.len());

    // Use the decoding crate to handle JSON parsing, base64 decoding, and RLE decompression
    let (decompressed_len, requires_response) = decoding::decode_json_rle_base64(json_data)
        .map_err(|e| {
            log::error!("Decoding error: {}", e);
            e
        })?;

    log::info!(
        "Decompressed {} bytes, requires_response: {}",
        decompressed_len,
        requires_response
    );

    if decompressed_len != DISPLAY_SIZE_IN_BYTES {
        log::error!(
            "Invalid framebuffer size: expected {} bytes, got {} bytes",
            DISPLAY_SIZE_IN_BYTES,
            decompressed_len
        );
        return Err("Invalid framebuffer size");
    }

    // Use the decompressed data from json_data buffer for display update
    let frame_data = &mut json_data[..decompressed_len];

    let mut display_on = display
        .on()
        .await
        .map_err(|_| "Failed to turn on display")?;

    display_on
        .update_and_save_frame::<FlashStorage>(&mut frame_data.match_size(0x00), true)
        .await
        .map_err(|_| "Failed to update display")?;

    display_on
        .off()
        .await
        .map_err(|_| "Failed to turn off display")?;

    log::info!("Successfully displayed raw binary data");
    Ok(())
}
