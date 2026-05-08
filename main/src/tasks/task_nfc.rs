//! NFC tag monitoring and data processing

use crate::AppFlashStorage as FlashStorage;
use crate::NotificationType;
use crate::tasks::{format_registration_response, is_registration_response};
use crate::{NUM_NFC_CHANGE_RECEIVERS, NUM_NOTIFICATION_RECEIVERS};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, watch::Sender};
use embassy_time::Duration;
use embassy_time::Timer;
use esp_hal::{
    Async,
    gpio::{Input, Output},
    i2c::master::I2c,
};
use nfc::{MAX_NFCDATA_SIZE, Nfc, STM25DV64KC};
use storage::storage::PersistentStorage;

type AppNfc = Nfc<STM25DV64KC, Input<'static>, Output<'static>, I2c<'static, Async>>;

/// Write the device's registration code to the tag so any plain RF read returns
/// this device's identity. Used to seed the tag at boot and to re-assert it after
/// every content write that overwrites the tag's NDEF area.
async fn write_registration_response(nfc: &mut AppNfc, client_id: &str) {
    match nfc
        .write_text(&format_registration_response(client_id))
        .await
    {
        Ok(_) => log::info!("✓ Device registration response written"),
        Err(e) => log::warn!("Write response failed (non-critical): {e:?}"),
    }
}

/// Monitors NFC tag for new data and persists it to flash storage
///
/// Watches for RF writes to the NFC tag and parses NDEF records containing:
/// - WiFi credentials
/// - Display text
/// - URLs for QR codes
/// - Live update commands
#[embassy_executor::task]
pub async fn task_nfc(
    mut nfc: AppNfc,
    notification: Sender<'static, NoopRawMutex, NotificationType, NUM_NOTIFICATION_RECEIVERS>,
    nfc_change: Sender<'static, NoopRawMutex, u32, NUM_NFC_CHANGE_RECEIVERS>,
    client_id: &'static str,
) {
    // Initialize the NFC tag (formats NDEF and enables RF write access)
    log::info!("Initializing NFC tag...");
    match nfc.initialize_for_ndef_write_access().await {
        Ok(_) => log::info!("✓ Tag initialized successfully"),
        Err(e) => {
            log::error!("✗ Initialization failed: {:?}", e);
            return;
        }
    }

    write_registration_response(&mut nfc, client_id).await;

    let mut storage = PersistentStorage::new(FlashStorage::default(), &mut []);
    let mut storage_data = [0u8; MAX_NFCDATA_SIZE];
    let mut change_counter: u32 = 0;

    'process: loop {
        if let Err(_) = nfc.depower_board().await {
            log::error!("failed to depower the board");
            continue 'process;
        }
        Timer::after_millis(50).await;
        log::info!("Ready - try writing from your phone now");
        match nfc.wait_and_get_data(Duration::from_millis(1000)).await {
            Ok(data) => match data {
                nfc::NFCData::Wifi(ref ssid, ref password) => {
                    log::info!("SSID: {ssid}, PSWD: {password}");
                    if let Err(e) = data.to_bytes(&mut storage_data) {
                        log::error!("Serialization error: {e:?}");
                        continue 'process;
                    }

                    // Write the registration response back BEFORE persisting
                    // creds — the iOS/web registration flow polls the tag for
                    // REG:C:<code>;; right after writing WiFi, and having it
                    // immediately after can speed up the registration process
                    write_registration_response(&mut nfc, client_id).await;

                    if let Err(e) = storage.write_bytes(
                        storage::storage::StorageContents::WifiCredentials,
                        0,
                        &storage_data,
                    ) {
                        log::error!("Storage error: {e:?}");
                        continue 'process;
                    }
                    notification.send(NotificationType::WifiCredentials);
                    change_counter = change_counter.wrapping_add(1);
                    nfc_change.send(change_counter);
                    log::info!(
                        "Successfully saved wifi in NVS, nfc_change={}",
                        change_counter
                    )
                }
                nfc::NFCData::Text(ref text) => {
                    log::info!("Text: {text}");

                    // Skip registration response data (written by device, not meant for display)
                    if is_registration_response(text.as_str()) {
                        log::info!("Skipping registration response data");
                        continue 'process;
                    }

                    // Regular text to display (via NFC, for low bandwidth scenarios)
                    if let Err(e) = data.to_bytes(&mut storage_data) {
                        log::error!("Serialization error: {e:?}");
                        continue 'process;
                    }
                    if let Err(e) = storage.write_bytes(
                        storage::storage::StorageContents::DisplayText,
                        0,
                        &storage_data,
                    ) {
                        log::error!("Storage error: {e:?}");
                        continue 'process;
                    }
                    notification.send(NotificationType::DisplayText);
                    change_counter = change_counter.wrapping_add(1);
                    nfc_change.send(change_counter);
                    log::info!(
                        "Sent DisplayText notification, nfc_change={}",
                        change_counter
                    );
                }
                nfc::NFCData::Uri(ref uri) => {
                    log::info!("URI: {uri}");
                    if let Err(e) = data.to_bytes(&mut storage_data) {
                        log::error!("Serialization error: {e:?}");
                        continue 'process;
                    }
                    if let Err(e) = storage.write_bytes(
                        storage::storage::StorageContents::DisplayURL,
                        0,
                        &storage_data,
                    ) {
                        log::error!("Storage error: {e:?}");
                        continue 'process;
                    }
                    notification.send(NotificationType::DisplayURL);
                    change_counter = change_counter.wrapping_add(1);
                    nfc_change.send(change_counter);
                    log::info!(
                        "Sent DisplayURL notification, nfc_change={}",
                        change_counter
                    );
                }
                nfc::NFCData::Unknown => log::error!("Unknown type"),
            },
            Err(_) => log::error!("Error occurred when NFC NDEF data"),
        }

        // Re-assert the registration code so the tag reverts to "device ID" once
        // any phone-initiated write has been processed. Skipped via `continue 'process`
        // when an arm bails out early (e.g. reading back our own response, serialization
        // error, storage failure) — in those cases re-writing isn't needed or wanted.
        write_registration_response(&mut nfc, client_id).await;
    }
}
