#!/usr/bin/env bash
# Build, package, and publish firmware for OTA distribution.
# Default: dev mode (unsigned). Pass --secure to sign with the secure boot key.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

if [[ -f "${SCRIPT_DIR}/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "${SCRIPT_DIR}/.env"
    set +a
fi

if [[ -z "${FIRMWARE_HOST_DIR:-}" || -z "${FIRMWARE_BASE_URL:-}" ]]; then
    echo "Error: FIRMWARE_HOST_DIR and FIRMWARE_BASE_URL must be set in main/.env" >&2
    exit 1
fi

WEBSITE_FIRMWARE_DIR="${FIRMWARE_HOST_DIR}"
SIGNING_KEY="${PROJECT_ROOT}/secure_boot_signing_key.pem"
TARGET_ELF="${SCRIPT_DIR}/target/riscv32imc-unknown-none-elf/release/main"

SECURE=0
VERSION=""

usage() {
    cat <<EOF
Usage: $0 <version> [--secure]

Arguments:
  <version>   Semantic version string (e.g. 1.0.0)

Options:
  --secure    Sign with secure boot V2 key (default: unsigned)
  -h, --help  Show this help

Environment (loaded from main/.env):
  FIRMWARE_HOST_DIR  Website firmware directory (required)
  FIRMWARE_BASE_URL  Public base URL for hosted firmware (required)

Outputs:
  firmware.bin copied to <host-dir>/<version>/firmware.bin
  MQTT trigger JSON printed to stdout
EOF
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --secure) SECURE=1; shift ;;
        -h|--help) usage 0 ;;
        -*) echo "Unknown option: $1" >&2; usage 1 ;;
        *)
            if [[ -z "$VERSION" ]]; then
                VERSION="$1"
            else
                echo "Unexpected argument: $1" >&2
                usage 1
            fi
            shift
            ;;
    esac
done

if [[ -z "$VERSION" ]]; then
    echo "Error: version argument required" >&2
    usage 1
fi

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Error: version must match semver (e.g. 1.0.0), got: $VERSION" >&2
    exit 1
fi

cd "$SCRIPT_DIR"

echo ">>> Building release firmware..."
cargo build --release

OUT_DIR="${WEBSITE_FIRMWARE_DIR}/${VERSION}"
mkdir -p "$OUT_DIR"
OUT_BIN="${OUT_DIR}/firmware.bin"

echo ">>> Saving flashable image to ${OUT_BIN}..."
espflash save-image --chip esp32c3 "$TARGET_ELF" "$OUT_BIN"

if [[ "$SECURE" -eq 1 ]]; then
    if [[ ! -f "$SIGNING_KEY" ]]; then
        echo "Error: secure boot signing key not found at ${SIGNING_KEY}" >&2
        exit 1
    fi
    echo ">>> Signing with secure boot V2 key..."
    espsecure.py sign_data --version 2 --keyfile "$SIGNING_KEY" "$OUT_BIN"
fi

SIZE=$(stat -f%z "$OUT_BIN")
CRC32=$(python3 -c "import binascii; print(binascii.crc32(open('${OUT_BIN}','rb').read()) & 0xFFFFFFFF)")

URL="${FIRMWARE_BASE_URL}/${VERSION}/firmware.bin"
TRIGGER_JSON="{\"url\":\"${URL}\",\"version\":\"${VERSION}\",\"size\":${SIZE},\"crc32\":${CRC32}}"

# Write latest.json at the firmware-host root so the Lambda / server side
# can publish the latest trigger without recomputing size+CRC.
LATEST_JSON="${WEBSITE_FIRMWARE_DIR}/latest.json"
echo "$TRIGGER_JSON" > "$LATEST_JSON"

echo ""
echo ">>> Published firmware v${VERSION}"
echo "    Path:   ${OUT_BIN}"
echo "    Latest: ${LATEST_JSON}"
echo "    Size:   ${SIZE} bytes"
echo "    CRC32:  ${CRC32}"
echo "    Signed: $([[ $SECURE -eq 1 ]] && echo yes || echo no)"
echo ""
echo ">>> MQTT trigger payload (publish to {client_id}/root/ota):"
echo ""
echo "$TRIGGER_JSON"
echo ""
echo ">>> Next steps:"
echo "    cd ${WEBSITE_FIRMWARE_DIR%/firmware}/.."
echo "    git add docs/firmware/${VERSION}/ docs/firmware/latest.json"
echo "    git commit -m 'Publish firmware v${VERSION}'"
echo "    git push"
