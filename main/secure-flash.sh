#!/bin/bash
# Secure Boot Flash Script
# Builds, signs, and flashes firmware for secure boot enabled devices
#
# Usage:
#   ./secure-flash.sh          - Normal update (build, sign, flash app only)
#   ./secure-flash.sh --init   - First time setup (flash bootloader, partition table, and app)

set -e  # Exit on error

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARENT_DIR="$(dirname "$SCRIPT_DIR")"
VENV_DIR="${PARENT_DIR}/.venv"
SIGNING_KEY="${SCRIPT_DIR}/secure_boot_signing_key.pem"
TARGET_DIR="${SCRIPT_DIR}/target/riscv32imc-unknown-none-elf/release"
BINARY_NAME="main"
APP_BIN="${SCRIPT_DIR}/app.bin"

# Bootloader paths (from esp-idf secure_bootloader project)
BOOTLOADER_DIR="${SCRIPT_DIR}/../esp-idf/secure_bootloader"
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

# Check for --init flag (first time setup)
if [ "$1" == "--init" ]; then
    echo -e "${GREEN}=== Secure Boot Initial Setup ===${NC}"
    echo -e "${RED}WARNING: This will enable Secure Boot on first device boot!${NC}"
    echo -e "${RED}This is IRREVERSIBLE. Make sure you have backed up your signing key.${NC}"
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

    # Step 1: Build Rust app
    echo -e "${YELLOW}[1/6] Building release binary...${NC}"
    cargo build --release

    # Step 2: Convert to flashable binary
    echo -e "${YELLOW}[2/6] Converting to flashable binary...${NC}"
    espflash save-image --chip esp32c3 "${TARGET_DIR}/${BINARY_NAME}" "$APP_BIN"

    # Step 3: Sign the binary
    echo -e "${YELLOW}[3/6] Signing binary with secure boot key...${NC}"
    espsecure.py sign_data --version 2 --keyfile "$SIGNING_KEY" "$APP_BIN"

    # Step 4: Flash bootloader
    echo -e "${YELLOW}[4/6] Flashing secure bootloader...${NC}"
    esptool.py --chip esp32c3 write_flash 0x0 "$BOOTLOADER_BIN"

    # Step 5: Flash partition table
    echo -e "${YELLOW}[5/6] Flashing partition table...${NC}"
    esptool.py --chip esp32c3 write_flash 0x8000 "$PARTITION_TABLE"

    # Step 6: Flash app
    echo -e "${YELLOW}[6/6] Flashing signed application...${NC}"
    esptool.py --chip esp32c3 write_flash 0x10000 "$APP_BIN"

    echo -e "${GREEN}=== Initial setup complete! ===${NC}"
    echo -e "${YELLOW}On first boot, the device will:${NC}"
    echo "  1. Burn the public key digest into eFuse"
    echo "  2. Enable SECURE_BOOT_EN eFuse"
    echo "  3. Disable JTAG"
    echo -e "${RED}After this, only signed firmware will run on this device.${NC}"

else
    echo -e "${GREEN}=== Secure Boot Flash Script ===${NC}"

    # Step 1: Build
    echo -e "${YELLOW}[1/4] Building release binary...${NC}"
    cargo build --release

    # Step 2: Convert to flashable binary
    echo -e "${YELLOW}[2/4] Converting to flashable binary...${NC}"
    espflash save-image --chip esp32c3 "${TARGET_DIR}/${BINARY_NAME}" "$APP_BIN"

    # Step 3: Sign the binary
    echo -e "${YELLOW}[3/4] Signing binary with secure boot key...${NC}"
    espsecure.py sign_data --version 2 --keyfile "$SIGNING_KEY" "$APP_BIN"

    # Step 4: Flash
    echo -e "${YELLOW}[4/4] Flashing signed binary...${NC}"
    esptool.py --chip esp32c3 write_flash 0x10000 "$APP_BIN"

    echo -e "${GREEN}=== Flash complete! ===${NC}"
fi
