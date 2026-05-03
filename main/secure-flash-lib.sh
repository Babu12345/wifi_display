#!/bin/bash
# Reusable functions for secure-flash.sh.
#
# Sourced by both the main script and `test-secure-flash.sh`. Defines
# functions only — no top-level side effects, no hardware access, no
# subprocess invocations at source time. All hardware I/O (espefuse.py
# calls) is the caller's responsibility; the functions take eFuse summary
# text as input so they're trivially testable with canned fixtures.

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# Detect bootloader flash-encryption mode by inspecting an sdkconfig file.
#
# Args:
#   $1 — path to the bootloader's sdkconfig
# Output: one of "development", "release", "unknown" on stdout
detect_flash_encryption_mode() {
    local sdkconfig="$1"
    if [ ! -f "$sdkconfig" ]; then
        echo "unknown"
        return
    fi
    if grep -q "CONFIG_SECURE_FLASH_ENCRYPTION_MODE_DEVELOPMENT=y" "$sdkconfig" 2>/dev/null; then
        echo "development"
    elif grep -q "CONFIG_SECURE_FLASH_ENCRYPTION_MODE_RELEASE=y" "$sdkconfig" 2>/dev/null; then
        echo "release"
    else
        echo "unknown"
    fi
}

# Classify the chip's secure-boot/encryption state from an eFuse summary.
#
# Args:
#   $1 — full text of `espefuse.py summary` output
# Output: one of "fresh", "development", "release", "stuck-release" on stdout
#
# State definitions:
#   fresh         — SECURE_BOOT_EN=False (no eFuses burned yet)
#   development   — SECURE_BOOT_EN=True, DIS_DOWNLOAD_MANUAL_ENCRYPT=False
#   release       — SECURE_BOOT_EN=True, DIS_DOWNLOAD_MANUAL_ENCRYPT=True,
#                   SPI_BOOT_CRYPT_CNT=0b111
#   stuck-release — SECURE_BOOT_EN=True, DIS_DOWNLOAD_MANUAL_ENCRYPT=True,
#                   SPI_BOOT_CRYPT_CNT≠0b111. The bootloader's first-boot
#                   encryption was interrupted between
#                   esp_flash_encryption_enable_secure_features() and
#                   esp_flash_encrypt_enable() — the chip can't decrypt its
#                   own flash and boot-loops on "invalid header".
classify_chip_state() {
    local efuse="$1"
    local secure_boot dis_manual_enc spi_crypt_cnt
    secure_boot=$(echo "$efuse" | grep -E "^SECURE_BOOT_EN " | grep -oE "True|False" | head -1)
    dis_manual_enc=$(echo "$efuse" | grep -E "^DIS_DOWNLOAD_MANUAL_ENCRYPT " | grep -oE "True|False" | head -1)
    spi_crypt_cnt=$(echo "$efuse" | grep -E "^SPI_BOOT_CRYPT_CNT " | grep -oE "0b[01]+" | head -1)

    if [ "$secure_boot" = "False" ]; then
        echo "fresh"
    elif [ "$dis_manual_enc" = "True" ] && [ "$spi_crypt_cnt" != "0b111" ]; then
        echo "stuck-release"
    elif [ "$dis_manual_enc" = "True" ]; then
        echo "release"
    else
        echo "development"
    fi
}

# Extract a named eFuse field from a summary blob.
#
# Args:
#   $1 — full text of `espefuse.py summary` output
#   $2 — eFuse field name (e.g. SPI_BOOT_CRYPT_CNT)
#   $3 — regex pattern of the value to extract (e.g. "True|False")
# Output: matched value, or empty string if the line / value isn't present
extract_efuse_field() {
    local efuse="$1" field="$2" pattern="$3"
    echo "$efuse" | grep -E "^${field} " | grep -oE "$pattern" | head -1
}

# Verify the chip's eFuses match the expected end state for a given mode.
#
# Args:
#   $1 — expected mode: "release" or "development"
#   $2 — full text of `espefuse.py summary` output
# Returns: 0 if all checks pass, 1 otherwise
# Side effects: prints colored pass/fail lines to stdout
verify_chip_post_init() {
    local expected_mode="$1"
    local efuse="$2"

    local secure_boot key_purpose_0 key_purpose_1 dis_pad_jtag dis_usb_jtag
    local spi_crypt_cnt dis_manual_enc rd_dis
    secure_boot=$(extract_efuse_field   "$efuse" "SECURE_BOOT_EN"              "True|False")
    key_purpose_0=$(extract_efuse_field "$efuse" "KEY_PURPOSE_0"               "XTS_AES_128_KEY|USER")
    key_purpose_1=$(extract_efuse_field "$efuse" "KEY_PURPOSE_1"               "SECURE_BOOT_DIGEST0|USER")
    dis_pad_jtag=$(extract_efuse_field  "$efuse" "DIS_PAD_JTAG"                "True|False")
    dis_usb_jtag=$(extract_efuse_field  "$efuse" "DIS_USB_JTAG"                "True|False")
    spi_crypt_cnt=$(extract_efuse_field "$efuse" "SPI_BOOT_CRYPT_CNT"          "0b[01]+")
    dis_manual_enc=$(extract_efuse_field "$efuse" "DIS_DOWNLOAD_MANUAL_ENCRYPT" "True|False")
    rd_dis=$(extract_efuse_field        "$efuse" "RD_DIS"                      "0b[01]+")

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
        return 1
    fi
}
