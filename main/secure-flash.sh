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
#   ./secure-flash.sh --erase-otadata
#
# Auto-detects chip state via SECURE_BOOT_EN eFuse:
#   - Fresh chip:   prompts for confirmation, burns keys, flashes bootloader + partition table + app
#   - Initialized:  flashes partition table + app (bootloader is signed/locked, can't be re-written)
#
# --erase-otadata: also clear the otadata partition so the bootloader stops
#   trying to boot a stale ota_1 entry (e.g. after a failed OTA). Useful when
#   reverting to the freshly-flashed ota_0 image.

set -e  # Exit on error

ERASE_OTADATA=0
VERIFY_ONLY=0
SKIP_VERIFY=0
for arg in "$@"; do
    case "$arg" in
        --erase-otadata) ERASE_OTADATA=1 ;;
        --verify)        VERIFY_ONLY=1 ;;
        --no-verify)     SKIP_VERIFY=1 ;;
        *) echo "Unknown option: $arg" >&2; exit 1 ;;
    esac
done

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

# Read the chip's eFuse summary and verify it matches the expected end state
# for the given mode. Used as post-init validation after the bootloader's
# first boot has completed.
#
# Args:
#   $1 — expected mode: "release" or "development"
# Returns 0 if all checks pass, non-zero otherwise.
verify_chip_post_init() {
    local expected_mode="$1"
    echo -e "${CYAN}Reading eFuse state...${NC}"
    local efuse=$(espefuse.py --chip esp32c3 --port "$PORT" summary 2>&1)

    local secure_boot key_purpose_0 key_purpose_1 dis_pad_jtag dis_usb_jtag spi_crypt_cnt dis_manual_enc rd_dis
    secure_boot=$(echo "$efuse" | grep -E "^SECURE_BOOT_EN " | grep -oE "True|False" | head -1)
    key_purpose_0=$(echo "$efuse" | grep -E "^KEY_PURPOSE_0 " | grep -oE "XTS_AES_128_KEY|USER" | head -1)
    key_purpose_1=$(echo "$efuse" | grep -E "^KEY_PURPOSE_1 " | grep -oE "SECURE_BOOT_DIGEST0|USER" | head -1)
    dis_pad_jtag=$(echo "$efuse" | grep -E "^DIS_PAD_JTAG " | grep -oE "True|False" | head -1)
    dis_usb_jtag=$(echo "$efuse" | grep -E "^DIS_USB_JTAG " | grep -oE "True|False" | head -1)
    spi_crypt_cnt=$(echo "$efuse" | grep -E "^SPI_BOOT_CRYPT_CNT " | grep -oE "0b[01]+" | head -1)
    dis_manual_enc=$(echo "$efuse" | grep -E "^DIS_DOWNLOAD_MANUAL_ENCRYPT " | grep -oE "True|False" | head -1)
    rd_dis=$(echo "$efuse" | grep -E "^RD_DIS " | grep -oE "0b[01]+" | head -1)

    local pass=0
    check() {
        local name="$1" actual="$2" expected="$3"
        if [ "$actual" = "$expected" ]; then
            echo -e "  ${GREEN}✓${NC} $name = $actual"
        else
            echo -e "  ${RED}✗${NC} $name = $actual (expected $expected)"
            pass=1
        fi
    }

    echo "Common security checks:"
    check "SECURE_BOOT_EN"               "$secure_boot"    "True"
    check "KEY_PURPOSE_0"                "$key_purpose_0"  "XTS_AES_128_KEY"
    check "KEY_PURPOSE_1"                "$key_purpose_1"  "SECURE_BOOT_DIGEST0"
    check "DIS_PAD_JTAG"                 "$dis_pad_jtag"   "True"
    check "DIS_USB_JTAG"                 "$dis_usb_jtag"   "True"
    # RD_DIS bit 0 protects BLOCK_KEY0 (the encryption key) from being read.
    if [[ "$rd_dis" =~ ^0b.*1$ ]]; then
        echo -e "  ${GREEN}✓${NC} RD_DIS bit 0 set (encryption key read-protected)"
    else
        echo -e "  ${RED}✗${NC} RD_DIS = $rd_dis (encryption key NOT read-protected)"
        pass=1
    fi

    echo ""
    if [ "$expected_mode" = "release" ]; then
        echo "Release-mode lockdown checks:"
        check "DIS_DOWNLOAD_MANUAL_ENCRYPT" "$dis_manual_enc" "True"
        check "SPI_BOOT_CRYPT_CNT"          "$spi_crypt_cnt"  "0b111"
    else
        echo "Development-mode checks:"
        check "DIS_DOWNLOAD_MANUAL_ENCRYPT" "$dis_manual_enc" "False"
        check "SPI_BOOT_CRYPT_CNT"          "$spi_crypt_cnt"  "0b001"
    fi

    local mode_label="DEVELOPMENT"
    [ "$expected_mode" = "release" ] && mode_label="RELEASE"

    echo ""
    if [ $pass -eq 0 ]; then
        echo -e "${GREEN}=== Verification passed — chip is correctly locked into ${mode_label} mode ===${NC}"
        return 0
    else
        echo -e "${RED}=== Verification FAILED — chip state does not match expected ${mode_label} mode ===${NC}"
        echo -e "${YELLOW}If the chip just finished --init, the bootloader's first boot may not have completed.${NC}"
        echo -e "${YELLOW}Power-cycle the chip, wait 30+ seconds, then re-run with: ./main/secure-flash.sh --verify${NC}"
        return 1
    fi
}

