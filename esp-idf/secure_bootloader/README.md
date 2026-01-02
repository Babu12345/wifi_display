# ESP-IDF Secure Bootloader for ESP32-C3

This project builds the ESP-IDF bootloader with Secure Boot V2 and Flash Encryption enabled for use with the Rust wifi_display firmware.

## Prerequisites

1. ESP-IDF v5.x installed and sourced (`source $IDF_PATH/export.sh`)
2. Signing and encryption keys generated in parent directory

## Configuration

Run `idf.py menuconfig` and configure:

### Security Features

```
Security features --->
    [*] Enable hardware Secure Boot in bootloader
    Secure Boot Version (Secure Boot V2)  --->
    [*] Enable flash encryption on boot
    Flash encryption mode --->
        ( ) Development (NOT SECURE)      # Allows UART flashing
        (X) Release                       # DISABLES UART flashing (OTA only)

    # UART Download Mode - ONLY WORKS IN DEVELOPMENT MODE!
    # In Release mode, this setting is IGNORED - UART is always disabled
    UART ROM download mode --->
        (X) Enabled (not recommended)     # Only works in Development mode
        ( ) Enabled with security download mode
        ( ) Disabled

    [*] Check Flash Encryption enabled on app startup (default value)
```

### CRITICAL: Release Mode Disables UART

**WARNING: In Release mode, the "UART ROM download mode" setting is IGNORED.**

Release mode automatically burns the `ENABLE_SECURITY_DOWNLOAD` eFuse on first boot, which disables normal UART flashing regardless of your menuconfig setting. This is hardcoded ESP-IDF behavior for security.

| Flash Encryption Mode | UART Flashing After First Boot |
|-----------------------|-------------------------------|
| **Development** | Yes (respects UART ROM download mode setting) |
| **Release** | **No** (always disabled, ignores UART setting) |

**If you need UART flashing for firmware updates, you MUST use Development mode.**

### Check Flash Encryption on Startup

This option validates flash encryption is properly enabled each time the app boots:

```
Security features --->
    [*] Check Flash Encryption enabled on app startup (default value)
```

When enabled, the bootloader verifies the `SPI_BOOT_CRYPT_CNT` eFuse is set before running the application. This prevents accidentally running unencrypted firmware on a device that should be encrypted.

### UART Download Mode Options

| Option | UART Flashing | Security | Use Case |
|--------|---------------|----------|----------|
| **Enabled** | Yes (encrypted binaries only) | Lower | Development, easier recovery |
| **Enabled with security download mode** | Authenticated only | Medium | Production with OTA fallback |
| **Disabled** | No | Highest | Maximum security, OTA only |

**Important:** Even with "Enabled (not recommended)", the device still requires:
- Signed firmware (Secure Boot V2)
- Pre-encrypted binaries (Flash Encryption)

The trade-off is that physical attackers can erase flash or attempt fault injection, but cannot execute unsigned code.

### Partition Table

```
Partition Table --->
    Partition Table (Custom partition table CSV)  --->
    (partitions_secure.csv) Custom partition CSV file
    (0x10000) Offset of partition table
```

### Serial Flasher Config

```
Serial flasher config --->
    Flash size (4 MB)  --->
```

## Quick Start with Build Script

A helper script is provided to simplify bootloader management:

```bash
# Check current configuration status
./build-bootloader.sh --status

# Open menuconfig to change settings
./build-bootloader.sh --menuconfig

# Clean and rebuild (required after config changes)
./build-bootloader.sh --clean

# Just build (if no config changes)
./build-bootloader.sh
```

The script automatically finds and sources ESP-IDF, detects the current mode, and warns if the config is newer than the built bootloader.

## Switching Between Development and Release Mode

**IMPORTANT:** You must rebuild the bootloader after changing modes. The mode is baked into the bootloader binary.

### Using the Build Script (Recommended)

```bash
# 1. Open menuconfig
./build-bootloader.sh --menuconfig

# 2. Navigate to: Security features → Enable usage mode
# 3. Select Development or Release
# 4. Press S to save, Q to quit

# 5. Clean and rebuild
./build-bootloader.sh --clean
```

### Manual Method

1. **Source ESP-IDF and open menuconfig:**
   ```bash
   source $IDF_PATH/export.sh
   idf.py menuconfig
   ```

2. **Navigate to:** `Security features → Enable usage mode`

3. **Select your mode:**
   - `Development (NOT SECURE)` - Allows UART flashing of pre-encrypted binaries
   - `Release` - Disables UART flashing permanently (OTA only)

4. **Save and exit** (Press `S` then `Q`)

5. **Clean and rebuild** (required after mode change):
   ```bash
   idf.py fullclean
   idf.py bootloader partition-table
   ```

### Verify Current Mode

```bash
# Using the build script
./build-bootloader.sh --status

# Or manually check sdkconfig
grep "FLASH_ENCRYPTION_MODE" sdkconfig
```

- `CONFIG_SECURE_FLASH_ENCRYPTION_MODE_DEVELOPMENT=y` → Development mode
- `CONFIG_SECURE_FLASH_ENCRYPTION_MODE_RELEASE=y` → Release mode

## Building

### Using the Build Script (Recommended)

```bash
# Build bootloader and partition table
./build-bootloader.sh

# Clean build (after config changes)
./build-bootloader.sh --clean
```

### Manual Method

```bash
# Source ESP-IDF (use your installed version path)
source $IDF_PATH/export.sh
# Or for example: source ~/esp/v5.3.3/esp-idf/export.sh

# Build bootloader and partition table
idf.py bootloader partition-table
```

### Clean Build (Required After Config Changes)

If you change any menuconfig settings (especially security settings), always do a clean build:

```bash
# Using build script
./build-bootloader.sh --clean

# Or manually
idf.py fullclean
idf.py bootloader partition-table
```

**Why clean build matters:** The bootloader binary embeds the security configuration. If you change from Release to Development mode but don't rebuild, the old Release mode bootloader will still be used and will burn the wrong eFuses.

## Output Files

After building, these files are used by `secure-flash.sh`:
- `build/bootloader/bootloader.bin` - Bootloader binary
- `build/partition_table/partition-table.bin` - Partition table binary

## Flashing

Use the `secure-flash.sh` script in the `main/` directory:

```bash
# Initial setup (burns eFuses - IRREVERSIBLE)
cd ../main
./secure-flash.sh --init

# Subsequent updates
./secure-flash.sh
```

## eFuses Burned on First Boot

After initial flash with `--init`, the first boot will permanently burn:
- `SECURE_BOOT_EN` - Enables secure boot verification
- Secure boot public key digest
- `SPI_BOOT_CRYPT_CNT` - Enables flash encryption (if not already burned)
- `DIS_DOWNLOAD_MANUAL_ENCRYPT` - Disables manual encryption in download mode
- `DIS_PAD_JTAG` / `DIS_USB_JTAG` - Disables JTAG (depending on UART mode setting)

## Key Files

Keys should be stored in the parent directory (`../`):
- `secure_boot_signing_key.pem` - RSA-3072 signing key for Secure Boot V2
- `flash_encryption_key.bin` - 256-bit AES-XTS key for Flash Encryption

**CRITICAL:** Back up these keys securely. Losing them means you cannot update the device.
