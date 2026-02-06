# WiFi Display - Hardware Block Diagram

## Product Overview

**Product Name:** WiFi Display
**Model:** WD-01
**Description:** WiFi-enabled e-paper display with NFC provisioning

---

## System Block Diagram

```
                                    +------------------+
                                    |   Power Supply   |
                                    |    (5V USB)      |
                                    +--------+---------+
                                             |
                                             | 3.3V
                                             v
+------------------+              +-------------------------+              +------------------+
|                  |    I2C      |                         |    SPI      |                  |
|   NFC Tag        |<----------->|      ESP32-C3           |<----------->|   E-Paper        |
|   STM25DV64KC    |  SCL/SDA    |   (Main Controller)     |  MOSI/SCK   |   Display        |
|                  |             |                         |             |   4.2" 400x300   |
+------------------+             |   - WiFi 802.11 b/g/n   |             +------------------+
        ^                        |   - 160MHz RISC-V       |
        |                        |   - 400KB SRAM          |
   NFC/RFID                      |   - 4MB Flash           |
   13.56 MHz                     |                         |
        |                        +------------+------------+
        v                                     |
+------------------+                          |
|   Smartphone     |                          | WiFi 2.4GHz
|   (NFC Writer)   |                          |
+------------------+                          v
                                    +------------------+
                                    |   WiFi Router    |
                                    |   / Cloud        |
                                    +------------------+
```

---

## Detailed Pin Connections

### ESP32-C3 Module (Pre-certified)

| GPIO | Function | Connected To | Interface |
|------|----------|--------------|-----------|
| GPIO2 | Output | Status LED | Digital |
| GPIO3 | Output | NFC VCC Control | Digital |
| GPIO4 | Output | E-Paper RST | Digital |
| GPIO5 | Output | E-Paper DC | Digital |
| GPIO6 | I2C SCL | NFC Tag | I2C |
| GPIO7 | I2C SDA | NFC Tag | I2C |
| GPIO8 | SPI SCK | E-Paper CLK | SPI |
| GPIO9 | Input | NFC GPO | Digital |
| GPIO10 | SPI MOSI | E-Paper DIN | SPI |
| GPIO20 | Input | E-Paper BUSY | Digital |
| GPIO21 | Output | E-Paper CS | SPI |

---

## Component Specifications

### 1. Main Controller - ESP32-C3

| Parameter | Value |
|-----------|-------|
| Manufacturer | Espressif |
| Module | ESP32-C3-WROOM-02 (or similar) |
| CPU | 32-bit RISC-V, 160MHz |
| WiFi | 802.11 b/g/n, 2.4GHz |
| Bluetooth | BLE 5.0 (available, not used) |
| Flash | 4MB |
| SRAM | 400KB |
| Operating Voltage | 3.0V - 3.6V |
| Operating Temp | -40C to +85C |
| **FCC ID** | (Check module certification) |
| **CE** | (Check module certification) |

### 2. E-Paper Display - 4.2" EPD

| Parameter | Value |
|-----------|-------|
| Display Type | E-Paper / E-Ink |
| Resolution | 400 x 300 pixels |
| Size | 4.2 inches diagonal |
| Colors | Black/White |
| Interface | SPI (4-wire) |
| Refresh Time | ~2-4 seconds |
| Operating Voltage | 3.3V |
| Driver IC | SSD1683 or similar |

### 3. NFC Tag - STM25DV64KC

| Parameter | Value |
|-----------|-------|
| Manufacturer | STMicroelectronics |
| Type | Dynamic NFC/RFID EEPROM |
| Memory | 64Kbit (8KB) |
| RF Frequency | 13.56 MHz |
| Interface | I2C (up to 1MHz) |
| NFC Standard | ISO 15693, NFC Forum Type 5 |
| Operating Voltage | 1.8V - 5.5V |
| Operating Temp | -40C to +85C |

### 4. Power Supply

| Parameter | Value |
|-----------|-------|
| Input Voltage | 5V DC (USB) |
| Regulated Output | 3.3V |
| Current (Active WiFi) | ~120-350mA |
| Current (Deep Sleep) | <10uA |

---

## RF Information

### WiFi (ESP32-C3)

| Parameter | Value |
|-----------|-------|
| Frequency Band | 2.4 GHz (2400 - 2483.5 MHz) |
| Modulation | OFDM, DSSS |
| TX Power | Up to +21 dBm |
| Standards | IEEE 802.11 b/g/n |
| Antenna | PCB Antenna / Chip Antenna |
| **Pre-certified** | Yes (Espressif module) |

### NFC (STM25DV64KC)

| Parameter | Value |
|-----------|-------|
| Frequency | 13.56 MHz |
| Protocol | ISO 15693 |
| Range | <5cm (passive) |
| **Note** | Passive tag, no intentional radiator |

---

## Functional Block Diagram

```
+-----------------------------------------------------------------------+
|                           WiFi Display System                          |
+-----------------------------------------------------------------------+
|                                                                        |
|  +-------------+     +------------------+     +--------------------+   |
|  |             |     |                  |     |                    |   |
|  |   Power     |---->|   ESP32-C3       |---->|   E-Paper Display  |   |
|  |   (3.3V)    |     |   Controller     |     |   (400x300)        |   |
|  |             |     |                  |     |                    |   |
|  +-------------+     +--------+---------+     +--------------------+   |
|                               |                                        |
|                               |                                        |
|                      +--------v---------+                              |
|                      |                  |                              |
|                      |   NFC Tag        |                              |
|                      |   (STM25DV64KC)  |                              |
|                      |                  |                              |
|                      +--------+---------+                              |
|                               |                                        |
|                               | 13.56 MHz                              |
|                               v                                        |
|                      +------------------+                              |
|                      |   NFC Field      |                              |
|                      |   (Smartphone)   |                              |
|                      +------------------+                              |
|                                                                        |
+-----------------------------------------------------------------------+

Data Flow:
1. User taps smartphone to NFC tag to provision WiFi credentials
2. ESP32-C3 reads credentials from NFC tag via I2C
3. ESP32-C3 connects to WiFi network
4. ESP32-C3 receives display content via MQTT over WiFi
5. ESP32-C3 renders content to E-Paper display via SPI
```

---

## Interfaces Summary

| Interface | Speed | Devices |
|-----------|-------|---------|
| SPI | 20 MHz | E-Paper Display |
| I2C | 100 kHz (standard) | NFC Tag (STM25DV64KC) |
| WiFi | 802.11n (up to 150Mbps) | Cloud/Router |
| NFC | 13.56 MHz | Smartphone (external) |

---

## Compliance Notes

1. **WiFi Module**: Uses pre-certified ESP32-C3 module from Espressif
   - Antenna design follows Espressif reference design
   - No modifications to RF section

2. **NFC Tag**: Passive device (STM25DV64KC)
   - No intentional RF emissions
   - Powered by external NFC field

3. **EMC Considerations**:
   - SPI and I2C traces kept short
   - Proper decoupling capacitors on power rails
   - Ground plane under RF section

---

## Contact Information

**Company:** Wanyeki Technologies
**Contact:** Babuabel Wanyeki
**Email:** babs@wanyekitech.com
**Phone:** 9526070855