# What the LOCAL bootloader source is built for. This only takes effect on a
# chip going through `--init`; for already-initialized chips the bootloader
# on flash is whatever was there at init time and can't be replaced.
FLASH_MODE=$(detect_flash_encryption_mode)
echo ""
if [ "$FLASH_MODE" == "development" ]; then
    echo -e "${CYAN}Local bootloader build: ${GREEN}DEVELOPMENT${NC}"
elif [ "$FLASH_MODE" == "release" ]; then
    echo -e "${CYAN}Local bootloader build: ${RED}RELEASE${NC}"
else
    echo -e "${CYAN}Local bootloader build: ${YELLOW}UNKNOWN${NC}"
fi

# Activate virtual environment (needed for espefuse before we can read the chip)
if [ -d "$VENV_DIR" ]; then
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

# Detect chip's ACTUAL state from its eFuses. For an initialized chip we also
# want to know whether it's locked at the dev-mode level or the release-mode
# level — those imply very different behavior even if the local bootloader
# source has been switched modes since init.
echo -e "${CYAN}Detecting chip state from eFuses...${NC}"
EFUSE_SUMMARY=$(espefuse.py --chip esp32c3 --port "$PORT" summary 2>&1)
SECURE_BOOT_EN=$(echo "$EFUSE_SUMMARY" | grep -E "^SECURE_BOOT_EN " | grep -oE "True|False" | head -1)
DIS_MANUAL_ENC=$(echo "$EFUSE_SUMMARY" | grep -E "^DIS_DOWNLOAD_MANUAL_ENCRYPT " | grep -oE "True|False" | head -1)
SPI_CRYPT_CNT=$(echo "$EFUSE_SUMMARY" | grep -E "^SPI_BOOT_CRYPT_CNT " | grep -oE "0b[01]+" | head -1)

if [ "$SECURE_BOOT_EN" == "False" ]; then
    CHIP_INITIALIZED=false
    CHIP_MODE="fresh"
elif [ "$DIS_MANUAL_ENC" == "True" ]; then
    CHIP_INITIALIZED=true
    CHIP_MODE="release"
else
    CHIP_INITIALIZED=true
    CHIP_MODE="development"
fi

# --verify: just check the chip's eFuse state matches the local bootloader's
# expected end state, then exit. Useful after a fresh-chip --init has finished
# its first boot (which is when the mode-specific eFuses get burned).
if [ "$VERIFY_ONLY" -eq 1 ]; then
    if [ "$CHIP_MODE" = "fresh" ]; then
        echo -e "${RED}Chip is FRESH — nothing to verify yet.${NC}"
        echo "Run './main/secure-flash.sh' to initialize it first."
        exit 1
    fi
    echo ""
    echo -e "${CYAN}=== Verifying chip against expected $FLASH_MODE state ===${NC}"
    verify_chip_post_init "$FLASH_MODE"
    exit $?
fi

