#!/bin/bash
# Tests for secure-flash-lib.sh.
#
# Sources the lib (no hardware access required) and exercises each function
# against canned `espefuse.py summary` fixtures representing the four chip
# states the script must handle:
#
#   fresh          — no eFuses burned
#   development    — secure boot on, manual encrypt allowed (UART recovery ok)
#   release        — secure boot on, manual encrypt disabled, CRYPT_CNT=0b111
#   stuck-release  — DIS_DOWNLOAD_MANUAL_ENCRYPT burned but CRYPT_CNT≠0b111
#                    (bootloader's first-boot encryption was interrupted)
#
# Run from anywhere:
#     ./main/test-secure-flash.sh
#
# Exit code: 0 if all tests pass, 1 otherwise.

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=secure-flash-lib.sh
source "${SCRIPT_DIR}/secure-flash-lib.sh"

# ----------------------------------------------------------------------------
# Fixtures: snippets of `espefuse.py --chip esp32c3 summary` output.
#
# Only the lines the lib's grep patterns key on are included. Real espefuse
# output has many more fields; the parsers anchor on `^FIELD ` so unrelated
# lines don't interfere.
# ----------------------------------------------------------------------------

FRESH_EFUSE='Security fuses:
SECURE_BOOT_EN (BLOCK0)                Enable secure boot                                  = False R/W (0b0)
SPI_BOOT_CRYPT_CNT (BLOCK0)            Enables flash encryption                            = 0 R/W (0b000)
DIS_DOWNLOAD_MANUAL_ENCRYPT (BLOCK0)   Disable flash encryption in dl mode                 = False R/W (0b0)
DIS_PAD_JTAG (BLOCK0)                  Permanently disable JTAG access via pads            = False R/W (0b0)
DIS_USB_JTAG (BLOCK0)                  Set this bit to disable function of usb-jtag        = False R/W (0b0)
KEY_PURPOSE_0 (BLOCK0)                 KEY0 purpose                                        = USER R/W (0x0)
KEY_PURPOSE_1 (BLOCK0)                 KEY1 purpose                                        = USER R/W (0x0)
RD_DIS (BLOCK0)                        Disable reading from BLOCK4-10                      = 0 R/W (0b0000000)'

DEV_EFUSE='Security fuses:
SECURE_BOOT_EN (BLOCK0)                Enable secure boot                                  = True R/W (0b1)
SPI_BOOT_CRYPT_CNT (BLOCK0)            Enables flash encryption                            = 1 R/W (0b001)
DIS_DOWNLOAD_MANUAL_ENCRYPT (BLOCK0)   Disable flash encryption in dl mode                 = False R/W (0b0)
DIS_PAD_JTAG (BLOCK0)                  Permanently disable JTAG access via pads            = True R/W (0b1)
DIS_USB_JTAG (BLOCK0)                  Set this bit to disable function of usb-jtag        = True R/W (0b1)
KEY_PURPOSE_0 (BLOCK0)                 KEY0 purpose                                        = XTS_AES_128_KEY R/W (0x4)
KEY_PURPOSE_1 (BLOCK0)                 KEY1 purpose                                        = SECURE_BOOT_DIGEST0 R/W (0x9)
RD_DIS (BLOCK0)                        Disable reading from BLOCK4-10                      = 1 R/W (0b0000001)'

RELEASE_EFUSE='Security fuses:
SECURE_BOOT_EN (BLOCK0)                Enable secure boot                                  = True R/W (0b1)
SPI_BOOT_CRYPT_CNT (BLOCK0)            Enables flash encryption                            = 7 R/W (0b111)
DIS_DOWNLOAD_MANUAL_ENCRYPT (BLOCK0)   Disable flash encryption in dl mode                 = True R/W (0b1)
DIS_PAD_JTAG (BLOCK0)                  Permanently disable JTAG access via pads            = True R/W (0b1)
DIS_USB_JTAG (BLOCK0)                  Set this bit to disable function of usb-jtag        = True R/W (0b1)
KEY_PURPOSE_0 (BLOCK0)                 KEY0 purpose                                        = XTS_AES_128_KEY R/W (0x4)
KEY_PURPOSE_1 (BLOCK0)                 KEY1 purpose                                        = SECURE_BOOT_DIGEST0 R/W (0x9)
RD_DIS (BLOCK0)                        Disable reading from BLOCK4-10                      = 1 R/W (0b0000001)'

