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
esp-hal-ota = { path = "../esp-hal-ota" }  # vendored, see note below
ota = { path = "../ota" }                  # hardware-agnostic OTA crate
```

---

## Implementation Notes (actual delta from plan)

The as-built system differs from the plan in a few places worth documenting.

### Memory constraint: can't run two TLS sessions at once

mbedtls statically allocates two 16 KB record buffers per session (`MBEDTLS_SSL_IN_CONTENT_LEN` / `OUT_CONTENT_LEN`). Two concurrent sessions (MQTT + OTA HTTPS) + WiFi's ~60 KB reservation + app state doesn't fit in the ESP32-C3's 400 KB SRAM. Handshake fails with `MBEDTLS_ERR_SSL_ALLOC_FAILED` (-32512).

**Solution: optimistic ACK + drop MQTT before OTA.**

1. OTA trigger arrives on MQTT
2. Copy payload to a static buffer, send `{"response":"success"}` ACK immediately
3. Return from the MQTT handler with a sentinel error → `client` drops → TLS heap freed
4. Outer loop sees the signal, runs the OTA download in a clean context
5. Reboot on success (new firmware) or failure (old firmware)

Server infers final success from the device reconnecting with the new version — standard pattern in production OTA systems (Mender, SWUpdate, AWS Jobs).

### Partition alignment gotcha

`otadata` must be aligned to its own size (8 KB). Putting it at `0xF000` (4KB-aligned but not 8KB-aligned) makes espflash reject the table with "invalid". Fix: shrink `nvs` from `0x6000` to `0x5000` so `otadata` lands at `0xE000`.

### StackResources bump

Raised from `StackResources<3>` to `StackResources<5>` — DHCP + DNS + MQTT + OTA HTTPS needs at least 4 TCP sockets. Costs ~2 KB RAM.

### Separate CA cert for firmware hosting

MQTT uses AWS IoT's CA. Firmware is hosted on GitHub Pages which uses Let's Encrypt — different chain. Added `OTA_CA_CERT_PATH` in `.env` pointing to ISRG Root X1 (downloaded directly from letsencrypt.org since GitHub Pages doesn't send the root in its chain).

### `esp-hal-ota` vendored locally

Upstream v0.4.6 depends on `esp32c3` PAC v0.31.0, but our forked `esp-hal` uses PAC v0.27.0 → duplicate `DEVICE_PERIPHERALS` linker symbol. Vendored copy at `../esp-hal-ota/` changes one line (`esp32c3 = []` instead of `esp32c3 = ["dep:esp32c3"]`) since the crate doesn't actually use any PAC types.

### Hardware-agnostic `ota` crate

Protocol logic (trigger parsing, CRC verify, progress tracking, URL parsing, flash-write orchestration) lives in `../ota/` with mock traits for `FlashWriter` + `HttpClient`. 37 unit tests run on the host without any ESP32 hardware. ESP32-specific implementations live in `main/src/ota_flash.rs` and `main/src/ota_http.rs`.

### Firmware publishing script

`./main/publish-firmware.sh <version> [--secure]` automates the build → save-image → (optional sign) → CRC32 → copy-to-website pipeline and prints the MQTT trigger JSON ready to publish. Paths configured via `FIRMWARE_HOST_DIR` and `FIRMWARE_BASE_URL` in `.env`.

### Runtime-derived MQTT client ID (one binary, whole fleet)

OTA only works if every device identifies itself uniquely to the broker. The original build baked `MQTT_CLIENT_ID` in at compile time via `.env`, which meant one binary-per-device — incompatible with OTA.

The client ID is now derived at boot from the chip's eFuse MAC address, formatted as a 12-character uppercase hex string (e.g. `80F1B2ECB820`). See `device_client_id()` in `src/lib.rs`. It's computed once in `main()` and passed into both `task_nfc` and `task_wifi_runner`.

**What this buys us:**

- One binary serves every device in the fleet — OTA can now legitimately target `public/ota`
- Zero per-device provisioning — the MAC is unique, burned in by Espressif at manufacturing
- The NFC registration response (`REG:C:<id>;;`) surfaces the MAC to the app so it can register the device without the user typing anything

**AWS IoT implications:**

The existing policy uses wildcards (`topic/*/root/*`, `client/*`), so it accepts any MAC-based client ID without modification. Only caveat: if you had an existing Thing named `000001`, it becomes orphaned after the device reboots under its MAC. Either re-register via NFC or update the DB row manually.

---

## Testing (dev mode)

### One-time setup

Ensure `.env` has these keys (see `.env.example`):

```bash
OTA_CA_CERT_PATH=src/certificates/ota_ca.pem
FIRMWARE_HOST_DIR=/path/to/portrait_v2/web/public/firmware
FIRMWARE_BASE_URL=https://www.paperportraitdisplay.com/firmware
```

Add the OTA permissions to your AWS IoT policy (Subscribe + Receive on `*/root/ota`). After updating the policy, power-cycle the device so it reconnects with the new permissions.

### Initial flash (once, via USB)

```bash
cd main
cargo r   # builds and flashes with partitions.csv, opens monitor
```

Wait for the device to boot, connect to WiFi, and subscribe to MQTT topics.

### Publish a new firmware version

```bash
cd main

