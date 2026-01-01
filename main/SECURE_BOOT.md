# ESP32-C3 Secure Boot V2 Setup

This guide covers enabling Secure Boot V2 to ensure only your signed firmware can run on the device.

## Critical Warnings

**This is IRREVERSIBLE once enabled:**
- eFuses are one-time programmable - once burned, the chip permanently requires signed firmware
- If you lose your signing key, the device becomes a brick
- JTAG debugging is permanently disabled
- Keep your private key secure and backed up in multiple locations

## Prerequisites

```bash
# Create a virtual environment for ESP tools (in project directory)
python3 -m venv .venv

# Activate the virtual environment
source .venv/bin/activate

# Install esptool/espsecure
pip install esptool
```

**Note**: Remember to activate the virtual environment (`source .venv/bin/activate`) before running any `espsecure.py` or `esptool.py` commands.

## Step 1: Generate Your Signing Key

```bash
# Generate RSA-3072 signing key (Secure Boot V2)
espsecure.py generate_signing_key --version 2 secure_boot_signing_key.pem
```

**Keep this key SAFE and BACKED UP** - losing it means losing access to your device forever.

## Step 2: Build a Secure Bootloader

You need a bootloader with Secure Boot enabled. This requires using esp-idf's build system for the bootloader:

```bash
# Clone esp-idf
git clone --recursive https://github.com/espressif/esp-idf.git
cd esp-idf
./install.sh esp32c3
source export.sh

# Create a minimal project just for bootloader
idf.py create-project secure_bootloader
cd secure_bootloader
idf.py set-target esp32c3
idf.py menuconfig
```

In menuconfig, configure:
- **Security features → Enable hardware Secure Boot in bootloader → Secure Boot V2**
- **Security features → Secure boot private signing key** → path to your `secure_boot_signing_key.pem`
- **Security features → UART ROM download mode** → `Permanently switch to Secure Download mode` or `Permanently disabled` (for production)

```bash
# Build the signed bootloader
idf.py bootloader
```

## Step 3: Sign Your Rust Application

After building your Rust binary:

```bash
# Build release
cargo build --release

# Convert to flashable binary
espflash save-image --chip esp32c3 target/riscv32imc-unknown-none-elf/release/main app.bin

# Sign the binary
espsecure.py sign_data --version 2 --keyfile secure_boot_signing_key.pem app.bin
```

## Step 4: Flash Everything (First Time - Enables Secure Boot)

```bash
# Flash bootloader (from esp-idf build)
esptool.py --chip esp32c3 write_flash 0x0 build/bootloader/bootloader.bin

# Flash partition table
esptool.py --chip esp32c3 write_flash 0x8000 build/partition_table/partition-table.bin

# Flash your signed Rust app
esptool.py --chip esp32c3 write_flash 0x10000 app.bin
```

## Step 5: First Boot Burns eFuses

On first boot, the bootloader will:
1. Burn the public key digest into eFuse
2. Enable `SECURE_BOOT_EN` eFuse
3. Disable JTAG
4. The device now ONLY accepts signed firmware

## Future Updates

Every time you update your firmware:

```bash
# Build
cargo build --release

# Convert
espflash save-image --chip esp32c3 target/riscv32imc-unknown-none-elf/release/main app.bin

# Sign
espsecure.py sign_data --version 2 --keyfile secure_boot_signing_key.pem app.bin

# Flash (still works over serial with your signed binary)
esptool.py --chip esp32c3 write_flash 0x10000 app.bin
```

---

# Flash Encryption

Flash Encryption prevents reading firmware from the chip. Combined with Secure Boot, this provides complete protection against both unauthorized firmware and firmware extraction.

## Critical Warnings

**Power interruption during first boot encryption will CORRUPT the flash** - do not interrupt power while encryption is running (can take up to 1 minute for large partitions).

**No key recovery** - the encryption key is stored in eFuse and cannot be read by software. If you use device-generated keys, there's no way to decrypt the flash contents externally.

## Development vs Release Mode

| Feature | Development Mode | Release Mode |
|---------|-----------------|--------------|
| Re-flash plaintext | Yes (bootloader encrypts) | No |
| UART download | Allowed | Disabled |
| Disable encryption | Once (burn eFuse) | Never |
| Use case | Testing | Production |

**Development Mode**: Allows repeated plaintext flashing - bootloader encrypts on-device. Can disable encryption once by burning `SPI_BOOT_CRYPT_CNT` eFuse.

