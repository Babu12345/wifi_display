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
const DISPLAY_UPDATE_DELAY_MS: u64 = 20; // Minimum delay between display updates
const DISPLAY_TEXT_BUFFER_LENGTH: usize = 512;
const DISPLAY_URL_BUFFER_LENGTH: usize = 256;
// URL buffer starts after text buffer in the unified buffer
const DISPLAY_URL_BUFFER_OFFSET: usize = DISPLAY_TEXT_BUFFER_LENGTH;

const DISPLAY_WIDTH: u32 = 400;
const DISPLAY_HEIGHT: u32 = 300;

const DISPLAY_SIZE_IN_BYTES: usize = (DISPLAY_WIDTH * DISPLAY_HEIGHT / 8) as usize; // 15,000 bytes

/// Messages that can be sent to the display task
#[derive(Debug, Clone, Copy)]
pub enum DisplayMessage {
    /// Display text on the screen (via NFC)
    Text,
    /// Display a QR code from URL (via NFC)
    QRCode,
    /// Display text with a QR code (text on left, QR on right)
    TextWithQR,
    /// Display raw binary data (complete frame from MQTT)
    RawBinary,
    /// Update max cycles before full refresh
    SetMaxCycles(u8),
}

// Static buffer to transmit display data from the different tasks protected by a Mutex
// Sized to hold a complete display frame (15,000 bytes)
static UNIFIED_DISPLAY_BUFFER: Mutex<CriticalSectionRawMutex, [u8; DISPLAY_SIZE_IN_BYTES]> =
    Mutex::new([0; DISPLAY_SIZE_IN_BYTES]);

/// Processes display update requests from a channel and renders to the e-ink display
///
/// Supports three message types:
/// - Text: Renders text with configurable font size and alignment
/// - QRCode: Generates and displays QR codes from URLs
/// - RawBinary: Decodes and displays raw frame data (base64 + RLE compressed)
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

        // Process the message
        match message {
            DisplayMessage::Text => {
                indicator.toggle();
                let mut buf = UNIFIED_DISPLAY_BUFFER.lock().await;
                // Only use first DISPLAY_TEXT_BUFFER_LENGTH bytes for text
                let text_slice = &buf[..DISPLAY_TEXT_BUFFER_LENGTH];
                let len = text_slice
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(text_slice.len());
                match core::str::from_utf8(&text_slice[..len]) {
                    Ok(text) if !text.is_empty() => {
                        // Copy text to stack before reusing buffer for rendering
                        let mut text_copy = heapless::String::<DISPLAY_TEXT_BUFFER_LENGTH>::new();
                        let _ = text_copy.push_str(text);
                        match display_text(&mut display, &text_copy, &mut buf).await {
                            Ok(_) => log::info!("Successfully displayed text"),
                            Err(e) => log::error!("Error displaying text: {:?}", e),
                        }
                    }
                    Ok(_) => {} // Empty text, do nothing
                    Err(_) => {
                        log::error!("Invalid UTF-8 in text buffer");
                    }
                }
                indicator.toggle();
            }
            DisplayMessage::QRCode => {
                indicator.toggle();
                let mut buf = UNIFIED_DISPLAY_BUFFER.lock().await;
                // Only use first DISPLAY_TEXT_BUFFER_LENGTH bytes for URL
                let url_slice = &buf[..DISPLAY_TEXT_BUFFER_LENGTH];
                let len = url_slice
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(url_slice.len());
                match core::str::from_utf8(&url_slice[..len]) {
                    Ok(url) if !url.is_empty() => {
                        // Copy URL to stack before reusing buffer for rendering
                        let mut url_copy = heapless::String::<DISPLAY_TEXT_BUFFER_LENGTH>::new();
                        let _ = url_copy.push_str(url);
                        match display_qr_code(&mut display, &url_copy, &mut buf).await {
                            Ok(_) => log::info!("Successfully displayed QR code"),
                            Err(e) => log::error!("Error displaying QR code: {:?}", e),
                        }
                    }
                    Ok(_) => {} // Empty URL, do nothing
                    Err(_) => {
                        log::error!("Invalid UTF-8 in URL buffer");
                    }
                }
                indicator.toggle();
            }
            DisplayMessage::TextWithQR => {
                indicator.toggle();
                let mut buf = UNIFIED_DISPLAY_BUFFER.lock().await;
                // Text is in first DISPLAY_TEXT_BUFFER_LENGTH bytes
                let text_slice = &buf[..DISPLAY_TEXT_BUFFER_LENGTH];
                let text_len = text_slice
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(text_slice.len());
                // URL is in next DISPLAY_URL_BUFFER_LENGTH bytes
                let url_slice =
                    &buf[DISPLAY_URL_BUFFER_OFFSET..DISPLAY_URL_BUFFER_OFFSET + DISPLAY_URL_BUFFER_LENGTH];
                let url_len = url_slice
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(url_slice.len());

                match (
                    core::str::from_utf8(&text_slice[..text_len]),
                    core::str::from_utf8(&url_slice[..url_len]),
                ) {
                    (Ok(text), Ok(url)) if !text.is_empty() && !url.is_empty() => {
                        // Copy to stack before reusing buffer
                        let mut text_copy = heapless::String::<DISPLAY_TEXT_BUFFER_LENGTH>::new();
                        let _ = text_copy.push_str(text);
                        let mut url_copy = heapless::String::<DISPLAY_URL_BUFFER_LENGTH>::new();
                        let _ = url_copy.push_str(url);
                        match display_text_with_qr(&mut display, &text_copy, &url_copy, &mut buf)
                            .await
                        {
                            Ok(_) => log::info!("Successfully displayed text with QR"),
                            Err(e) => log::error!("Error displaying text with QR: {:?}", e),
                        }
                    }
                    _ => {
                        log::error!("Invalid UTF-8 or empty content in text/URL buffer");
                    }
                }
                indicator.toggle();
            }
            DisplayMessage::RawBinary => {
                let buf = UNIFIED_DISPLAY_BUFFER.lock().await;

                log::info!("Displaying complete frame: {} bytes", DISPLAY_SIZE_IN_BYTES);
                match display_raw_binary(&mut display, &buf[..]).await {
                    Ok(_) => log::info!("Successfully displayed raw binary"),
                    Err(e) => log::error!("Error displaying raw binary: {:?}", e),
                }
            }
            DisplayMessage::SetMaxCycles(cycles) => {
                log::info!("Setting max cycles to {}", cycles);
                display.set_max_cycles(cycles);
            }
        }

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

