#!/bin/bash
# Secure Boot + Flash Encryption Flash Script (Release Mode)
# Builds, signs, encrypts, and flashes firmware for secure boot enabled devices
#
# Prerequisites:
#   - Build bootloader with Secure Boot V2 AND Flash Encryption (Release mode) in menuconfig
#   - Generate signing key: espsecure.py generate_signing_key --version 2 secure_boot_signing_key.pem
#   - Generate encryption key: espsecure.py generate_flash_encryption_key flash_encryption_key.bin
#
# Usage:
#   ./secure-flash.sh          - Normal update (build, sign, encrypt, flash app)
#   ./secure-flash.sh --init   - First time setup (burns keys, flash bootloader, partition table, and app)

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
BOOTLOADER_DIR="${PARENT_DIR}/esp-idf/secure_bootloader"
BOOTLOADER_BIN="${BOOTLOADER_DIR}/build/bootloader/bootloader.bin"
PARTITION_TABLE="${BOOTLOADER_DIR}/build/partition_table/partition-table.bin"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

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

# Check for --init flag (first time setup)
if [ "$1" == "--init" ]; then
    echo -e "${GREEN}=== Secure Boot + Flash Encryption Initial Setup (Release Mode) ===${NC}"
    echo -e "${RED}WARNING: This will enable Secure Boot AND Flash Encryption!${NC}"
    echo -e "${RED}This is IRREVERSIBLE:${NC}"
    echo -e "${RED}  - Encryption key will be burned to eFuse${NC}"
    echo -e "${RED}  - Only signed firmware will run${NC}"
    echo -e "${RED}  - Flash contents will be encrypted${NC}"
    echo -e "${RED}  - UART flashing of plaintext binaries will be PERMANENTLY DISABLED${NC}"
    echo -e "${RED}  - JTAG will be permanently disabled${NC}"
    echo -e "${RED}Make sure you have backed up your signing key AND encryption key!${NC}"
    echo ""
    read -p "Are you sure you want to continue? (yes/no): " confirm
    if [ "$confirm" != "yes" ]; then
        echo "Aborted."
        exit 0
    fi

    # Check bootloader exists
    if [ ! -f "$BOOTLOADER_BIN" ]; then
        echo -e "${RED}Error: Bootloader not found at ${BOOTLOADER_BIN}${NC}"
        echo "Build it first with esp-idf:"
        echo "  cd ../esp-idf/secure_bootloader"
        echo "  idf.py bootloader"
        exit 1
    fi

    if [ ! -f "$PARTITION_TABLE" ]; then
        echo -e "${RED}Error: Partition table not found at ${PARTITION_TABLE}${NC}"
        echo "Build it first with esp-idf:"
        echo "  cd ../esp-idf/secure_bootloader"
        echo "  idf.py partition-table"
        exit 1
    fi

    # Step 1: Burn encryption key to eFuse (skip if already burned)
    echo -e "${YELLOW}[1/10] Checking/burning encryption key to eFuse...${NC}"
    # Show full output, continue regardless of result (key may already be burned)
    espefuse.py --chip esp32c3 --port "$PORT" burn_key BLOCK_KEY0 "$ENCRYPTION_KEY" XTS_AES_128_KEY || true

    # Step 2: Build Rust app
    echo -e "${YELLOW}[2/10] Building release binary...${NC}"
    cargo build --release

    # Step 3: Convert to flashable binary
    echo -e "${YELLOW}[3/10] Converting to flashable binary...${NC}"
    espflash save-image --chip esp32c3 "${TARGET_DIR}/${BINARY_NAME}" "$APP_BIN"

    # Step 4: Sign the binary
    echo -e "${YELLOW}[4/10] Signing binary with secure boot key...${NC}"
    espsecure.py sign_data --version 2 --keyfile "$SIGNING_KEY" --output "$APP_SIGNED" "$APP_BIN"

    # Step 5: Encrypt app
    echo -e "${YELLOW}[5/10] Encrypting app with flash encryption key...${NC}"
    espsecure.py encrypt_flash_data --aes_xts --keyfile "$ENCRYPTION_KEY" \
        --address "$APP_OFFSET" --output "$APP_ENCRYPTED" "$APP_SIGNED"

    # Step 6: Encrypt bootloader
    echo -e "${YELLOW}[6/10] Encrypting bootloader...${NC}"
    BOOTLOADER_ENCRYPTED="${PARENT_DIR}/bootloader-encrypted.bin"
    espsecure.py encrypt_flash_data --aes_xts --keyfile "$ENCRYPTION_KEY" \
        --address 0x0 --output "$BOOTLOADER_ENCRYPTED" "$BOOTLOADER_BIN"

    # Step 7: Encrypt partition table
    echo -e "${YELLOW}[7/10] Encrypting partition table...${NC}"
    PARTITION_TABLE_ENCRYPTED="${PARENT_DIR}/partition-table-encrypted.bin"
    espsecure.py encrypt_flash_data --aes_xts --keyfile "$ENCRYPTION_KEY" \
        --address "$PARTITION_TABLE_OFFSET" --output "$PARTITION_TABLE_ENCRYPTED" "$PARTITION_TABLE"

    # Step 8: Burn SPI_BOOT_CRYPT_CNT to enable flash encryption
    echo -e "${YELLOW}[8/10] Enabling flash encryption (burning SPI_BOOT_CRYPT_CNT)...${NC}"
    espefuse.py --chip esp32c3 --port "$PORT" burn_efuse SPI_BOOT_CRYPT_CNT 0x1 --do-not-confirm || true

    # Step 9: Flash all encrypted binaries
    echo -e "${YELLOW}[9/10] Flashing all encrypted binaries...${NC}"
    esptool.py --chip esp32c3 --port "$PORT" write_flash \
        0x0 "$BOOTLOADER_ENCRYPTED" \
        "$PARTITION_TABLE_OFFSET" "$PARTITION_TABLE_ENCRYPTED" \
        "$APP_OFFSET" "$APP_ENCRYPTED"

    # Step 10: Verify eFuse status
    echo -e "${YELLOW}[10/10] Verifying eFuse status...${NC}"
    espefuse.py --chip esp32c3 --port "$PORT" summary | grep -E "SPI_BOOT_CRYPT_CNT|SECURE_BOOT_EN"

    echo -e "${GREEN}=== Initial setup complete! ===${NC}"
    echo -e "${YELLOW}On first boot, the device will:${NC}"
    echo "  1. Burn the secure boot public key digest into eFuse"
    echo "  2. Enable SECURE_BOOT_EN eFuse"
    echo "  3. Enable flash encryption using the burned key"
    echo "  4. Disable JTAG permanently"
    echo "  5. Disable plaintext UART flashing permanently"
    echo -e "${RED}After this:${NC}"
    echo -e "${RED}  - Only signed AND encrypted firmware can be flashed${NC}"
    echo -e "${RED}  - Flash contents cannot be read externally${NC}"
    echo -e "${RED}  - Future updates require pre-encryption with your key${NC}"

