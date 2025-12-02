//! Display handler task for rate-limited display updates

use crate::spi::SpiV2;
use display::{Display, EPD417, EPD417_SIZE};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};
use esp_hal::{Async, gpio::Output};
use esp_storage::FlashStorage;
use text::{Alignment, FontSize, Text};

const DISPLAY_TEXT_BUFFER_LENGTH: usize = 512;
/// Size of the display message channel
pub const DISPLAY_CHANNEL_SIZE: usize = 5;
const DISPLAY_UPDATE_DELAY_MS: u64 = 200; // Minimum delay between display updates

/// Messages that can be sent to the display task
#[derive(Debug, Clone, Copy)]
pub enum DisplayMessage {
    /// Display text on the screen
    Text,
    /// Display a QR code from URL
    QRCode,
}

// Static buffers for display data shared between tasks
static mut DISPLAY_TEXT_BUFFER: [u8; DISPLAY_TEXT_BUFFER_LENGTH] = [0; DISPLAY_TEXT_BUFFER_LENGTH];
static mut DISPLAY_URL_BUFFER: [u8; DISPLAY_TEXT_BUFFER_LENGTH] = [0; DISPLAY_TEXT_BUFFER_LENGTH];

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
                let text = unsafe {
                    use core::ptr::addr_of_mut;
                    let buf = &*addr_of_mut!(DISPLAY_TEXT_BUFFER);
                    // Find the null terminator or end of buffer
                    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    if let Ok(s) = core::str::from_utf8(&buf[..len]) {
                        s
                    } else {
                        log::error!("Invalid UTF-8 in text buffer");
                        continue;
                    }
                };

                if !text.is_empty() {
                    match display_text(&mut display, text).await {
                        Ok(_) => log::info!("Successfully displayed text"),
                        Err(e) => log::error!("Error displaying text: {:?}", e),
                    }
                }
            }
            DisplayMessage::QRCode => {
                let url = unsafe {
                    use core::ptr::addr_of_mut;
                    let buf = &*addr_of_mut!(DISPLAY_URL_BUFFER);
                    // Find the null terminator or end of buffer
                    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    if let Ok(s) = core::str::from_utf8(&buf[..len]) {
                        s
                    } else {
                        log::error!("Invalid UTF-8 in URL buffer");
                        continue;
                    }
                };

                if !url.is_empty() {
                    match display_qr_code(&mut display, url).await {
                        Ok(_) => log::info!("Successfully displayed QR code"),
                        Err(e) => log::error!("Error displaying QR code: {:?}", e),
                    }
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
    unsafe {
        use core::ptr::addr_of_mut;
        let buf = &mut *addr_of_mut!(DISPLAY_TEXT_BUFFER);
        buf.fill(0);
        let text_bytes = text.as_bytes();
        let len = core::cmp::min(text_bytes.len(), buf.len());
        buf[..len].copy_from_slice(&text_bytes[..len]);
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
    unsafe {
        use core::ptr::addr_of_mut;
        let buf = &mut *addr_of_mut!(DISPLAY_URL_BUFFER);
        buf.fill(0);
        let url_bytes = url.as_bytes();
        let len = core::cmp::min(url_bytes.len(), buf.len());
        buf[..len].copy_from_slice(&url_bytes[..len]);
    }

    // Try to send, drop oldest message if channel is full
    if channel.try_send(DisplayMessage::QRCode).is_err() {
        log::warn!("Display channel full, message may be dropped");
    }
}

/// Update the e-ink display with text
async fn display_text<'a>(
    display: &'a mut Display<
        'static,
        SpiV2<'static, Async>,
        esp_hal::gpio::Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        display::OFF,
    >,
    text: &str,
) -> Result<(), &'static str> {
    log::info!("Updating display");

    // Use a static buffer that we reuse each time
    static mut DISPLAY_TEXT_BUFFER_INTERNAL: [u8; DISPLAY_TEXT_BUFFER_LENGTH] =
        [0; DISPLAY_TEXT_BUFFER_LENGTH];
    let static_text = unsafe {
        use core::ptr::addr_of_mut;
        let buf = &mut *addr_of_mut!(DISPLAY_TEXT_BUFFER_INTERNAL);
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
async fn display_qr_code<'a>(
    display: &'a mut Display<
        'static,
        SpiV2<'static, Async>,
        esp_hal::gpio::Input<'static>,
        Output<'static>,
        EPD417_SIZE,
        EPD417,
        display::OFF,
    >,
    url: &str,
) -> Result<(), &'static str> {
    log::info!("Updating display with QR code");

    // Store URL in static memory since Qr::new requires &'static str
    static mut URL_BUFFER_INTERNAL: [u8; DISPLAY_TEXT_BUFFER_LENGTH] = [0; DISPLAY_TEXT_BUFFER_LENGTH];
    let static_url = unsafe {
        use core::ptr::addr_of_mut;
        let buf = &mut *addr_of_mut!(URL_BUFFER_INTERNAL);
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
