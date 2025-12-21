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

const DISPLAY_WIDTH: u32 = 400;
const DISPLAY_HEIGHT: u32 = 300;

const DISPLAY_SIZE_IN_BYTES: usize = (DISPLAY_WIDTH * DISPLAY_HEIGHT / 8) as usize; // 15,000 bytes

// Buffer size for raw binary display data
// Must be >= DISPLAY_SIZE_IN_BYTES to hold decompressed frame after in-place decoding
// The JSON payload (~3KB compressed) expands to DISPLAY_SIZE_IN_BYTES (15KB) during decoding
const RAW_DISPLAY_BUFFER_SIZE: usize = DISPLAY_SIZE_IN_BYTES + 360; // 15KB + 360 bytes safety margin

// Compile-time assertion: Buffer must be large enough for decompressed display data
const _: () = assert!(
    RAW_DISPLAY_BUFFER_SIZE >= DISPLAY_SIZE_IN_BYTES,
    "RAW_DISPLAY_BUFFER_SIZE must be >= DISPLAY_SIZE_IN_BYTES for in-place decoding"
);

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

/// Chunk reassembly state for multi-chunk frame data
struct ChunkState {
    /// Buffer for assembling chunks
    buffer: [u8; DISPLAY_SIZE_IN_BYTES],
    /// Expected total number of chunks
    total_chunks: usize,
    /// Number of chunks received so far
    received_count: usize,
    /// Bitmap to track which chunks have been received
    received_chunks: [bool; 32], // Support up to 32 chunks (15KB / 512 bytes per chunk ≈ 30 chunks)
}

impl ChunkState {
    const fn new() -> Self {
        Self {
            buffer: [0; DISPLAY_SIZE_IN_BYTES],
            total_chunks: 0,
            received_count: 0,
            received_chunks: [false; 32],
        }
    }

    fn reset(&mut self) {
        self.buffer.fill(0);
        self.total_chunks = 0;
        self.received_count = 0;
        self.received_chunks.fill(false);
    }

    fn is_complete(&self) -> bool {
        self.received_count > 0 && self.received_count == self.total_chunks
    }
}

// Static buffer for all display data protected by mutex
// OPTIMIZATION: Single unified buffer for all display types since messages are processed sequentially
// - Text/URL use first DISPLAY_TEXT_BUFFER_LENGTH bytes (512)
// - Raw binary uses full RAW_DISPLAY_BUFFER_SIZE bytes (15,360)
static UNIFIED_DISPLAY_BUFFER: Mutex<CriticalSectionRawMutex, [u8; RAW_DISPLAY_BUFFER_SIZE]> =
    Mutex::new([0; RAW_DISPLAY_BUFFER_SIZE]);

// Chunk reassembly state for raw binary data
static CHUNK_STATE: Mutex<CriticalSectionRawMutex, ChunkState> =
    Mutex::new(ChunkState::new());

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

/// Update the e-ink display with raw binary data from JSON chunked payload
/// Expected JSON format: {"frame": "base64_encoded_string", "requires_response": true/false, "chunk_index": 0, "total_chunks": 1}
/// The base64 data is decoded in-place into the json_data buffer without additional allocations
/// Strategy: Move base64 string to end of buffer, then decode to beginning (avoids overlap)
/// The data is RLE compressed: [count, byte, count, byte, ...]
/// Chunks are reassembled in a static buffer until all chunks are received
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
    log::info!("Parsing JSON chunk payload: {} bytes", json_data.len());

    // Use the decoding crate to handle JSON parsing, base64 decoding, and RLE decompression
    let (chunk_len, metadata) = decoding::decode_chunk(json_data).map_err(|e| {
        log::error!("Chunk decoding error: {}", e);
        e
    })?;

    log::info!(
        "Decoded chunk {}/{}: {} bytes, requires_response: {}",
        metadata.chunk_index + 1,
        metadata.total_chunks,
        chunk_len,
        metadata.requires_response
    );

    // Acquire chunk state lock
    let mut chunk_state = CHUNK_STATE.lock().await;

    // Check if this is a new frame (first chunk or reset)
    if metadata.chunk_index == 0 || chunk_state.total_chunks != metadata.total_chunks {
        log::info!("Starting new frame with {} chunks", metadata.total_chunks);
        chunk_state.reset();
        chunk_state.total_chunks = metadata.total_chunks;
    }

    // Validate chunk index
    if metadata.chunk_index >= metadata.total_chunks {
        log::error!(
            "Invalid chunk index: {} >= {}",
            metadata.chunk_index,
            metadata.total_chunks
        );
        return Err("Invalid chunk index");
    }

    // Check if we already received this chunk
    if chunk_state.received_chunks[metadata.chunk_index] {
        log::warn!("Duplicate chunk {} received, ignoring", metadata.chunk_index);
        return Ok(());
    }

    // Calculate chunk offset in the final buffer
    // For even distribution, we need to know the expected chunk size
    // Since we're dealing with RLE-compressed data, chunks may vary in size
    // We'll store chunk offset information or use a simple append strategy

    // For simplicity, let's assume chunks are relatively equal in size
    let chunk_size_estimate = DISPLAY_SIZE_IN_BYTES / metadata.total_chunks;
    let offset = metadata.chunk_index * chunk_size_estimate;

    // Copy chunk data to reassembly buffer
    let copy_len = core::cmp::min(chunk_len, DISPLAY_SIZE_IN_BYTES - offset);
    if offset + copy_len > DISPLAY_SIZE_IN_BYTES {
        log::error!(
            "Chunk data exceeds buffer: offset={}, len={}, buffer={}",
            offset,
            copy_len,
            DISPLAY_SIZE_IN_BYTES
        );
        return Err("Chunk data exceeds buffer size");
    }

    chunk_state.buffer[offset..offset + copy_len].copy_from_slice(&json_data[..copy_len]);
    chunk_state.received_chunks[metadata.chunk_index] = true;
    chunk_state.received_count += 1;

    log::info!(
        "Chunk {}/{} received and stored",
        chunk_state.received_count,
        chunk_state.total_chunks
    );

    // Check if all chunks have been received
    if !chunk_state.is_complete() {
        log::info!(
            "Waiting for more chunks ({}/{})",
            chunk_state.received_count,
            chunk_state.total_chunks
        );
        return Ok(());
    }

    log::info!("All chunks received, displaying frame");

    // All chunks received, display the frame
    let mut display_on = display
        .on()
        .await
        .map_err(|_| "Failed to turn on display")?;

    // Create a mutable slice from the chunk buffer
    let frame_data = &mut chunk_state.buffer[..];

    display_on
        .update_and_save_frame::<FlashStorage>(&mut frame_data.match_size(0x00), true)
        .await
        .map_err(|_| "Failed to update display")?;

    display_on
        .off()
        .await
        .map_err(|_| "Failed to turn off display")?;

    log::info!("Successfully displayed reassembled frame");

    // Reset chunk state for next frame
    chunk_state.reset();

    Ok(())
}
