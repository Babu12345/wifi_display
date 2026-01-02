# Claude Code Instructions

## Project Structure Rules

### Do NOT modify the `esp-idf/` directory
The `esp-idf/` directory contains ESP-IDF framework code and configuration. Do not:
- Create new files in `esp-idf/`
- Modify existing files in `esp-idf/` unless explicitly requested
- Add scripts or documentation to `esp-idf/secure_bootloader/`

**Why:** The esp-idf directory is managed separately and may be regenerated or updated independently.

### Where to put project files
- Scripts (like `secure-flash.sh`, `build-bootloader.sh`) → `main/`
- Documentation → `main/` (e.g., `SECURE_BOOT.md`)
- Rust source code → `main/src/`
- Project-level config → root directory

## Key Files Reference

| File | Purpose |
|------|---------|
| `main/secure-flash.sh` | Build, sign, encrypt, and flash firmware |
| `main/build-bootloader.sh` | Helper script for bootloader management |
| `main/SECURE_BOOT.md` | Secure boot and flash encryption documentation |
| `main/partitions_secure.csv` | Partition table for secure boot mode |
| `esp-idf/secure_bootloader/` | ESP-IDF bootloader project (external reference only) |

## ESP32-C3 Secure Boot Notes

- **Development mode**: Allows UART flashing after first boot
- **Release mode**: Permanently disables UART flashing (OTA only)
- Always rebuild bootloader after changing menuconfig settings: `./main/build-bootloader.sh --clean`
- eFuse burns are **irreversible**
