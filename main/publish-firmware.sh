#!/usr/bin/env bash
# Build, package, and publish firmware for OTA distribution.
# Default: signed with the secure boot key. Pass --insecure for an unsigned dev build.

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

SECURE=1
VERSION=""

usage() {
    cat <<EOF
Usage: $0 <version> [--insecure]

Arguments:
  <version>   Semantic version string (e.g. 1.0.0)

Options:
  --insecure  Skip secure boot signing (default: signed with secure boot V2 key)
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
        --insecure) SECURE=0; shift ;;
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
if [[ "$SECURE" -eq 1 ]]; then
    cargo build --release --features secure-boot,production
else
    cargo build --release
fi

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
    VENV_DIR="${PROJECT_ROOT}/.venv"
    if [[ ! -d "$VENV_DIR" ]]; then
        echo "Error: Python venv not found at ${VENV_DIR}" >&2
        echo "Create one with: python3 -m venv .venv && source .venv/bin/activate && pip install esptool" >&2
        exit 1
    fi
    # shellcheck disable=SC1091
    source "${VENV_DIR}/bin/activate"
    echo ">>> Signing with secure boot V2 key..."
    SIGNED_TMP="${OUT_BIN}.signed"
    espsecure.py sign_data --version 2 --keyfile "$SIGNING_KEY" --output "$SIGNED_TMP" "$OUT_BIN"
    mv "$SIGNED_TMP" "$OUT_BIN"
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
# Resolve the paper-portrait repo root from the firmware dir.
# FIRMWARE_HOST_DIR is expected to live at <repo>/web/public/firmware, so the
# repo root is three levels up.
REPO_ROOT="$(cd "${WEBSITE_FIRMWARE_DIR}/../../.." && pwd)"
REL_FIRMWARE="$(python3 -c "import os,sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))" "$WEBSITE_FIRMWARE_DIR" "$REPO_ROOT")"

echo ">>> Next steps:"
echo "    cd ${REPO_ROOT}"
echo "    git add ${REL_FIRMWARE}/${VERSION}/ ${REL_FIRMWARE}/latest.json"
echo "    git commit -m 'Publish firmware v${VERSION}'"
echo "    git push"
echo ""
echo "    # AWS Amplify Hosting will redeploy the web/ app on push and serve"
echo "    # the new binary at: ${URL}"
