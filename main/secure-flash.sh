#!/bin/bash
# Secure Boot + Flash Encryption Flash Script
# Builds, signs, encrypts, and flashes firmware for secure boot enabled devices
#
# Prerequisites:
#   - Build bootloader with Secure Boot V2 AND Flash Encryption in menuconfig
#   - Generate signing key: espsecure.py generate_signing_key --version 2 secure_boot_signing_key.pem
#   - Generate encryption key: espsecure.py generate_flash_encryption_key flash_encryption_key.bin
#
# Usage:
#   ./secure-flash.sh
#
# Auto-detects chip state via SECURE_BOOT_EN eFuse:
#   - Fresh chip:   prompts for confirmation, burns keys, flashes bootloader + partition table + app
#   - Initialized:  flashes partition table + app (bootloader is signed/locked, can't be re-written)

set -e  # Exit on error

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARENT_DIR="$(dirname "$SCRIPT_DIR")"
VENV_DIR="${PARENT_DIR}/.venv"
SIGNING_KEY="${PARENT_DIR}/secure_boot_signing_key.pem"
ENCRYPTION_KEY="${PARENT_DIR}/flash_encryption_key.bin"
TARGET_DIR="${SCRIPT_DIR}/target/riscv32imc-unknown-none-elf/release"
BINARY_NAME="main"
APP_BIN="${SCRIPT_DIR}/app.bin"
APP_SIGNED="${SCRIPT_DIR}/app-signed.bin"
APP_ENCRYPTED="${SCRIPT_DIR}/app-encrypted.bin"

# Flash addresses (must match partition table in menuconfig)
PARTITION_TABLE_OFFSET="0x10000"
APP_OFFSET="0x20000"

# Bootloader config path
BOOTLOADER_DIR="${PARENT_DIR}/esp-idf/secure_bootloader"
SDKCONFIG="${BOOTLOADER_DIR}/sdkconfig"

# Serial port detection - user can override with PORT env var
detect_ports() {
    local ports=()
    # macOS ports
    for port in /dev/cu.usbserial-* /dev/cu.usbmodem* /dev/cu.wchusbserial*; do
        [ -e "$port" ] && ports+=("$port")
    done
    # Linux ports
    for port in /dev/ttyUSB* /dev/ttyACM*; do
        [ -e "$port" ] && ports+=("$port")
    done
    echo "${ports[@]}"
}

