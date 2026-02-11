# Paper Portait PCB - EN 62368-1 Safety Certification Documentation

**Document Version:** 1.0
**Date:** 2025-01-28
**Product:** Paper Portait PCB
**Manufacturer:** Wanyeki Technologies LLC
**Website:** https://wanyekitech.com/

---

## 1. Product Overview

The Paper Portait PCB is a low-power IoT device featuring:
- ESP32-C3 WiFi/BLE microcontroller
- E-Ink display interface
- NFC communication with energy harvesting capability
- USB Type-C charging
- Li-ion battery power management

---

## 2. Safety Block Diagram

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                              PAPER PORTAIT PCB                                   │
│                         EN 62368-1 Safety Classification                         │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                  │
│  ┌──────────────────────┐                                                        │
│  │   USB INPUT (ES1)    │    ENERGY SOURCE 1: USB 5V                            │
│  │   ════════════════   │                                                        │
│  │  J4: USB4105-GF-A    │◄── USB Type-C Connector                               │
│  │  R7, R9: 5.1k ohms   │◄── CC1/CC2 Pull-downs (USB-C Detection)               │
│  │                      │                                                        │
│  └──────────┬───────────┘                                                        │
│             │ VBUS (5V)                                                          │
│             ▼                                                                    │
│  ┌──────────────────────┐     ┌─────────────────────┐  ┌─────────────────────┐ │
│  │  BATTERY CHARGER     │     │  BATTERY (ES2)      │  │  SOLAR PANEL (ES3)  │ │
│  │  ══════════════════  │     │  ═══════════════    │  │  ═══════════════    │ │
│  │  IC1: BQ24040DSQR    │◄───►│  J1: Battery        │  │  J3: Solar Input    │ │
│  │  D6: PG LED (Blue)   │     │  Connector          │  │  (620102131822)     │ │
│  │  D2: CHG LED (Red)   │     │  (620102131822)     │  └──────────┬──────────┘ │
│  │  R5: 2k (ISET)       │     └─────────────────────┘             │            │
│  │  R2: 1k (ITERM)      │◄────────────────────────────────────────┘            │
│  │  C17: 1uF (VCC)      │                                                        │
│  │  C13: 22uF (OUT)     │                                                        │
│  │  R8: 1.5k (PG)       │                                                        │
│  └──────────┬───────────┘                                                        │
│             │ VD (Battery/USB Voltage)                                           │
│             ▼                                                                    │
│  ┌──────────────────────┐                                                        │
│  │  DC-DC REGULATOR     │                                                        │
│  │  ══════════════════  │                                                        │
│  │  U3: TPS62172DSGR    │◄── Buck Regulator (5V → 3.3V)                         │
│  │  L2: 2.2uH           │                                                        │
│  │  C11: 10uF (VIN)     │                                                        │
│  │  C12: 22uF (VOUT)    │                                                        │
│  │  R11: 100k (EN)      │                                                        │
│  │  D1: Indicator LED   │                                                        │
│  │  R3: 68 ohms         │                                                        │
│  └──────────┬───────────┘                                                        │
│             │ +3V3                                                               │
│             ▼                                                                    │
│  ┌──────────────────────────────────────────────────────────────────────┐       │
│  │                        3.3V POWER DOMAIN                              │       │
│  │  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────┐   │       │
│  │  │  MICROCONTROLLER│  │  E-INK DRIVER   │  │  NFC (Energy        │   │       │
│  │  │  ══════════════ │  │  ═════════════  │  │  Harvesting Ready)  │   │       │
│  │  │ U2: ESP32-C3-   │  │ Q1: Si1308EDL   │  │  ═══════════════    │   │       │
│  │  │   WROOM-02-N4   │  │ J2: FH34SRJ-24S │  │ U4: ST25DV64KC     │   │       │
│  │  │ R4: 10k (EN)    │  │   (Display FPC) │  │ L3: W3102 (Antenna)│   │       │
│  │  │ C15: 1uF        │  │ L1: 10uH        │  │ C19: 22uF          │   │       │
│  │  │ Bypass caps:    │  │ D3,D4,D5:       │  │ R12: 10k           │   │       │
│  │  │  C9,C14: 0.1uF  │  │  SS2040FL       │  │                    │   │       │
│  │  │  C8,C10: 4.7uF  │  │ C1-C7: 1uF      │  │ I2C: SDA, SCL      │   │       │
│  │  │ R6: 1.5k (I2C)  │  │                 │  │ GPO: Interrupt     │   │       │
│  │  └─────────────────┘  └─────────────────┘  └─────────────────────┘   │       │
│  └──────────────────────────────────────────────────────────────────────┘       │
│                                                                                  │
│  ┌──────────────────────┐  ┌──────────────────────┐                             │
│  │  USER INTERFACE      │  │  POWER SWITCHES      │                             │
│  │  ══════════════════  │  │  ══════════════════  │                             │
│  │  S1: TS04-66-85      │  │  S2: EG1213          │                             │
│  │    (Tactile Button)  │  │    (Slide Switch)    │                             │
│  └──────────────────────┘  └──────────────────────┘                             │
│                                                                                  │
└─────────────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Energy Source Classification

