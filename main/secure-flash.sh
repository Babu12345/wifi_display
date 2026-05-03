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
#   ./secure-flash.sh --verify
#   ./secure-flash.sh --recover
#   ./secure-flash.sh --no-verify
#
# Auto-detects chip state via eFuses:
#   - Fresh:        flashes plaintext bootloader/partition/app, lets the
#                   bootloader's first boot encrypt + burn mode-specific eFuses,
#                   then waits via serial polling and verifies eFuse state.
#   - Development:  pre-encrypts and flashes partition table + app.
#   - Release:      refuses (UART writes are blocked; use OTA).
#   - Stuck-release: detected when DIS_DOWNLOAD_MANUAL_ENCRYPT=True but
#                   SPI_BOOT_CRYPT_CNT≠0b111 — the bootloader's first-boot
#                   encryption was interrupted. Run with --recover to repair.
#
# Flags:
#   --erase-otadata  After flashing, write 8 KiB of 0xFF to the otadata
#                    partition (forces bootloader fallback to ota_0).
#   --verify         Skip flashing; just read eFuses and confirm they match
#                    the expected end state for the local bootloader's mode.
#   --recover        Auto-repair a stuck-release chip: burn SPI_BOOT_CRYPT_CNT
#                    to 0b111, re-flash partition + app pre-encrypted, verify.
#   --no-verify      Skip post-flash verification (for CI / scripted runs).

set -e  # Exit on error

ERASE_OTADATA=0
VERIFY_ONLY=0
SKIP_VERIFY=0
RECOVER=0
for arg in "$@"; do
    case "$arg" in
        --erase-otadata) ERASE_OTADATA=1 ;;
        --verify)        VERIFY_ONLY=1 ;;
        --no-verify)     SKIP_VERIFY=1 ;;
        --recover)       RECOVER=1 ;;
        *) echo "Unknown option: $arg" >&2; exit 1 ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARENT_DIR="$(dirname "$SCRIPT_DIR")"

# Pure-logic helpers (chip-state classification, eFuse parsing, post-init
# verification) live in the lib so `test-secure-flash.sh` can exercise them
# without touching hardware.
source "${SCRIPT_DIR}/secure-flash-lib.sh"
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

# Read the chip's eFuse summary over UART (wraps espefuse.py).
read_chip_efuses() {
    espefuse.py --chip esp32c3 --port "$PORT" summary 2>&1
}

# Read eFuses and verify them. Wraps the lib's verify_chip_post_init with the
# UART read step + the post-init failure hint that's only meaningful when the
# script is being run interactively against real hardware.
verify_chip_post_init_live() {
    local expected_mode="$1"
    echo -e "${CYAN}Reading eFuse state...${NC}"
    local efuse
    efuse=$(read_chip_efuses)
    if verify_chip_post_init "$expected_mode" "$efuse"; then
        return 0
    fi
    echo -e "${YELLOW}If the chip just finished --init, the bootloader's first boot may not have completed.${NC}"
    echo -e "${YELLOW}Power-cycle the chip, wait 30+ seconds, then re-run with: ./main/secure-flash.sh --verify${NC}"
    return 1
}

# What the LOCAL bootloader source is built for. This only takes effect on a
# chip going through `--init`; for already-initialized chips the bootloader
# on flash is whatever was there at init time and can't be replaced.
FLASH_MODE=$(detect_flash_encryption_mode "$SDKCONFIG")
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
EFUSE_SUMMARY=$(read_chip_efuses)
CHIP_MODE=$(classify_chip_state "$EFUSE_SUMMARY")
DIS_MANUAL_ENC=$(extract_efuse_field "$EFUSE_SUMMARY" "DIS_DOWNLOAD_MANUAL_ENCRYPT" "True|False")
SPI_CRYPT_CNT=$(extract_efuse_field "$EFUSE_SUMMARY" "SPI_BOOT_CRYPT_CNT" "0b[01]+")
if [ "$CHIP_MODE" = "fresh" ]; then
    CHIP_INITIALIZED=false
else
    CHIP_INITIALIZED=true
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
    verify_chip_post_init_live "$FLASH_MODE"
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
    stuck-release)
        echo -e "${RED}Chip's actual mode: STUCK RELEASE${NC} (DIS_DOWNLOAD_MANUAL_ENCRYPT=True, SPI_BOOT_CRYPT_CNT=$SPI_CRYPT_CNT)"
        echo -e "${YELLOW}  The release-mode lockdown was burned but SPI_BOOT_CRYPT_CNT never${NC}"
        echo -e "${YELLOW}  reached 0b111 — this happens when the bootloader's first-boot${NC}"
        echo -e "${YELLOW}  encryption was interrupted by a DTR reset before completion.${NC}"
        echo -e "${YELLOW}  The chip is currently boot-looping on 'invalid header'.${NC}"
        echo ""
        echo -e "${CYAN}Run with --recover to attempt automatic repair:${NC}"
        echo -e "${CYAN}    PORT=$PORT ./main/secure-flash.sh --recover${NC}"
        if [ "$RECOVER" -eq 0 ]; then
            exit 1
        fi
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

