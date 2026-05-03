#!/usr/bin/env python3
"""Tail an ESP32-C3's USB-Serial-JTAG output without toggling DTR/RTS.

Used by `secure-flash.sh` after a fresh-chip --init: the bootloader needs
to encrypt flash content in place + burn release-mode eFuses on its first
boot, and ANY DTR-triggered reset (esptool, espefuse, espflash monitor)
during that window would interrupt the process and leave the chip in a
half-locked state.

This script opens the serial port read-only, with DTR/RTS held low (which
is the default un-asserted state on USB-Serial-JTAG — no reset triggered),
and prints incoming bytes until either:

  * a known "boot succeeded" marker appears
  * a known "boot failed" marker appears (exits non-zero)
  * the timeout elapses (exits non-zero)

Markers are matched anywhere in the buffered stream so they don't need to
land on a single read() call.

Usage:
    python3 wait-for-boot.py <port> [--timeout SECONDS]
"""

import argparse
import os
import sys
import time

try:
    import serial
except ImportError:
    print("error: pyserial not installed. Activate the project's .venv first.", file=sys.stderr)
    sys.exit(2)


def wait_for_port(path: str, timeout: float) -> bool:
    """Block until `path` exists on the filesystem (USB CDC enumeration), or
    until `timeout` seconds elapse. Returns True if the port appeared."""
    start = time.time()
    while time.time() - start < timeout:
        if os.path.exists(path):
            return True
        time.sleep(0.25)
    return False


SUCCESS_MARKERS = [
    b"Flash encryption completed",      # bootloader logs this after CRYPT_CNT is burned
    b"Loaded app from partition",       # bootloader hands off to the app
    b"INFO - Device client ID",          # app's first log line
]

FAILURE_MARKERS = [
    b"Signature verification failed",
    b"Image required chip rev",
    b"Initialization of Flash encryption key failed",
    b"Encryption flash contents failed",
    b"Enabling of Flash encryption failed",
    # `invalid header: 0xNNNN` from the ROM bootloader means it's reading
    # ciphertext as plaintext — typically a chip with encryption written but
    # SPI_BOOT_CRYPT_CNT not set (i.e. the bug we're trying to detect).
    # Match it more than once to avoid false positives during the brief
    # window where the ROM might emit one of these before our flash takes.
    b"invalid header: 0x",
]


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("port")
    p.add_argument("--timeout", type=int, default=120)
    p.add_argument(
        "--port-wait",
        type=int,
        default=15,
        help="seconds to wait for the port path to exist (handles USB re-enumeration)",
    )
    args = p.parse_args()

    if not wait_for_port(args.port, args.port_wait):
        print(
            f"error: {args.port} did not appear within {args.port_wait}s "
            "(USB device not enumerated)",
            file=sys.stderr,
        )
        return 2

    try:
        ser = serial.Serial()
        ser.port = args.port
        ser.baudrate = 115200
        # Critical: hold DTR/RTS low so the chip doesn't reset when we open it.
        # On USB-Serial-JTAG, the CDC reset trick toggles DTR/RTS asserted, so
        # leaving them de-asserted is a non-event for the chip.
        ser.dtr = False
        ser.rts = False
        ser.timeout = 0.5
        ser.open()
    except Exception as e:
        print(f"error opening {args.port}: {e}", file=sys.stderr)
        return 2

    print(f"--- watching {args.port} for up to {args.timeout}s, no DTR/RTS toggling ---")
    print("(everything below this line is live serial output from the chip)")
    print("-" * 60)

    buf = b""
    start = time.time()
    while time.time() - start < args.timeout:
        chunk = ser.read(256)
        if chunk:
            buf += chunk
            try:
                sys.stdout.write(chunk.decode("utf-8", errors="replace"))
            except Exception:
                pass
            sys.stdout.flush()

            for m in FAILURE_MARKERS:
                if m in buf:
                    print(f"\n--- failure marker detected: {m.decode()!r} ---")
                    ser.close()
                    return 1

            for m in SUCCESS_MARKERS:
                if m in buf:
                    # Give the chip a moment to settle past the marker
                    time.sleep(2)
                    print(f"\n--- success marker detected: {m.decode()!r} ---")
                    ser.close()
                    return 0

    print(f"\n--- timeout: {args.timeout}s elapsed without seeing a known marker ---")
    ser.close()
    return 1


if __name__ == "__main__":
    sys.exit(main())