if [ -z "$PORT" ]; then
    AVAILABLE_PORTS=($(detect_ports))
    if [ ${#AVAILABLE_PORTS[@]} -eq 0 ]; then
        echo -e "${RED}Error: No ESP32 serial port detected${NC}"
        echo "Make sure the device is connected and try again."
        echo "You can also specify the port manually: PORT=/dev/cu.usbserial-XXX ./secure-flash.sh"
        exit 1
    elif [ ${#AVAILABLE_PORTS[@]} -eq 1 ]; then
        PORT="${AVAILABLE_PORTS[0]}"
        echo -e "${CYAN}Using serial port: ${PORT}${NC}"
    else
        echo -e "${CYAN}Multiple serial ports detected:${NC}"
        for i in "${!AVAILABLE_PORTS[@]}"; do
            echo "  $((i+1)). ${AVAILABLE_PORTS[$i]}"
        done
        read -p "Select port number [1-${#AVAILABLE_PORTS[@]}]: " port_choice
        if [[ "$port_choice" =~ ^[0-9]+$ ]] && [ "$port_choice" -ge 1 ] && [ "$port_choice" -le ${#AVAILABLE_PORTS[@]} ]; then
            PORT="${AVAILABLE_PORTS[$((port_choice-1))]}"
        else
            echo -e "${RED}Invalid selection${NC}"
            exit 1
        fi
        echo -e "${CYAN}Using serial port: ${PORT}${NC}"
    fi
else
    echo -e "${CYAN}Using serial port: ${PORT}${NC}"
fi

# Bootloader paths (from esp-idf secure_bootloader project)
BOOTLOADER_BIN="${BOOTLOADER_DIR}/build/bootloader/bootloader.bin"
PARTITION_TABLE="${BOOTLOADER_DIR}/build/partition_table/partition-table.bin"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Detect bootloader flash encryption mode from sdkconfig
detect_flash_encryption_mode() {
    if [ ! -f "$SDKCONFIG" ]; then
        echo "unknown"
        return
    fi

    if grep -q "CONFIG_SECURE_FLASH_ENCRYPTION_MODE_DEVELOPMENT=y" "$SDKCONFIG" 2>/dev/null; then
        echo "development"
    elif grep -q "CONFIG_SECURE_FLASH_ENCRYPTION_MODE_RELEASE=y" "$SDKCONFIG" 2>/dev/null; then
        echo "release"
    else
        echo "unknown"
    fi
}

# Display mode information and warnings
FLASH_MODE=$(detect_flash_encryption_mode)
echo ""
if [ "$FLASH_MODE" == "development" ]; then
    echo -e "${GREEN}Bootloader Mode: DEVELOPMENT${NC}"
    echo -e "${CYAN}  - UART flashing will remain enabled after first boot${NC}"
    echo -e "${CYAN}  - You can update firmware via ./secure-flash.sh${NC}"
elif [ "$FLASH_MODE" == "release" ]; then
    echo -e "${RED}Bootloader Mode: RELEASE${NC}"
    echo -e "${RED}  WARNING: UART flashing will be PERMANENTLY DISABLED after first boot!${NC}"
    echo -e "${RED}  After first boot, updates are only possible via OTA.${NC}"
    echo -e "${YELLOW}  If you need UART flashing, rebuild bootloader in Development mode:${NC}"
    echo -e "${YELLOW}    cd ../esp-idf/secure_bootloader${NC}"
    echo -e "${YELLOW}    idf.py menuconfig  # Change to Development mode${NC}"
    echo -e "${YELLOW}    idf.py fullclean && idf.py bootloader partition-table${NC}"
else
    echo -e "${YELLOW}Bootloader Mode: UNKNOWN${NC}"
    echo -e "${YELLOW}  Could not detect mode from sdkconfig. Verify your bootloader configuration.${NC}"
fi
echo ""

# Activate virtual environment
if [ -d "$VENV_DIR" ]; then
    echo -e "${CYAN}Activating virtual environment...${NC}"
    source "${VENV_DIR}/bin/activate"
else
    echo -e "${RED}Error: Virtual environment not found at ${VENV_DIR}${NC}"
    echo "Create one with: python3 -m venv .venv && source .venv/bin/activate && pip install esptool"
    exit 1
fi

# Check if signing key exists
if [ ! -f "$SIGNING_KEY" ]; then
    echo -e "${RED}Error: Signing key not found at ${SIGNING_KEY}${NC}"
    echo "Generate one with: espsecure.py generate_signing_key --version 2 secure_boot_signing_key.pem"
    exit 1
fi

# Check if encryption key exists
if [ ! -f "$ENCRYPTION_KEY" ]; then
    echo -e "${RED}Error: Encryption key not found at ${ENCRYPTION_KEY}${NC}"
    echo "Generate one with: espsecure.py generate_flash_encryption_key flash_encryption_key.bin"
    exit 1
fi

# Detect chip state by reading SECURE_BOOT_EN eFuse
echo -e "${CYAN}Detecting chip state...${NC}"
EFUSE_SUMMARY=$(espefuse.py --chip esp32c3 --port "$PORT" summary 2>&1)
if echo "$EFUSE_SUMMARY" | grep -qE "^SECURE_BOOT_EN.*= True"; then
    CHIP_INITIALIZED=true
    echo -e "${GREEN}Chip is already initialized — running update flow.${NC}"
else
    CHIP_INITIALIZED=false
    echo -e "${YELLOW}Fresh chip detected — running initial setup.${NC}"
fi
echo ""

# Fresh chip: warn user and require confirmation
if [ "$CHIP_INITIALIZED" = false ]; then
    if [ "$FLASH_MODE" == "development" ]; then
        echo -e "${GREEN}=== Initial Setup (Development Mode) ===${NC}"
    else
        echo -e "${GREEN}=== Initial Setup (Release Mode) ===${NC}"
    fi
    echo -e "${RED}WARNING: This will enable Secure Boot AND Flash Encryption!${NC}"
    echo -e "${RED}This is IRREVERSIBLE:${NC}"
    echo -e "${RED}  - Encryption key will be burned to eFuse${NC}"
    echo -e "${RED}  - Only signed firmware will run${NC}"
    echo -e "${RED}  - Flash contents will be encrypted${NC}"
    if [ "$FLASH_MODE" == "release" ]; then
        echo -e "${RED}  - UART flashing will be PERMANENTLY DISABLED (Release mode)${NC}"
    else
        echo -e "${CYAN}  - UART flashing will remain enabled (Development mode)${NC}"
    fi
    echo -e "${RED}  - JTAG will be permanently disabled${NC}"
    echo -e "${RED}Make sure you have backed up your signing key AND encryption key!${NC}"
    echo ""
    read -p "Are you sure you want to continue? (yes/no): " confirm
    if [ "$confirm" != "yes" ]; then
        echo "Aborted."
        exit 0
    fi

    if [ ! -f "$BOOTLOADER_BIN" ]; then
        echo -e "${RED}Error: Bootloader not found at ${BOOTLOADER_BIN}${NC}"
        echo "Build it first: ./main/build-bootloader.sh"
        exit 1
    fi
    if [ ! -f "$PARTITION_TABLE" ]; then
        echo -e "${RED}Error: Partition table not found at ${PARTITION_TABLE}${NC}"
        exit 1
    fi
fi

# Build app
echo -e "${YELLOW}Building release binary...${NC}"
(cd "$SCRIPT_DIR" && cargo build --release --features secure-boot)

echo -e "${YELLOW}Converting to flashable binary...${NC}"
espflash save-image --chip esp32c3 "${TARGET_DIR}/${BINARY_NAME}" "$APP_BIN"

echo -e "${YELLOW}Signing binary with secure boot key...${NC}"
espsecure.py sign_data --version 2 --keyfile "$SIGNING_KEY" --output "$APP_SIGNED" "$APP_BIN"

echo -e "${YELLOW}Encrypting app with flash encryption key...${NC}"
espsecure.py encrypt_flash_data --aes_xts --keyfile "$ENCRYPTION_KEY" \
    --address "$APP_OFFSET" --output "$APP_ENCRYPTED" "$APP_SIGNED"

# Always re-flash the partition table so partitions_secure.csv changes take effect
PARTITION_TABLE_ENCRYPTED="${PARENT_DIR}/partition-table-encrypted.bin"
if [ ! -f "$PARTITION_TABLE" ]; then
    echo -e "${RED}Error: Partition table binary not found at ${PARTITION_TABLE}${NC}"
    echo "Build it first: ./main/build-bootloader.sh"
    exit 1
fi
echo -e "${YELLOW}Encrypting partition table...${NC}"
espsecure.py encrypt_flash_data --aes_xts --keyfile "$ENCRYPTION_KEY" \
    --address "$PARTITION_TABLE_OFFSET" --output "$PARTITION_TABLE_ENCRYPTED" "$PARTITION_TABLE"

if [ "$CHIP_INITIALIZED" = false ]; then
    echo -e "${YELLOW}Burning encryption key to eFuse...${NC}"
    espefuse.py --chip esp32c3 --port "$PORT" burn_key BLOCK_KEY0 "$ENCRYPTION_KEY" XTS_AES_128_KEY --do-not-confirm || true

    echo -e "${YELLOW}Encrypting bootloader...${NC}"
    BOOTLOADER_ENCRYPTED="${PARENT_DIR}/bootloader-encrypted.bin"
    espsecure.py encrypt_flash_data --aes_xts --keyfile "$ENCRYPTION_KEY" \
        --address 0x0 --output "$BOOTLOADER_ENCRYPTED" "$BOOTLOADER_BIN"

    echo -e "${YELLOW}Enabling flash encryption (burning SPI_BOOT_CRYPT_CNT)...${NC}"
    espefuse.py --chip esp32c3 --port "$PORT" burn_efuse SPI_BOOT_CRYPT_CNT 0x1 --do-not-confirm || true

    echo -e "${YELLOW}Flashing bootloader, partition table, and app...${NC}"
    esptool.py --chip esp32c3 --port "$PORT" write_flash \
        0x0 "$BOOTLOADER_ENCRYPTED" \
        "$PARTITION_TABLE_OFFSET" "$PARTITION_TABLE_ENCRYPTED" \
        "$APP_OFFSET" "$APP_ENCRYPTED"

    echo -e "${YELLOW}Verifying eFuse status...${NC}"
    espefuse.py --chip esp32c3 --port "$PORT" summary | grep -E "SPI_BOOT_CRYPT_CNT|SECURE_BOOT_EN"

    echo -e "${GREEN}=== Initial setup complete! ===${NC}"
    echo -e "${YELLOW}On first boot, the device will burn the secure boot public key digest,${NC}"
    echo -e "${YELLOW}enable SECURE_BOOT_EN, and disable JTAG.${NC}"
    if [ "$FLASH_MODE" == "release" ]; then
        echo -e "${RED}UART flashing will be permanently disabled — future updates via OTA only.${NC}"
    else
        echo -e "${CYAN}Future updates: ./main/secure-flash.sh${NC}"
    fi
else
    echo -e "${YELLOW}Flashing partition table and app...${NC}"
    esptool.py --chip esp32c3 --port "$PORT" write_flash \
        "$PARTITION_TABLE_OFFSET" "$PARTITION_TABLE_ENCRYPTED" \
        "$APP_OFFSET" "$APP_ENCRYPTED"

    echo -e "${GREEN}=== Update flashed! ===${NC}"
fi