if [ "$CHIP_MODE" = "stuck-release" ] && [ "$RECOVER" -eq 1 ]; then
    # === Auto-recovery: stuck release-mode init ===
    #
    # The chip is in the post-encrypt, pre-CRYPT_CNT state. The flash already
    # holds correctly encrypted bootloader + (probably) partition table, and
    # possibly a partially-encrypted app. We:
    #
    #   1. Burn SPI_BOOT_CRYPT_CNT to 0b111 — turns on hardware decryption.
    #   2. Re-flash partition table + app with pre-encrypted bytes (using
    #      --force, since esptool can't tell our bytes are pre-encrypted and
    #      defaults to refusing on a release-mode chip). This guarantees the
    #      app is fully encrypted regardless of how far the bootloader's
    #      original encrypt_contents got.
    #   3. Wait for boot via the no-DTR serial reader.
    #   4. Verify all eFuses match the release-mode end state.
    echo -e "${CYAN}=== Recovery: completing stuck release-mode init ===${NC}"
    echo ""
    echo -e "${YELLOW}Step 1: Burn SPI_BOOT_CRYPT_CNT 0b$SPI_CRYPT_CNT → 0b111${NC}"
    espefuse.py --chip esp32c3 --port "$PORT" \
        burn_efuse SPI_BOOT_CRYPT_CNT 7 --do-not-confirm

    echo ""
    echo -e "${YELLOW}Step 2: Build, sign, encrypt, and re-flash app + partition table${NC}"
    (cd "$SCRIPT_DIR" && cargo build --release --features secure-boot)
    espflash save-image --chip esp32c3 "${TARGET_DIR}/${BINARY_NAME}" "$APP_BIN"
    espsecure.py sign_data --version 2 --keyfile "$SIGNING_KEY" --output "$APP_SIGNED" "$APP_BIN"
    espsecure.py encrypt_flash_data --aes_xts --keyfile "$ENCRYPTION_KEY" \
        --address "$APP_OFFSET" --output "$APP_ENCRYPTED" "$APP_SIGNED"

    PARTITION_TABLE_ENCRYPTED="${PARENT_DIR}/partition-table-encrypted.bin"
    espsecure.py encrypt_flash_data --aes_xts --keyfile "$ENCRYPTION_KEY" \
        --address "$PARTITION_TABLE_OFFSET" --output "$PARTITION_TABLE_ENCRYPTED" "$PARTITION_TABLE"

    # --force tells esptool to write the bytes even though encryption is on
    # at the chip level — safe because we're providing pre-encrypted bytes.
    esptool.py --chip esp32c3 --port "$PORT" --before default_reset write_flash --force \
        "$PARTITION_TABLE_OFFSET" "$PARTITION_TABLE_ENCRYPTED" \
        "$APP_OFFSET" "$APP_ENCRYPTED"

    echo ""
    echo -e "${YELLOW}Step 3: Wait for chip to boot${NC}"
    # SPI_BOOT_CRYPT_CNT is now 0b111 and the flash is fully encrypted, so the
    # bootloader skips the encryption pass entirely. A short settle delay is
    # enough — no need for the 45s wait the fresh-init path uses.
    sleep 5
    if ! python3 "${SCRIPT_DIR}/wait-for-boot.py" "$PORT" --timeout 60; then
        echo ""
        echo -e "${RED}Chip didn't reach a boot-success marker. The encryption hardware${NC}"
        echo -e "${RED}may be permanently broken on this chip — set it aside.${NC}"
        exit 1
    fi

    echo ""
    echo -e "${YELLOW}Step 4: Verify eFuse state${NC}"
    if ! verify_chip_post_init_live "release"; then
        exit 1
    fi
    echo ""
    echo -e "${GREEN}=== Recovery successful — chip is fully release-mode locked ===${NC}"
    exit 0
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
        echo "The bootloader's encryption pass takes 30–60s for a 1MB app and"
        echo "is the interruptible window: any external reset between"
        echo "esp_flash_encryption_enable_secure_features() and"
        echo "esp_flash_encrypt_enable() leaves the chip stuck."
        echo ""
        # Sleep silently — do NOT open the serial port — until we're confident
        # the encryption pass is past. Even with DTR/RTS held low, opening the
        # port through pyserial can briefly assert control lines on some
        # macOS/USB stacks, which is enough to reset the chip mid-encryption.
        # Empirically the dangerous window closes within ~45s.
        echo -e "${YELLOW}Sleeping 45s before touching the serial port...${NC}"
        sleep 45
        echo -e "${CYAN}Now tailing serial output for boot markers.${NC}"
        if ! python3 "${SCRIPT_DIR}/wait-for-boot.py" "$PORT" --timeout 90; then
            echo ""
            echo -e "${RED}Did not see a successful-boot marker within the timeout.${NC}"
            echo -e "${YELLOW}This may mean the bootloader is still encrypting (very large${NC}"
            echo -e "${YELLOW}firmware?), or that something failed. Wait another minute and${NC}"
            echo -e "${YELLOW}try './main/secure-flash.sh --verify' to check the eFuse state.${NC}"
            exit 1
        fi
        echo ""
        echo -e "${CYAN}=== Post-init verification ===${NC}"
        if ! verify_chip_post_init_live "$FLASH_MODE"; then
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
        if ! verify_chip_post_init_live "$CHIP_MODE"; then
            echo -e "${YELLOW}(eFuses unchanged by an update — failure here means the chip's${NC}"
            echo -e "${YELLOW}lockdown state was already inconsistent with its detected mode.)${NC}"
            exit 1
        fi
    fi
fi