case "$CHIP_MODE" in
    fresh)
        echo -e "${YELLOW}Chip's actual mode: ${YELLOW}FRESH${NC} (no eFuses burned yet)"
        if [ "$FLASH_MODE" = "release" ]; then
            echo -e "${YELLOW}  Will be initialized into RELEASE mode based on the bootloader build above.${NC}"
        elif [ "$FLASH_MODE" = "development" ]; then
            echo -e "${YELLOW}  Will be initialized into DEVELOPMENT mode based on the bootloader build above.${NC}"
        else
            echo -e "${YELLOW}  Will be initialized using whatever mode the bootloader was compiled for.${NC}"
        fi
        ;;
    development)
        echo -e "${CYAN}Chip's actual mode: ${GREEN}DEVELOPMENT${NC} (DIS_DOWNLOAD_MANUAL_ENCRYPT=False, SPI_BOOT_CRYPT_CNT=$SPI_CRYPT_CNT)"
        echo -e "${CYAN}  UART recovery still possible on this chip.${NC}"
        if [ "$FLASH_MODE" == "release" ]; then
            echo -e "${YELLOW}  Note: your local bootloader is RELEASE, but it can't replace this chip's${NC}"
            echo -e "${YELLOW}  signed dev-mode bootloader. The release config only takes effect on a fresh chip.${NC}"
        fi
        ;;
    release)
        echo -e "${RED}Chip's actual mode: RELEASE${NC} (UART flashing permanently disabled at hardware level)"
        echo -e "${RED}  This chip can ONLY accept updates via OTA. UART write attempts would fail.${NC}"
        echo ""
        echo -e "${CYAN}To update this chip:${NC}"
        echo -e "${CYAN}  1. ./main/publish-firmware.sh <version> --secure${NC}"
        echo -e "${CYAN}  2. Trigger OTA via MQTT (the script prints the trigger payload)${NC}"
        exit 1
        ;;
esac
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

# Build the app — needed for both the fresh-chip and update paths.
echo -e "${YELLOW}Building release binary...${NC}"
(cd "$SCRIPT_DIR" && cargo build --release --features secure-boot)

echo -e "${YELLOW}Converting to flashable binary...${NC}"
espflash save-image --chip esp32c3 "${TARGET_DIR}/${BINARY_NAME}" "$APP_BIN"

echo -e "${YELLOW}Signing binary with secure boot key...${NC}"
espsecure.py sign_data --version 2 --keyfile "$SIGNING_KEY" --output "$APP_SIGNED" "$APP_BIN"

if [ ! -f "$PARTITION_TABLE" ]; then
    echo -e "${RED}Error: Partition table binary not found at ${PARTITION_TABLE}${NC}"
    echo "Build it first: ./main/build-bootloader.sh"
    exit 1
fi

if [ "$CHIP_INITIALIZED" = false ]; then
    # === Fresh chip: --init flow ===
    #
    # We flash PLAINTEXT bootloader / partition table / app, then let the
    # bootloader's first boot encrypt them in place. This is the ESP-IDF
    # standard flow and — critically — it's the only way the bootloader's
    # release-mode eFuse burns happen (`DIS_DOWNLOAD_MANUAL_ENCRYPT`,
    # `DIS_DOWNLOAD_ICACHE`, `SPI_BOOT_CRYPT_CNT` max-out, etc.).
    #
    # Pre-burning `SPI_BOOT_CRYPT_CNT = 0b001` (the old shortcut) made
    # `esp_flash_encrypt_init()` early-return on first boot, skipping
    # `esp_flash_encryption_enable_secure_features()` — so the release-mode
    # lockdown never happened. Don't burn it here; the bootloader will set it
    # to the correct value for the mode it was compiled with.
    echo -e "${YELLOW}Burning encryption key to eFuse...${NC}"
    espefuse.py --chip esp32c3 --port "$PORT" burn_key BLOCK_KEY0 "$ENCRYPTION_KEY" XTS_AES_128_KEY --do-not-confirm || true

    echo -e "${YELLOW}Flashing plaintext bootloader, partition table, and signed app...${NC}"
    echo -e "${CYAN}  (the bootloader will encrypt these in place on first boot)${NC}"
    esptool.py --chip esp32c3 --port "$PORT" write_flash \
        0x0 "$BOOTLOADER_BIN" \
        "$PARTITION_TABLE_OFFSET" "$PARTITION_TABLE" \
        "$APP_OFFSET" "$APP_SIGNED"

    echo -e "${GREEN}=== Initial flash complete ===${NC}"
    echo ""
    echo -e "${YELLOW}On first boot the bootloader will:${NC}"
    echo "  1. Verify its signature, burn SECURE_BOOT_EN + signing key digest"
    echo "  2. Disable JTAG"
    echo "  3. Encrypt bootloader, partition table, and app in place"
    if [ "$FLASH_MODE" == "release" ]; then
        echo "  4. Burn DIS_DOWNLOAD_MANUAL_ENCRYPT and max out SPI_BOOT_CRYPT_CNT"
        echo -e "${RED}     UART flashing will be permanently disabled after this completes.${NC}"
    else
        echo "  4. Set SPI_BOOT_CRYPT_CNT=0b001 (dev mode — UART recovery still possible)"
    fi
    echo "  5. Reboot — the second boot runs the encrypted firmware normally"
    echo ""

    if [ "$SKIP_VERIFY" -eq 1 ]; then
        echo -e "${YELLOW}--no-verify set, skipping post-init verification.${NC}"
        echo -e "${YELLOW}Verify later with: ./main/secure-flash.sh --verify${NC}"
    else
        echo -e "${CYAN}=== Waiting for first-boot encryption to finish ===${NC}"
        echo "Tailing serial output without DTR/RTS toggling — the chip can't"
        echo "be interrupted by us reading. Looking for the bootloader's"
        echo "'Flash encryption completed' / app start markers."
        echo ""
        # The chip should already be booting (esptool just hard-reset it). Give
        # it a tiny head start so the first bytes don't get dropped while we
        # open the port.
        sleep 2
        if ! python3 "${SCRIPT_DIR}/wait-for-boot.py" "$PORT" --timeout 120; then
            echo ""
            echo -e "${RED}Did not see a successful-boot marker within the timeout.${NC}"
            echo -e "${YELLOW}This may mean the bootloader is still encrypting (very large${NC}"
            echo -e "${YELLOW}firmware?), or that something failed. Wait another minute and${NC}"
            echo -e "${YELLOW}try './main/secure-flash.sh --verify' to check the eFuse state.${NC}"
            exit 1
        fi
        echo ""
        echo -e "${CYAN}=== Post-init verification ===${NC}"
        if ! verify_chip_post_init "$FLASH_MODE"; then
            exit 1
        fi
    fi