# Make a visible change first (e.g., bump ESP_APP_DESC version or add a log line)

# Build, package, and copy to website:
./publish-firmware.sh 1.0.1            # dev mode (default)
# or
./publish-firmware.sh 1.0.1 --secure   # secure boot signed

# Commit + push the binary (the script prints these commands):
cd "$FIRMWARE_HOST_DIR/../../.."
git add web/public/firmware/1.0.1/
git commit -m "Publish firmware v1.0.1"
git push

# Wait ~1 min for AWS Amplify Hosting to deploy, verify:
curl -I https://www.paperportraitdisplay.com/firmware/1.0.1/firmware.bin
# → HTTP/2 200
```

### Trigger the OTA

Copy the JSON printed by `publish-firmware.sh` and publish it via AWS IoT MQTT test client:

- **Topic:** `{client_id}/root/ota` — **no leading slash**
- **Payload:** `{"url":"...","version":"1.0.1","size":...,"crc32":...}`

### What you should see on serial monitor

```
Received MQTT message on topic: {client_id}/root/ota
OTA update triggered via MQTT
OTA requested, running download after MQTT teardown
OTA: downloading version=1.0.1 size=... from https://...
OTA: resolved <host> to <ip>
OTA: TCP connected to <host>:443
OTA: TLS connected
OTA: GET request sent
OTA: HTTP 200 OK, starting download
OTA: 25% ...
OTA: 50% ...
OTA: 100% ...
OTA: firmware verified and boot target set
OTA success, rebooting into new firmware
<reboot>
<new version boots, reconnects to MQTT>
```

### Rollback test (optional)

Publish a v1.0.2 that panics at boot. Trigger OTA. After reboot, the new firmware will fail to `mark_valid()`. On next boot the bootloader rolls back to v1.0.1 automatically.

---

## Gotcha: USB reflash doesn't reset which partition boots

`cargo r` (espflash) writes firmware to `ota_0`, but **does not touch `otadata`**. If a previous OTA set `otadata` to point at `ota_1`, the device will keep booting from the stale `ota_1` binary even after you USB-flash a fresh one to `ota_0`.

Symptoms: you flash what should be "new" code, but the device behaves like it's running an old version (missing log lines you just added, old bugs coming back, etc.).

**Fix — erase the OTA selector before reflashing:**

```bash
cd main

# Nuke otadata only — blank otadata = boot from ota_0
espflash erase-parts --partition-table partitions.csv otadata

# Or for a fully clean slate:
espflash erase-flash

# Then reflash normally
cargo r
```

**When you'll hit this:** most often during OTA development when you've published a buggy firmware, it OTA'd successfully, and now the device is stuck running the buggy version. Since the buggy version may not be able to OTA itself to a fix (chicken-and-egg), USB recovery is the escape hatch — but you must erase `otadata` first.

**Also remember:** after recovering via USB, re-publish the fixed firmware to the website so future OTAs install the fixed code, not the buggy one that's still sitting at the hosted URL.

---

## References

- [ESP-IDF OTA Documentation](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-reference/system/ota.html)
- [ESP32-C3 Secure Boot V2 + OTA](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/security/secure-boot-v2.html#ota-updates)
- [Anti-rollback protection](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-reference/system/ota.html#anti-rollback)