| Energy Source | Type | Voltage | Current Limit | Classification |
|---------------|------|---------|---------------|----------------|
| ES1 - USB | External DC | 5V | 500mA - 1.5A | ES1 (≤60V DC) |
| ES2 - Battery | Internal DC | 3.7V nominal | Protected cell | ES1 (≤60V DC) |
| ES3 - Solar Panel | External DC (J3) | 4.2-6V | Panel dependent | ES1 (≤60V DC) |
| ES4 - NFC Harvesting | RF Induced | <3.3V | <10mA | ES1 |

**All circuits operate within ES1 classification - No hazardous voltage levels present.**

---

## 4. USB-Connected Components

These components are directly in the USB power/data path and require special attention for EN 62368-1 compliance:

| Reference | Part Number | Manufacturer | Description | Safety Relevance |
|-----------|-------------|--------------|-------------|------------------|
| **J4** | USB4105-GF-A | GCT | USB Type-C Connector | Primary interface, ESD protection point |
| **R7** | ERJ-1GNF5101C | Panasonic | 5.1k ohm (CC1) | USB-C configuration resistor |
| **R9** | ERJ-1GNF5101C | Panasonic | 5.1k ohm (CC2) | USB-C configuration resistor |
| **IC1** | BQ24040DSQR | Texas Instruments | Battery Charger IC | Receives VBUS directly, thermal protection |
| **C17** | CL03A105MO3NRNH | Samsung | 1uF/16V | Input filtering on USB side |

### USB Port Specifications
- **Connector Type:** USB Type-C (UFP - Upstream Facing Port)
- **Power Role:** Sink only (device charges from host)
- **Data Lines:** Not connected (power only)
- **ESD Rating:** Verify J4 (USB4105-GF-A) meets IEC 61000-4-2 Level 4

---

## 5. Complete Bill of Materials (BOM)

### 5.1 Power Input & Battery Charging

