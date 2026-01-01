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
# Install esptool/espsecure
pip install esptool
```

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

## Optional: Flash Encryption

For complete security, also enable **Flash Encryption** to prevent reading firmware from the chip. This is configured alongside Secure Boot in menuconfig under Security features.

## References

- [ESP32-C3 Secure Boot V2 Documentation](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/security/secure-boot-v2.html)
- [ESP-IDF Security Features](https://docs.espressif.com/projects/esp-idf/en/stable/esp32/security/secure-boot-v2.html)