/// Store text and URL in the display buffer and send message to display both
/// Text will be displayed on the left side with a smaller font, QR code on the right
pub fn queue_text_with_qr_display(
    channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
    text: &str,
    url: &str,
) {
    if let Ok(mut buf) = UNIFIED_DISPLAY_BUFFER.try_lock() {
        // Store text in first section
        buf[..DISPLAY_TEXT_BUFFER_LENGTH].fill(0);
        let text_bytes = text.as_bytes();
        let text_len = core::cmp::min(text_bytes.len(), DISPLAY_TEXT_BUFFER_LENGTH);
        buf[..text_len].copy_from_slice(&text_bytes[..text_len]);

        // Store URL in second section
        buf[DISPLAY_URL_BUFFER_OFFSET..DISPLAY_URL_BUFFER_OFFSET + DISPLAY_URL_BUFFER_LENGTH]
            .fill(0);
        let url_bytes = url.as_bytes();
        let url_len = core::cmp::min(url_bytes.len(), DISPLAY_URL_BUFFER_LENGTH);
        buf[DISPLAY_URL_BUFFER_OFFSET..DISPLAY_URL_BUFFER_OFFSET + url_len]
            .copy_from_slice(&url_bytes[..url_len]);
    } else {
        log::warn!("Display buffer locked, skipping update");
        return;
    }

    // Try to send, drop oldest message if channel is full
    if channel.try_send(DisplayMessage::TextWithQR).is_err() {
        log::warn!("Display channel full, message may be dropped");
    }
}

/// Append chunk data directly to the display buffer at the given offset
/// Returns the number of bytes written, or None if buffer is locked
pub fn append_to_display_buffer(data: &[u8], offset: usize) -> Option<usize> {
    if let Ok(mut buf) = UNIFIED_DISPLAY_BUFFER.try_lock() {
        let available = buf.len().saturating_sub(offset);
        let copy_len = core::cmp::min(data.len(), available);
        if copy_len > 0 {
            buf[offset..offset + copy_len].copy_from_slice(&data[..copy_len]);
        }
        Some(copy_len)
    } else {
        log::warn!("Display buffer locked");
        None
    }
}

/// Reset the display buffer (call when starting a new frame)
pub fn reset_display_buffer() {
    if let Ok(mut buf) = UNIFIED_DISPLAY_BUFFER.try_lock() {
        buf.fill(0);
    }
}

/// Signal that the complete frame is ready for display
/// Returns true if successfully queued, false if channel was full
pub fn queue_frame_ready(
    channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
) -> bool {
    if channel.try_send(DisplayMessage::RawBinary).is_err() {
        log::warn!("Display channel full, frame may be dropped");
        false
    } else {
        true
    }
}

/// Queue a max cycles update for the display
pub fn queue_set_max_cycles(
    channel: &'static Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
    cycles: u8,
) {
    if channel
        .try_send(DisplayMessage::SetMaxCycles(cycles))
        .is_err()
    {
        log::warn!("Display channel full, max cycles update may be dropped");
    }
}