| Ref | Qty | Part Number | Manufacturer | Value | Footprint | Safety Notes |
|-----|-----|-------------|--------------|-------|-----------|--------------|
| **J4** | 1 | USB4105-GF-A | GCT | USB-C | GCT_USB4105-GF-A | **USB INPUT - Primary safety interface** |
| **R7** | 1 | ERJ-1GNF5101C | Panasonic | 5.1k ohm | R_0201 | **USB CC1 pin** |
| **R9** | 1 | ERJ-1GNF5101C | Panasonic | 5.1k ohm | R_0201 | **USB CC2 pin** |
| **IC1** | 1 | BQ24040DSQR | Texas Instruments | - | SON40P200X200X80-11N | **Charger IC - thermal protection built-in** |
| J1 | 1 | 620102131822 | Wurth Elektronik | - | 620102131822 | Battery connector |
| J3 | 1 | 620102131822 | Wurth Elektronik | - | 620102131822 | Solar panel charger input |
| D2 | 1 | 150060RS75000 | Wurth Elektronik | Red LED | LED_0603 | CHG indicator |
| D6 | 1 | 150060BS75000 | Wurth Elektronik | Blue LED | LED_0603 | Power Good indicator |
| R2 | 1 | ERJ-1GNF1001C | Panasonic | 1k ohm | R_0201 | ITERM setting |
| R5 | 1 | ERA-1AEB202C | Panasonic | 2k ohm | R_0201 | ISET (charge current) |
| R8 | 1 | ERJ-1GNF1501C | Panasonic | 1.5k ohm | R_0201 | PG pullup |
| C17 | 1 | CL03A105MO3NRNH | Samsung | 1uF/16V | C_0201 | VCC decoupling |
| C13 | 1 | CL10A226MP8NUNE | Samsung | 22uF/10V | C_0603 | Output capacitor |

### 5.2 Voltage Regulation (3.3V Supply)

| Ref | Qty | Part Number | Manufacturer | Value | Footprint | Safety Notes |
|-----|-----|-------------|--------------|-------|-----------|--------------|
| U3 | 1 | TPS62172DSGR | Texas Instruments | - | SON50P200X200X80-9N | Buck regulator, thermal shutdown |
| L2 | 1 | MLZ1608N2R2LT000 | TDK | 2.2uH | L_0603 | Power inductor |
| C11 | 1 | CL10A106MP8NNNC | Samsung | 10uF/10V | C_0603 | Input capacitor |
| C12 | 1 | CL10A226MP8NUNE | Samsung | 22uF/10V | C_0603 | Output capacitor |
| R11 | 1 | ERJ-1GNF1003C | Panasonic | 100k ohm | R_0201 | Enable pullup |
| D1 | 1 | 150060RS75000 | Wurth Elektronik | Red LED | LED_0603 | Power indicator |
| R3 | 1 | ERJ-1GNF68R0C | Panasonic | 68 ohm | R_0201 | LED current limit |

### 5.3 Microcontroller

| Ref | Qty | Part Number | Manufacturer | Value | Footprint | Safety Notes |
|-----|-----|-------------|--------------|-------|-----------|--------------|
| U2 | 1 | ESP32-C3-WROOM-02-N4 | Espressif | - | ESP32-C3-WROOM-02 | WiFi/BLE module, FCC/CE certified |
| R4 | 1 | ERJ-1GNF1002C | Panasonic | 10k ohm | R_0201 | EN pullup |
| R6 | 1 | ERJ-1GNF1501C | Panasonic | 1.5k ohm | R_0201 | I2C pullup |
| C15 | 1 | CL03A105MO3NRNH | Samsung | 1uF/16V | C_0201 | Decoupling |
| C8 | 1 | CL05A475MP5NRNC | Samsung | 4.7uF/10V | C_0402 | Bypass |
| C10 | 1 | CL05A475MP5NRNC | Samsung | 4.7uF/10V | C_0402 | Bypass |
| C9 | 1 | CL03A104KP3NNNC | Samsung | 0.1uF/10V | C_0201 | Bypass |
| C14 | 1 | CL03A104KP3NNNC | Samsung | 0.1uF/10V | C_0201 | Bypass |

### 5.4 E-Ink Display Driver

