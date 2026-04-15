# OTA Firmware Update Plan

Over-the-air firmware updates for the ESP32-C3 wifi display, integrated with the existing MQTT + TLS infrastructure and secure boot signing.

---

## Current State

| Item | Value |
|------|-------|
| Flash size | 8 MB (0x800000) |
| Firmware binary | ~1.2 MB |
| Current partition layout | Single `factory` app partition (0x020000–0x310000, ~2.9 MB) |
| User data storage | Starts at 0x310000 (defined in `display_storage` crate) |
| Secure boot | V2 (RSA-3072), firmware must be signed |
| Flash encryption | Supported (dev/release modes) |
| Existing transport | MQTT over TLS (AWS IoT, port 8883) |

---

## Phase 1: Partition Table Changes

Replace the single `factory` partition with two OTA app partitions and an `otadata` partition.

### New `partitions_secure.csv`

```csv
# ESP32-C3 Partition Table for Secure Boot V2 + OTA
# Name,   Type, SubType,  Offset,    Size,     Flags
nvs,      data, nvs,      0x011000,  0x6000,
phy_init, data, phy,      0x017000,  0x1000,
otadata,  data, ota,      0x018000,  0x2000,
ota_0,    app,  ota_0,    0x020000,  0x178000,
ota_1,    app,  ota_1,    0x198000,  0x178000,
```

**Layout rationale:**
- `otadata` (8 KB at 0x018000): Tracks which OTA slot is active. Must be `data, ota` subtype.
- `ota_0` (1.5 MB): Primary app slot. 1.5 MB gives ~300 KB headroom over current 1.2 MB binary.
- `ota_1` (1.5 MB): Secondary app slot for OTA writes.
- Both OTA partitions end at 0x310000, preserving the existing user data storage boundary.
- **No changes needed to the `display_storage` crate.**

### New `partitions.csv` (development mode)

```csv
# Name,   Type, SubType,  Offset,    Size,     Flags
nvs,      data, nvs,      0x009000,  0x6000,
otadata,  data, ota,      0x00F000,  0x2000,
ota_0,    app,  ota_0,    0x010000,  0x180000,
ota_1,    app,  ota_1,    0x190000,  0x180000,
```

### Bootloader rebuild required

After changing partition tables, the esp-idf bootloader must be rebuilt:
```bash
./main/build-bootloader.sh --clean
```

The bootloader must have OTA support enabled (it does by default when `otadata` + `ota_0`/`ota_1` partitions are present).

---

## Phase 2: OTA Download Mechanism

Use MQTT as the **trigger** and HTTPS as the **transport**. MQTT delivers a small message with the firmware URL; the device downloads the binary over HTTPS with full TCP reliability.

### Why HTTPS instead of MQTT chunks

The existing MQTT chunked pattern works well for display frames (~15-50 KB), but firmware is ~1.2 MB (~300 chunks at QoS0). A single lost chunk means restarting the entire transfer. HTTPS gives TCP-level reliability (retransmits, ordering, flow control) for free, and the TLS stack (`esp-mbedtls`) is already in use.

### Flow

```
1. Server publishes to  {client_id}/root/ota  via MQTT:
   { "url": "https://fw.example.com/v1.2/firmware.bin", "version": "1.2.0", "size": 1258000 }

2. Device receives MQTT message, validates fields

3. Device opens HTTPS connection to the URL
   - Reuses existing TLS stack (esp-mbedtls)
   - Downloads in 4 KB chunks
   - Writes each chunk directly to the inactive OTA partition

4. After full download, device:
   - Verifies total bytes written matches expected size
   - Sets the new partition as boot target
   - Sends MQTT ACK: { "ota_status": "success", "version": "1.2.0" }
   - Reboots

5. New firmware boots, validates (connects to WiFi + MQTT), calls mark_valid()
   - If validation fails after N boots, bootloader auto-rolls back
```

### MQTT Topic

Add a new reserved topic alongside `raw`, `config`, and `ping`:

```
{client_id}/root/ota
```

Update the static topic count from 7 to 8 (4 reserved + 4 dynamic) in `MqttClient::<_, 8, _>::new(...)`.

### OTA Trigger Message Schema

```json
{
  "url": "https://fw.example.com/v1.2.0/firmware-signed.bin",
  "version": "1.2.0",
  "size": 1258000,
  "crc32": 2820145897
}
```

- `url`: HTTPS endpoint hosting the signed firmware binary (e.g., S3 presigned URL)
- `version`: Human-readable version string (for logging/ACK)
- `size`: Expected size in bytes (for validation before finalizing)
- `crc32`: CRC32 checksum of the firmware binary (verified after flash write)

---

## Phase 3: Implementation

### New module: `main/src/ota.rs`