# Stuck-release: DIS_DOWNLOAD_MANUAL_ENCRYPT was burned by the bootloader's
# secure_features() pass, but the chip reset before encrypt_enable() got to
# burn SPI_BOOT_CRYPT_CNT. This is the bug the script's classifier was
# specifically built to detect.
STUCK_RELEASE_EFUSE='Security fuses:
SECURE_BOOT_EN (BLOCK0)                Enable secure boot                                  = True R/W (0b1)
SPI_BOOT_CRYPT_CNT (BLOCK0)            Enables flash encryption                            = 0 R/W (0b000)
DIS_DOWNLOAD_MANUAL_ENCRYPT (BLOCK0)   Disable flash encryption in dl mode                 = True R/W (0b1)
DIS_PAD_JTAG (BLOCK0)                  Permanently disable JTAG access via pads            = True R/W (0b1)
DIS_USB_JTAG (BLOCK0)                  Set this bit to disable function of usb-jtag        = True R/W (0b1)
KEY_PURPOSE_0 (BLOCK0)                 KEY0 purpose                                        = XTS_AES_128_KEY R/W (0x4)
KEY_PURPOSE_1 (BLOCK0)                 KEY1 purpose                                        = SECURE_BOOT_DIGEST0 R/W (0x9)
RD_DIS (BLOCK0)                        Disable reading from BLOCK4-10                      = 1 R/W (0b0000001)'

# Release-mode chip whose RD_DIS bit 0 isn't set — the encryption key in
# BLOCK_KEY0 is still readable via UART. Should fail verification even though
# everything else looks right.
RELEASE_NO_RD_DIS_EFUSE='Security fuses:
SECURE_BOOT_EN (BLOCK0)                Enable secure boot                                  = True R/W (0b1)
SPI_BOOT_CRYPT_CNT (BLOCK0)            Enables flash encryption                            = 7 R/W (0b111)
DIS_DOWNLOAD_MANUAL_ENCRYPT (BLOCK0)   Disable flash encryption in dl mode                 = True R/W (0b1)
DIS_PAD_JTAG (BLOCK0)                  Permanently disable JTAG access via pads            = True R/W (0b1)
DIS_USB_JTAG (BLOCK0)                  Set this bit to disable function of usb-jtag        = True R/W (0b1)
KEY_PURPOSE_0 (BLOCK0)                 KEY0 purpose                                        = XTS_AES_128_KEY R/W (0x4)
KEY_PURPOSE_1 (BLOCK0)                 KEY1 purpose                                        = SECURE_BOOT_DIGEST0 R/W (0x9)
RD_DIS (BLOCK0)                        Disable reading from BLOCK4-10                      = 0 R/W (0b0000000)'

# ----------------------------------------------------------------------------
# Test harness
# ----------------------------------------------------------------------------

PASS=0
FAIL=0
FAILURES=()

assert_eq() {
    local name="$1" expected="$2" actual="$3"
    if [ "$expected" = "$actual" ]; then
        PASS=$((PASS + 1))
        printf '  \033[0;32m✓\033[0m %s\n' "$name"
    else
        FAIL=$((FAIL + 1))
        FAILURES+=("$name")
        printf '  \033[0;31m✗\033[0m %s\n' "$name"
        printf '      expected: %s\n' "$expected"
        printf '      actual:   %s\n' "$actual"
    fi
}

# Run verify_chip_post_init silently and print only its exit code, so tests
# can compare against expected pass/fail without polluting stdout.
verify_exit() {
    local mode="$1" efuse="$2"
    verify_chip_post_init "$mode" "$efuse" >/dev/null 2>&1
    echo $?
}

# ----------------------------------------------------------------------------
# Tests: classify_chip_state
# ----------------------------------------------------------------------------

echo ""
echo "== classify_chip_state =="
assert_eq "fresh chip"                        "fresh"         "$(classify_chip_state "$FRESH_EFUSE")"
assert_eq "development chip"                  "development"   "$(classify_chip_state "$DEV_EFUSE")"
assert_eq "release chip"                      "release"       "$(classify_chip_state "$RELEASE_EFUSE")"
assert_eq "stuck-release chip"                "stuck-release" "$(classify_chip_state "$STUCK_RELEASE_EFUSE")"
assert_eq "release-no-rd-dis still classifies as release" \
                                              "release"       "$(classify_chip_state "$RELEASE_NO_RD_DIS_EFUSE")"

# ----------------------------------------------------------------------------
# Tests: verify_chip_post_init
# ----------------------------------------------------------------------------