| Ref | Qty | Part Number | Manufacturer | Value | Footprint | Safety Notes |
|-----|-----|-------------|--------------|-------|-----------|--------------|
| Q1 | 1 | SI1308EDL-T1-GE3 | Vishay | N-MOSFET | SOT-323 | Display power switch |
| J2 | 1 | FH34SRJ-24S-0.5SH(99) | Hirose | 24-pin FPC | HRS_FH34SRJ-24S | Display connector |
| L1 | 1 | MLZ1608N100LT000 | TDK | 10uH | L_0603 | Filter inductor |
| D3 | 1 | SS2040FL-AU_R1_000A1 | Panjit | Schottky | SODFL3618X108N | Protection diode |
| D4 | 1 | SS2040FL-AU_R1_000A1 | Panjit | Schottky | SODFL3618X108N | Protection diode |
| D5 | 1 | SS2040FL-AU_R1_000A1 | Panjit | Schottky | SODFL3618X108N | Protection diode |
| R1 | 1 | KDV02FR470ET | Ohmite | 0.47 ohm | R_0201 | Current sense |
| C1 | 1 | CL03A105MO3NRNH | Samsung | 1uF/16V | C_0201 | Decoupling |
| C2 | 1 | CL03A105MO3NRNH | Samsung | 1uF/16V | C_0201 | Decoupling |
| C3 | 1 | CL03A105MO3NRNH | Samsung | 1uF/16V | C_0201 | Decoupling |
| C4 | 1 | CL03A105MO3NRNH | Samsung | 1uF/16V | C_0201 | Decoupling |
| C5 | 1 | CL03A105MO3NRNH | Samsung | 1uF/16V | C_0201 | Decoupling |
| C6 | 1 | CL03A105MO3NRNH | Samsung | 1uF/16V | C_0201 | Decoupling |
| C7 | 1 | CL03A105MO3NRNH | Samsung | 1uF/16V | C_0201 | Decoupling |

### 5.5 NFC Subsystem

| Ref | Qty | Part Number | Manufacturer | Value | Footprint | Safety Notes |
|-----|-----|-------------|--------------|-------|-----------|--------------|
| U4 | 1 | ST25DV64KC-IE8T3 | STMicroelectronics | - | TSSOP8_STM | NFC/RFID with energy harvesting |
| L3 | 1 | W3102 | Pulse (Yageo) | - | IND_W3102 | NFC antenna coil |
| C19 | 1 | CL10A226MP8NUNE | Samsung | 22uF/10V | C_0603 | VCC_NFC decoupling |
| R12 | 1 | ERJ-1GNF1002C | Panasonic | 10k ohm | R_0201 | GPO pullup |

### 5.6 User Interface & Switches

| Ref | Qty | Part Number | Manufacturer | Value | Footprint | Safety Notes |
|-----|-----|-------------|--------------|-------|-----------|--------------|
| S1 | 1 | TS04-66-85-BK-160-SMT | CUI Devices | Tactile | SW_TS04-66-85 | User button |
| S2 | 1 | EG1213 | E-Switch | SPDT | SW_EG1213 | Power slide switch |

---

## 6. EN 62368-1 Safety Compliance Summary

### 6.1 Applicable Clauses

| Clause | Requirement | Compliance Status |
|--------|-------------|-------------------|
| 4.1 | Electrical energy source classification | ES1 - All circuits ≤60V DC |
| 5.4 | Limited Power Source (LPS) | USB input is LPS from host |
| 6.4 | Thermal requirements | IC1, U3 have thermal protection |
| 9.2 | Mechanical hazards | No sharp edges, proper enclosure required |
| Annex Q | Battery requirements | Single cell Li-ion, protected |

### 6.2 Safety Features

1. **Overcurrent Protection:** BQ24040 includes programmable charge current limiting
2. **Thermal Protection:** BQ24040 reduces charge current at high temperatures
3. **Overvoltage Protection:** TPS62172 operates within safe input range
4. **ESD Protection:** USB connector rated for ESD per IEC 61000-4-2
5. **Low Voltage Operation:** All circuits operate at ≤5V (ES1 classification)

---

## 7. Regulatory Certifications (Module Level)

| Component | Certification | Notes |
|-----------|---------------|-------|
| ESP32-C3-WROOM-02 | FCC, CE, IC, MIC, KCC, NCC | Pre-certified WiFi/BLE module |
| ST25DV64KC | CE | NFC Forum Type 5 Tag |

---

## 8. Document Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2025-01-28 | - | Initial release |

---

*This document is intended to support EN 62368-1 safety certification. Final compliance determination must be made by an accredited test laboratory.*