Responsibilities:
1. Parse OTA trigger message (deserialize JSON from MQTT payload)
2. Open HTTPS GET connection to firmware URL
3. Find the inactive OTA partition
4. Stream response body in 4 KB chunks, writing each to flash
5. Validate total bytes written against expected size
6. Set new partition as boot target
7. Return result for MQTT ACK

### Key APIs

| Operation | Crate / API |
|-----------|-------------|
| Find inactive OTA partition | `esp-ota::Ota::begin()` |
| Write chunks to partition | `esp-ota::OtaUpdate::write()` |
| Finalize and set boot target | `esp-ota::OtaUpdate::finalize()` |
| Confirm new firmware on first boot | `esp-ota::Ota::mark_valid()` |
| HTTPS download | `esp-mbedtls` (already in use for MQTT TLS) |
| Reboot | `esp_hal::reset::software_reset()` |

### Integration with `task_wifi_runner.rs`

In the `select3` message handler, add a match arm for the OTA topic:

```rust
t if t == ota_topic.as_str() => {
    log::info!("OTA update triggered");
    match ota::perform_update(payload, stack, tls).await {
        Ok(version) => {
            // Publish ACK
            let ack = b"{\"ota_status\":\"success\"}";
            client.send_message(ota_topic.as_str(), ack, ...).await.ok();
            // Disconnect cleanly and reboot
            client.disconnect().await.ok();
            esp_hal::reset::software_reset();
        }
        Err(e) => {
            log::error!("OTA failed: {}", e);
            let nack = b"{\"ota_status\":\"error\"}";
            client.send_message(ota_topic.as_str(), nack, ...).await.ok();
        }
    }
}
```

### First-boot validation

At the top of `task_wifi_runner_inner`, before entering the main loop, add:

```rust
// If we just booted from a new OTA partition, confirm it's valid
// once we successfully connect to WiFi + MQTT
if ota::is_pending_verification() {
    ota::mark_valid();
    log::info!("OTA firmware validated");
}
```

Call `mark_valid()` **after** WiFi + MQTT connect succeeds. If the new firmware can't connect, the watchdog/boot count will trigger automatic rollback to the previous partition.

---

## Phase 4: Secure Boot Compatibility

The downloaded firmware binary **must be signed** before being hosted. The OTA write path does not need to handle signing — it writes the pre-signed binary as-is.

### Build and upload workflow (server-side)

```bash
cargo build --release
espflash save-image --chip esp32c3 target/riscv32imc-unknown-none-elf/release/main firmware.bin
espsecure.py sign_data --version 2 --keyfile secure_boot_signing_key.pem firmware.bin
# Upload signed firmware.bin to HTTPS hosting (S3, etc.)
# Then publish MQTT trigger with the URL
```

The secure bootloader verifies the signature on boot. If the downloaded binary is unsigned or signed with the wrong key, the bootloader rejects it and rolls back automatically.

### Flash encryption

If flash encryption is enabled, the OTA write API (`esp-ota`) handles transparent encryption — you write plaintext and the hardware encrypts on the fly. No special handling needed.

---

## Phase 5: Error Handling and Safety

| Scenario | Behavior |
|----------|----------|
| Download interrupted (WiFi drop) | OTA partition left in incomplete state; not marked as boot target; device continues on current firmware |
| Size mismatch after download | Don't finalize; log error; send NACK via MQTT |
| New firmware fails to connect | Bootloader rollback after N failed boots (configurable via `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`) |
| Power loss during OTA write | Incomplete write; `otadata` still points to old partition; safe |
| Power loss during reboot | Bootloader loads whichever partition `otadata` points to; safe |
| Invalid signature (secure boot) | Bootloader rejects new firmware; rolls back; safe |

### Rollback configuration

Enable in esp-idf menuconfig when rebuilding the bootloader:
- **Bootloader config → Enable app rollback support** → Yes
- This makes the bootloader track boot attempts and auto-revert if `mark_valid()` is never called

---

## Implementation Order

1. **Partition table** — Update both CSV files, rebuild bootloader, re-flash
2. **`ota.rs` module** — HTTPS download + OTA write logic
3. **MQTT integration** — Add OTA topic, trigger handler, ACK/NACK
4. **First-boot validation** — `mark_valid()` after successful MQTT connection
5. **Server-side** — Build pipeline to sign + upload firmware to S3, MQTT trigger script
6. **Testing** — Test with development mode first (UART recovery available), then production

---

## Dependencies to Add

```toml
# In main/Cargo.toml
esp-hal-ota = { version = "0.4.6", features = ["esp32c3", "log"] }
ota = { path = "../ota" }  # Hardware-agnostic OTA crate
```

---

## References

- [ESP-IDF OTA Documentation](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-reference/system/ota.html)
- [ESP32-C3 Secure Boot V2 + OTA](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/security/secure-boot-v2.html#ota-updates)
- [Anti-rollback protection](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-reference/system/ota.html#anti-rollback)