**Release Mode**: Permanently disables plaintext UART flashing. Updates only via OTA. This is the secure production setting.

## Step 1: Generate Encryption Key (Optional)

You can let the device generate its own key (recommended for production), or generate one yourself:

```bash
# Generate your own key (allows external decryption if needed)
idf.py secure-generate-flash-encryption-key flash_encryption_key.bin

# Burn the key to eFuse BEFORE first encrypted boot
idf.py --port /dev/ttyUSB0 efuse-burn-key BLOCK_KEY0 flash_encryption_key.bin XTS_AES_128_KEY
```

**For production**: Use device-generated keys (more secure) or generate unique keys per device.

## Step 2: Configure in menuconfig

When building the bootloader (same project as Secure Boot):

```bash
idf.py menuconfig
```

Configure:
- **Security features → Enable flash encryption on boot** → Yes
- **Security features → Enable usage mode** → `Development` (for testing) or `Release` (for production)
- **Security features → UART ROM download mode** → Match your Secure Boot setting

**Note**: Enabling flash encryption increases bootloader size. You may need to adjust partition table offset.

## Step 3: Flash and First Boot

```bash
# Flash plaintext images (bootloader will encrypt on first boot)
idf.py flash

# Monitor the encryption process
idf.py monitor
```

On first boot:
1. Bootloader generates/uses encryption key
2. Encrypts all partitions marked for encryption
3. Burns `SPI_BOOT_CRYPT_CNT` eFuse to enable encryption
4. Burns protective eFuses (disables JTAG, direct boot, etc.)

**This can take up to 1 minute - DO NOT power off!**

## Future Updates

### Development Mode

```bash
# Build your Rust app
cargo build --release

# Convert to binary
espflash save-image --chip esp32c3 target/riscv32imc-unknown-none-elf/release/main app.bin

# Sign (if using Secure Boot)
espsecure.py sign_data --version 2 --keyfile secure_boot_signing_key.pem app.bin

# Flash - bootloader will encrypt automatically
esptool.py --chip esp32c3 write_flash 0x10000 app.bin
```

### Release Mode

In Release mode, you must use **OTA updates** or pre-encrypt the binary:

```bash
# Pre-encrypt the binary (requires your encryption key)
espsecure.py encrypt_flash_data --aes_xts --keyfile flash_encryption_key.bin \
    --address 0x10000 --output app-encrypted.bin app.bin

# Flash the pre-encrypted binary
esptool.py --chip esp32c3 write_flash 0x10000 app-encrypted.bin
```

## eFuses Burned by Flash Encryption

| eFuse | Effect |
|-------|--------|
| `BLOCK_KEYN` | Stores 256-bit AES encryption key |
| `SPI_BOOT_CRYPT_CNT` | Enables encryption (odd bit count = enabled) |
| `DIS_DOWNLOAD_ICACHE` | Disables instruction cache in download mode |
| `DIS_PAD_JTAG` | Disables JTAG via pads |
| `DIS_USB_JTAG` | Disables JTAG via USB |
| `DIS_DIRECT_BOOT` | Disables direct boot mode |

## Troubleshooting

**"flash read err, 1000" or continuous reboot**: You flashed plaintext to an encrypted device. In Development mode, the bootloader should re-encrypt. In Release mode, the device is soft-bricked.

**"invalid header"**: Encryption mismatch between bootloader expectation and flash contents.

## Combined Secure Boot + Flash Encryption

When using both together:

1. Configure both in menuconfig before first flash
2. Generate/specify keys for both features
3. Flash and let first boot enable both
4. For updates: sign first, then encrypt (or let bootloader encrypt in dev mode)

```bash
# Full secure build and flash workflow
cargo build --release
espflash save-image --chip esp32c3 target/riscv32imc-unknown-none-elf/release/main app.bin
espsecure.py sign_data --version 2 --keyfile secure_boot_signing_key.pem app.bin
# In dev mode, just flash - bootloader encrypts
esptool.py --chip esp32c3 write_flash 0x10000 app.bin
```

## References

- [ESP32-C3 Secure Boot V2 Documentation](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/security/secure-boot-v2.html)
- [ESP32-C3 Flash Encryption Documentation](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/security/flash-encryption.html)
- [ESP-IDF Security Features](https://docs.espressif.com/projects/esp-idf/en/stable/esp32/security/secure-boot-v2.html)