/// Update the e-ink display with text
/// Reuses the provided buffer for rendering to avoid extra static allocation
async fn display_text(
    display: &mut Display<
        'static,
        SpiV2<'static, Async>,
        esp_hal::gpio::Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        display::OFF,
    >,
    text: &str,
    frame: &mut [u8; DISPLAY_SIZE_IN_BYTES],
) -> Result<(), &'static str> {
    log::info!("Updating display with text");

    let mut display_on = display
        .on(false)
        .await
        .map_err(|_| "Failed to turn on display")?;

    // Render text directly into the provided buffer
    Text::new(text)
        .with_font_size(FontSize::ExtraLarge24)
        .with_max_width(400)
        .with_alignment(Alignment::Left)
        .with_position(1, 30)
        .render_to_buffer::<DISPLAY_WIDTH, DISPLAY_HEIGHT, DISPLAY_SIZE_IN_BYTES>(frame);

    display_on
        .update_and_save_frame::<FlashStorage>(frame, true)
        .await
        .map_err(|_| "Failed to update display")?;

    display_on
        .off(false)
        .await
        .map_err(|_| "Failed to turn off display")?;

    Ok(())
}

/// Update the e-ink display with a QR code from URL
/// Reuses the provided buffer for rendering to avoid extra static allocation
async fn display_qr_code(
    display: &mut Display<
        'static,
        SpiV2<'static, Async>,
        esp_hal::gpio::Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        display::OFF,
    >,
    url: &str,
    frame: &mut [u8; DISPLAY_SIZE_IN_BYTES],
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
        .on(false)
        .await
        .map_err(|_| "Failed to turn on display")?;

    // Render QR code directly into the provided buffer
    qr.render_to_buffer::<DISPLAY_WIDTH, DISPLAY_HEIGHT, DISPLAY_SIZE_IN_BYTES>(frame);

    display_on
        .update_and_save_frame::<FlashStorage>(frame, true)
        .await
        .map_err(|_| "Failed to update display")?;

    display_on
        .off(false)
        .await
        .map_err(|_| "Failed to turn off display")?;

    Ok(())
}

/// Update the e-ink display with text and a QR code side by side
/// Text is rendered on the left with a smaller font, QR code on the right
async fn display_text_with_qr(
    display: &mut Display<
        'static,
        SpiV2<'static, Async>,
        esp_hal::gpio::Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        display::OFF,
    >,
    text: &str,
    url: &str,
    frame: &mut [u8; DISPLAY_SIZE_IN_BYTES],
) -> Result<(), &'static str> {
    log::info!("Updating display with text and QR code");

    // Layout: Text on left (about 55% width), QR on right (about 45% width)
    // Display is 400x300
    const TEXT_WIDTH: u32 = 220; // Left side for text
    const QR_AREA_WIDTH: u32 = 180; // Right side for QR
    const QR_MAX_SIZE: u32 = 160; // Max QR size to leave some margin

    // Calculate QR code scale to fit in right area
    let base_qr = url::Qr::new(url).with_scale(1);
    let base_size = base_qr.size().ok_or("Failed to generate QR code")?;

    // Calculate scale that fits in QR area
    let max_scale = QR_MAX_SIZE / base_size;
    let scale = if max_scale < 1 { 1 } else { max_scale };
    let qr_size = base_size * scale;

    // Position QR code centered in right area
    let qr_x = TEXT_WIDTH + ((QR_AREA_WIDTH - qr_size) / 2);
    let qr_y = (DISPLAY_HEIGHT - qr_size) / 2;

    let mut display_on = display
        .on(false)
        .await
        .map_err(|_| "Failed to turn on display")?;

    // First render text on the left side (this clears the buffer)
    Text::new(text)
        .with_font_size(FontSize::Medium6x12)
        .with_max_width(TEXT_WIDTH - 10) // Leave small margin
        .with_alignment(Alignment::Left)
        .with_position(5, 20)
        .render_to_buffer::<DISPLAY_WIDTH, DISPLAY_HEIGHT, DISPLAY_SIZE_IN_BYTES>(frame);

    // Then overlay QR code on the right side (doesn't clear)
    url::Qr::new(url)
        .with_position(qr_x as i32, qr_y as i32)
        .with_scale(scale)
        .render_to_buffer_overlay::<DISPLAY_WIDTH, DISPLAY_HEIGHT, DISPLAY_SIZE_IN_BYTES>(frame);

    display_on
        .update_and_save_frame::<FlashStorage>(frame, true)
        .await
        .map_err(|_| "Failed to update display")?;

    display_on
        .off(false)
        .await
        .map_err(|_| "Failed to turn off display")?;

    Ok(())
}

/// Update the e-ink display with complete raw frame data
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
    frame_data: &[u8],
) -> Result<(), &'static str> {
    log::info!("Rendering frame to display");

    let mut display_on = display
        .on(false)
        .await
        .map_err(|_| "Failed to turn on display")?;

    display_on
        .update_and_save_frame::<FlashStorage>(&mut frame_data.match_size(0x00), true)
        .await
        .map_err(|_| "Failed to update display")?;

    display_on
        .off(false)
        .await
        .map_err(|_| "Failed to turn off display")?;
    Ok(())
}