echo ""
echo "== verify_chip_post_init =="
assert_eq "release fixture passes release verify"      "0" "$(verify_exit release     "$RELEASE_EFUSE")"
assert_eq "release fixture fails dev verify"           "1" "$(verify_exit development "$RELEASE_EFUSE")"
assert_eq "dev fixture passes dev verify"              "0" "$(verify_exit development "$DEV_EFUSE")"
assert_eq "dev fixture fails release verify"           "1" "$(verify_exit release     "$DEV_EFUSE")"
assert_eq "stuck-release fails release verify"         "1" "$(verify_exit release     "$STUCK_RELEASE_EFUSE")"
assert_eq "fresh chip fails any verify (release)"      "1" "$(verify_exit release     "$FRESH_EFUSE")"
assert_eq "fresh chip fails any verify (development)"  "1" "$(verify_exit development "$FRESH_EFUSE")"
assert_eq "release-no-rd-dis fails release verify"     "1" "$(verify_exit release     "$RELEASE_NO_RD_DIS_EFUSE")"

# ----------------------------------------------------------------------------
# Tests: extract_efuse_field
# ----------------------------------------------------------------------------

echo ""
echo "== extract_efuse_field =="
assert_eq "extracts True/False"        "True"           "$(extract_efuse_field "$RELEASE_EFUSE" "SECURE_BOOT_EN" "True|False")"
assert_eq "extracts 0b binary"          "0b111"          "$(extract_efuse_field "$RELEASE_EFUSE" "SPI_BOOT_CRYPT_CNT" "0b[01]+")"
assert_eq "extracts XTS_AES_128_KEY"    "XTS_AES_128_KEY" "$(extract_efuse_field "$RELEASE_EFUSE" "KEY_PURPOSE_0" "XTS_AES_128_KEY|USER")"
assert_eq "extracts SECURE_BOOT_DIGEST0" "SECURE_BOOT_DIGEST0" "$(extract_efuse_field "$RELEASE_EFUSE" "KEY_PURPOSE_1" "SECURE_BOOT_DIGEST0|USER")"
assert_eq "missing field returns empty" ""              "$(extract_efuse_field "$RELEASE_EFUSE" "NONEXISTENT_FIELD" "True|False")"
# The grep is anchored to start-of-line so a substring match shouldn't fire:
PARTIAL_LINE='Some random line ending with SPI_BOOT_CRYPT_CNT (BLOCK0) = 5 R/W (0b101)'
assert_eq "anchored grep ignores mid-line matches" "" \
    "$(extract_efuse_field "$PARTIAL_LINE" "SPI_BOOT_CRYPT_CNT" "0b[01]+")"

# ----------------------------------------------------------------------------
# Tests: detect_flash_encryption_mode
# ----------------------------------------------------------------------------

echo ""
echo "== detect_flash_encryption_mode =="
TMP=$(mktemp)
trap 'rm -f "$TMP"' EXIT

cat > "$TMP" <<'EOF'
CONFIG_SECURE_FLASH_ENCRYPTION_MODE_DEVELOPMENT=y
EOF
assert_eq "development sdkconfig"        "development" "$(detect_flash_encryption_mode "$TMP")"

cat > "$TMP" <<'EOF'
CONFIG_SECURE_FLASH_ENCRYPTION_MODE_RELEASE=y
EOF
assert_eq "release sdkconfig"            "release"     "$(detect_flash_encryption_mode "$TMP")"

cat > "$TMP" <<'EOF'
# nothing relevant here
CONFIG_SOMETHING_ELSE=y
EOF
assert_eq "neither flag set → unknown"   "unknown"     "$(detect_flash_encryption_mode "$TMP")"

assert_eq "missing file → unknown"        "unknown"     "$(detect_flash_encryption_mode "/nonexistent/path/to/sdkconfig")"

# ----------------------------------------------------------------------------
# Summary
# ----------------------------------------------------------------------------

TOTAL=$((PASS + FAIL))
echo ""
echo "============================================================"
if [ $FAIL -eq 0 ]; then
    printf '\033[0;32m✓ All %d tests passed\033[0m\n' "$TOTAL"
    exit 0
else
    printf '\033[0;31m✗ %d/%d tests failed:\033[0m\n' "$FAIL" "$TOTAL"
    for f in "${FAILURES[@]}"; do
        printf '  - %s\n' "$f"
    done
    exit 1
fi
