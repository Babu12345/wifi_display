## WIFI / NFC Controlled e-ink display
WIFI and NFC controlled display that goes along with a companion application

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

## Authors
Babuabel Wanyeki

## License
Copyright (c) 2025 Wanyeki Technologies LLC. All rights reserved.