else
    # === Already-initialized chip: encrypted update flow ===
    #
    # The chip already has encryption enabled. Plaintext writes wouldn't be
    # readable by the cache MMU. We pre-encrypt with our local copy of the
    # encryption key (which matches what's burned in eFuse BLOCK_KEY0) so
    # the bytes on flash are correct ciphertext.
    #
    # The bootloader is signed and write-protected from < 0x8000, so we only
    # touch the partition table and the app.
    PARTITION_TABLE_ENCRYPTED="${PARENT_DIR}/partition-table-encrypted.bin"
    echo -e "${YELLOW}Encrypting app with flash encryption key...${NC}"
    espsecure.py encrypt_flash_data --aes_xts --keyfile "$ENCRYPTION_KEY" \
        --address "$APP_OFFSET" --output "$APP_ENCRYPTED" "$APP_SIGNED"

    echo -e "${YELLOW}Encrypting partition table...${NC}"
    espsecure.py encrypt_flash_data --aes_xts --keyfile "$ENCRYPTION_KEY" \
        --address "$PARTITION_TABLE_OFFSET" --output "$PARTITION_TABLE_ENCRYPTED" "$PARTITION_TABLE"

    echo -e "${YELLOW}Flashing partition table and app...${NC}"
    esptool.py --chip esp32c3 --port "$PORT" write_flash \
        "$PARTITION_TABLE_OFFSET" "$PARTITION_TABLE_ENCRYPTED" \
        "$APP_OFFSET" "$APP_ENCRYPTED"

    if [ "$ERASE_OTADATA" -eq 1 ]; then
        # esptool's erase_region is blocked by the secure-boot safety check, so
        # overwrite the 8KB otadata partition with 0xFF instead. The cache MMU
        # decryption of all-ones ciphertext produces non-zero garbage, the
        # bootloader's CRC check on each slot fails, and it falls back to
        # scanning OTA partitions — picking the freshly-flashed ota_0.
        OTADATA_BLANK="${SCRIPT_DIR}/otadata-blank.bin"
        python3 -c "import sys; sys.stdout.buffer.write(b'\xff' * 8192)" > "$OTADATA_BLANK"
        echo -e "${YELLOW}Erasing otadata (forces bootloader to fall back to ota_0)...${NC}"
        esptool.py --chip esp32c3 --port "$PORT" write_flash 0x18000 "$OTADATA_BLANK"
        rm -f "$OTADATA_BLANK"
    fi

    echo -e "${GREEN}=== Update flashed! ===${NC}"

    # For an already-initialized chip, eFuses don't change during a normal
    # update — but a sanity check confirms nothing's gone sideways. The chip's
    # actual mode (already detected as `$CHIP_MODE`) is what we verify against.
    if [ "$SKIP_VERIFY" -eq 0 ]; then
        echo ""
        echo -e "${CYAN}=== Sanity-checking eFuse state ===${NC}"
        if ! verify_chip_post_init "$CHIP_MODE"; then
            echo -e "${YELLOW}(eFuses unchanged by an update — failure here means the chip's${NC}"
            echo -e "${YELLOW}lockdown state was already inconsistent with its detected mode.)${NC}"
            exit 1
        fi
    fi
fi
