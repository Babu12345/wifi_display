#!/bin/bash
# Bootloader Build Script for ESP32-C3 Secure Boot
# Simplifies building and configuring the secure bootloader
#
# Usage:
#   ./build-bootloader.sh              - Build bootloader and partition table
#   ./build-bootloader.sh --menuconfig - Open menuconfig to change settings
#   ./build-bootloader.sh --clean      - Clean build and rebuild
#   ./build-bootloader.sh --status     - Show current configuration

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARENT_DIR="$(dirname "$SCRIPT_DIR")"
BOOTLOADER_DIR="${PARENT_DIR}/esp-idf/secure_bootloader"
SDKCONFIG="${BOOTLOADER_DIR}/sdkconfig"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# Find ESP-IDF
find_esp_idf() {
    # Check if already sourced
    if command -v idf.py &> /dev/null; then
        return 0
    fi

    # Common ESP-IDF locations
    local idf_paths=(
        "$IDF_PATH/export.sh"
        "$HOME/esp/esp-idf/export.sh"
        "$HOME/esp/v5.3.3/esp-idf/export.sh"
        "$HOME/esp/v5.4/esp-idf/export.sh"
        "$HOME/esp/v5.4.1/esp-idf/export.sh"
        "$HOME/.espressif/esp-idf/v5.3/export.sh"
    )

    for path in "${idf_paths[@]}"; do
        if [ -f "$path" ]; then
            echo -e "${CYAN}Sourcing ESP-IDF from: $path${NC}"
            source "$path"
            return 0
        fi
    done

    echo -e "${RED}Error: ESP-IDF not found${NC}"
    echo "Please set IDF_PATH or source ESP-IDF export.sh manually:"
    echo "  source \$IDF_PATH/export.sh"
    exit 1
}

# Detect current mode from sdkconfig
detect_mode() {
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

# Show current status
show_status() {
    echo ""
    echo -e "${CYAN}=== Bootloader Configuration Status ===${NC}"
    echo ""

    local mode=$(detect_mode)
    if [ "$mode" == "development" ]; then
        echo -e "Flash Encryption Mode: ${GREEN}DEVELOPMENT${NC}"
        echo "  - UART flashing remains enabled after first boot"
        echo "  - Updates via ./secure-flash.sh"
    elif [ "$mode" == "release" ]; then
        echo -e "Flash Encryption Mode: ${RED}RELEASE${NC}"
        echo "  - UART flashing DISABLED after first boot"
        echo "  - Updates only via OTA"
    else
        echo -e "Flash Encryption Mode: ${YELLOW}UNKNOWN${NC}"
        echo "  - Run --menuconfig to configure"
    fi

    echo ""

    # Check if bootloader is built
    if [ -f "${BOOTLOADER_DIR}/build/bootloader/bootloader.bin" ]; then
        local boot_time=$(stat -f "%Sm" -t "%Y-%m-%d %H:%M:%S" "${BOOTLOADER_DIR}/build/bootloader/bootloader.bin" 2>/dev/null || stat -c "%y" "${BOOTLOADER_DIR}/build/bootloader/bootloader.bin" 2>/dev/null | cut -d'.' -f1)
        echo -e "Bootloader: ${GREEN}Built${NC} ($boot_time)"
    else
        echo -e "Bootloader: ${YELLOW}Not built${NC}"
    fi

    if [ -f "${BOOTLOADER_DIR}/build/partition_table/partition-table.bin" ]; then
        local part_time=$(stat -f "%Sm" -t "%Y-%m-%d %H:%M:%S" "${BOOTLOADER_DIR}/build/partition_table/partition-table.bin" 2>/dev/null || stat -c "%y" "${BOOTLOADER_DIR}/build/partition_table/partition-table.bin" 2>/dev/null | cut -d'.' -f1)
        echo -e "Partition Table: ${GREEN}Built${NC} ($part_time)"
    else
        echo -e "Partition Table: ${YELLOW}Not built${NC}"
    fi

    # Check sdkconfig modification time
    if [ -f "$SDKCONFIG" ]; then
        local config_time=$(stat -f "%Sm" -t "%Y-%m-%d %H:%M:%S" "$SDKCONFIG" 2>/dev/null || stat -c "%y" "$SDKCONFIG" 2>/dev/null | cut -d'.' -f1)
        echo -e "Config (sdkconfig): Last modified $config_time"

        # Warn if config is newer than bootloader
        if [ -f "${BOOTLOADER_DIR}/build/bootloader/bootloader.bin" ]; then
            if [ "$SDKCONFIG" -nt "${BOOTLOADER_DIR}/build/bootloader/bootloader.bin" ]; then
                echo ""
                echo -e "${YELLOW}WARNING: sdkconfig is newer than bootloader!${NC}"
                echo -e "${YELLOW}Run './build-bootloader.sh --clean' to rebuild with new settings.${NC}"
            fi
        fi
    fi

    echo ""
}

# Main script - change to bootloader directory
cd "$BOOTLOADER_DIR"

case "$1" in
    --menuconfig)
        find_esp_idf
        echo ""
        echo -e "${CYAN}Opening menuconfig...${NC}"
        echo ""
        echo "To change Flash Encryption mode:"
        echo "  1. Navigate to: Security features → Enable usage mode"
        echo "  2. Select Development or Release"
        echo "  3. Press S to save, Q to quit"
        echo ""
        echo -e "${YELLOW}After changing settings, run: ./build-bootloader.sh --clean${NC}"
        echo ""
        idf.py menuconfig
        show_status
        ;;

    --clean)
        find_esp_idf
        echo ""
        echo -e "${CYAN}Cleaning and rebuilding bootloader...${NC}"
        echo ""
        idf.py fullclean
        idf.py bootloader partition-table
        echo ""
        echo -e "${GREEN}=== Build complete ===${NC}"
        show_status
        ;;

    --status)
        show_status
        ;;

    --help|-h)
        echo "Bootloader Build Script for ESP32-C3 Secure Boot"
        echo ""
        echo "Usage:"
        echo "  ./build-bootloader.sh              Build bootloader and partition table"
        echo "  ./build-bootloader.sh --menuconfig Open menuconfig to change settings"
        echo "  ./build-bootloader.sh --clean      Clean build directory and rebuild"
        echo "  ./build-bootloader.sh --status     Show current configuration status"
        echo "  ./build-bootloader.sh --help       Show this help message"
        echo ""
        echo "After changing menuconfig settings, always use --clean to rebuild."
        ;;

    *)
        find_esp_idf
        echo ""
        echo -e "${CYAN}Building bootloader and partition table...${NC}"
        echo ""
        idf.py bootloader partition-table
        echo ""
        echo -e "${GREEN}=== Build complete ===${NC}"
        show_status
        ;;
esac