else
    echo -e "${GREEN}=== Secure Boot + Flash Encryption Update (Release Mode) ===${NC}"

    # Step 1: Build
    echo -e "${YELLOW}[1/5] Building release binary...${NC}"
    cargo build --release

    # Step 2: Convert to flashable binary
    echo -e "${YELLOW}[2/5] Converting to flashable binary...${NC}"
    espflash save-image --chip esp32c3 "${TARGET_DIR}/${BINARY_NAME}" "$APP_BIN"

    # Step 3: Sign the binary
    echo -e "${YELLOW}[3/5] Signing binary with secure boot key...${NC}"
    espsecure.py sign_data --version 2 --keyfile "$SIGNING_KEY" --output "$APP_SIGNED" "$APP_BIN"

    # Step 4: Encrypt the signed binary
    echo -e "${YELLOW}[4/5] Encrypting binary with flash encryption key...${NC}"
    espsecure.py encrypt_flash_data --aes_xts --keyfile "$ENCRYPTION_KEY" \
        --address "$APP_OFFSET" --output "$APP_ENCRYPTED" "$APP_SIGNED"

    # Step 5: Flash encrypted binary
    echo -e "${YELLOW}[5/5] Flashing encrypted binary...${NC}"
    echo -e "${CYAN}Enter download mode: Hold BOOT, press RESET, release both${NC}"
    read -p "Press Enter when device is in download mode..."
    esptool.py --chip esp32c3 --port "$PORT" write_flash "$APP_OFFSET" "$APP_ENCRYPTED"

    echo -e "${GREEN}=== Flash complete! ===${NC}"
fi
