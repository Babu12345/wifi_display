## WIFI / NFC Controlled e-ink display

An ESP32-C3 based e-ink display that combines NFC tag functionality with secure MQTT connectivity for flexible content management. The device supports both local NFC-based configuration and remote real-time updates via an encrypted MQTT broker connection.

### Key Capabilities
- **NFC Configuration**: Wirelessly configure WiFi credentials, display text, and QR codes via smartphone
- **Live MQTT Updates**: Receive real-time display updates over TLS-encrypted MQTT connection
- **Multiple Display Modes**: Static text, QR codes, or live MQTT-driven content
- **Persistent Storage**: All settings and content survive power cycles
- **Power Efficient**: WiFi disabled during static display modes to conserve energy

## Requirements
- Rust 1.77 or later
- ESP32-C3 target support
- MQTT broker with TLS support (port 8883)

## Key Dependencies
- `esp-hal` - ESP32 hardware abstraction layer
- `esp-wifi` - WiFi connectivity for ESP32
- `esp-mbedtls` - TLS/SSL support with hardware acceleration
- `embassy-executor` - Async runtime for embedded systems
- `embassy-net` - Networking stack with TCP/UDP/DHCP
- `rust-mqtt` - MQTT client implementation
- Custom crates for NFC, e-ink display, and storage

## Certificate Setup
The project requires TLS certificates for secure MQTT communication. You need to provide three certificate files in the `main/src/certificates/` directory:

### Required Certificate Files
1. **ca1.pem** - Certificate Authority (CA) certificate
   - Used to verify the MQTT broker's certificate

2. **cert.pem.crt** - Client certificate
   - Your device's public certificate for client authentication

3. **private_key.pem.key** - Private key
   - The private key corresponding to your client certificate

### How to Add Certificates
1. Create the certificates directory (if it doesn't exist):
   ```bash
   mkdir -p main/src/certificates
   ```

2. Place your certificate files in `main/src/certificates/`:
   - `main/src/certificates/ca1.pem`
   - `main/src/certificates/cert.pem.crt`
   - `main/src/certificates/private_key.pem.key`

3. Ensure the files are in PEM format (text format starting with `-----BEGIN CERTIFICATE-----`)

**Note:** These certificate files are gitignored for security and should never be committed to the repository.

## Features

### NFC Tag Support
The device includes an STM25DV64KC NFC tag that enables wireless configuration and content updates:

#### Supported NFC Data Types
1. **WiFi Credentials**
   - Write WiFi SSID and password via NFC
   - Automatically connects to the configured network
   - Credentials are saved to persistent storage
   - Triggers notification: `WifiCredentials`

2. **Display Text**
   - Send custom text to display on the e-ink screen
   - Text is saved to persistent storage
   - Triggers notification: `DisplayText`
   - Exits live updates mode and displays the text
   - Special command: Send text `LIVE_UPDATES` to re-enter live updates mode

3. **Display URL (QR Code)**
   - Send a URL to generate and display a QR code
   - QR code is automatically scaled to fit the display
   - URL is saved to persistent storage
   - Triggers notification: `DisplayURL`
   - Exits live updates mode and displays the QR code

#### NFC Usage
- Bring your NFC-enabled phone close to the device
- Write NDEF-formatted data using an NFC app
- The device automatically detects and processes the data
- Changes persist across power cycles

### Display Modes
The e-ink display supports multiple operating modes:

1. **Custom Text Mode**
   - Displays text received via NFC
   - Text persists until updated
   - WiFi is disabled to save power

2. **QR Code Mode**
   - Generates and displays QR codes from URLs
   - Automatically scales QR code to fit display dimensions
   - Supports MIN_SCALE to MAX_SCALE sizing
   - WiFi is disabled to save power

3. **Live Secure Updates Mode** (Default)
   - The default mode when the device boots up
   - Connects to MQTT broker over TLS (port 8883)
   - Receives real-time updates for display content
   - Supports dynamic text and QR code rendering via MQTT messages
   - Maintains persistent connection with keep-alive pings
   - Automatically activated when neither text nor URL is stored
   - Can be re-entered by sending specific MQTT commands or power cycling

### Notification System
The device uses an internal notification system to coordinate between tasks:

- **WifiCredentials** - New WiFi credentials received via NFC; triggers reconnection
- **DisplayText** - New text content to display; exits MQTT mode, disables WiFi
- **DisplayURL** - New URL to convert to QR code; exits MQTT mode, disables WiFi
- **LiveSecureUpdates** - Switch to MQTT live update mode (default on boot)

Notifications are managed through Embassy's async `Watch` channel with support for 1 receiver.

#### Mode Switching
- Device boots in **Live Secure Updates** mode by default
- Sending **Text** or **URL** via NFC exits MQTT mode and switches to static display
- To return to live updates mode:
  - **Via NFC**: Send the text `LIVE_UPDATES` (switches to MQTT mode immediately)
  - **Via Power Cycle**: Restart the device (if no text/URL stored, defaults to live mode)
  - **Via Storage**: Manually clear stored text/URL data

### Persistent Storage
All configuration and content is stored in non-volatile flash storage:
- WiFi credentials
- Display text content
- Display URL content
- Data persists across reboots and power loss

## Usage Examples

### Configuring WiFi
1. Open an NFC app on your phone that supports writing WiFi credentials
2. Tap your phone to the NFC tag on the device
3. The device will connect to the network and save the credentials

### Displaying Custom Text
1. Use an NFC app to write plain text to the tag
2. The device will display the text on the e-ink screen
3. WiFi will be disabled to conserve power
4. Text persists until updated or cleared

### Displaying a QR Code
1. Use an NFC app to write a URL/URI to the tag
2. The device will generate and display a QR code for the URL
3. QR code is automatically scaled to fit the display
4. WiFi will be disabled to conserve power

### Switching to Live Updates
1. Use an NFC app to write the text `LIVE_UPDATES` to the tag
2. The device will immediately switch to live updates mode
3. WiFi will connect (if not already connected) and establish MQTT connection
4. Device will start receiving real-time updates from the MQTT broker

## OTA Firmware Updates

The device supports over-the-air firmware updates triggered via MQTT. See [main/OTA_PLAN.md](main/OTA_PLAN.md) for the full design.

### Publishing a new firmware version

```bash
cd main
./publish-firmware.sh 1.0.0            # dev mode (default)
./publish-firmware.sh 1.0.0 --secure   # sign with secure boot V2 key
```

The script builds the release image, packages it as a flashable binary, computes the CRC32 and size, copies it into the website firmware directory (configured in `.env`), and prints the MQTT trigger JSON to publish to `{client_id}/root/ota`.

Requires `FIRMWARE_HOST_DIR` and `FIRMWARE_BASE_URL` set in `main/.env` (see `main/.env.example`).

## Authors
Babuabel Wanyeki (babs@wanyekitech.com)

## License
Copyright (c) 2025 Wanyeki Technologies LLC. All rights reserved.
