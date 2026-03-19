# Set Up & Disassembly Instructions

## Equipment Under Test (EUT)

- **Product Name:** Portrait WiFi eInk Display
- **Model:** PP-100
- **Manufacturer:** Wanyeki Technologies LLC

---

## Set Up Instructions

### Powering the EUT

1. Connect a USB-C cable to the port on the right side of the device
2. Connect the other end to a 5V USB power adapter (1A minimum)
3. Turn on the device using the power switch on the back left side (short press)
4. The charge LED (D2) on the back will illuminate while charging

### Configuring for Maximum Power Draw

To configure the EUT to operate continuously at highest current draw, the device must be placed in **Live Updates mode** with an active WiFi connection and frequent display refreshes. This exercises WiFi TX/RX, CPU, and the eInk display simultaneously.

**Step 1: Provide a WiFi network**

The EUT requires a 2.4GHz 802.11 b/g/n WiFi access point within range. Set up a test access point or use an existing network.

**Step 2: Install the app and sign in**

1. Download **Paper Portrait Connect** from the Apple App Store on an iPhone (7 or later, iOS 14+)
2. Sign in with the pre-configured test account (both test devices are already registered):
   - **Login:** dolors-grids.4l@icloud.com
   - **Password:** Testing_12

**Step 3: Write WiFi credentials via NFC**

1. Open the app — the two test devices will appear in the device list
2. Tap the WiFi icon on a device card to update its WiFi credentials
3. Enter the WiFi SSID and password for the test network
4. Tap "Write to Device"
5. Hold the iPhone to the back of the EUT near the top (NFC tap area)
6. The device will connect to the test network

**Step 4: Start a live update feature**

From the app, select the registered device and choose a content type with a short update interval to maximize continuous radio and display activity:

- **Clock** with 1-minute update interval, OR
- **MTA Transit** with 1-minute update interval, OR
- **Stock Prices** with 5-minute update interval

This keeps the device in a continuous cycle of: WiFi receive (MQTT) -> CPU decode -> eInk display refresh.

Further instructions are in the user manual: https://babu12345.github.io/portrait_v2_ios/manual/

### Maximum Power Draw Operating Conditions

| Component | State During Max Power |
|-----------|----------------------|
| WiFi Radio | Active — TLS 1.3 MQTT connection, TX at 12 dBm, periodic keep-alive pings |
| CPU (ESP32-C3) | 80 MHz idle, bursts to 160 MHz during TLS handshake and frame decoding |
| eInk Display | Full refresh every 1-5 minutes (400x300 pixels, SPI at 20 MHz) |
| NFC (STM25DV64KC) | Idle (powered but not actively scanning once registered) |
| USB-C Charging | Active if cable connected (BQ24040 charging IC) |

### Estimated Current Draw

| Mode | Approximate Current (3.3V rail) |
|------|-------------------------------|
| Peak (WiFi TX burst) | 300-350 mA |
| Active (WiFi connected, idle between updates) | 50-80 mA |
| Display refresh | 20-40 mA additional during update |
| Static mode (WiFi off, display idle) | 5-10 mA |

### Confirming Active Operation

The EUT is operating at maximum power when:

- The display is periodically refreshing with new content (visible brief black/white flash every 1-5 minutes)
- Status LEDs (D3/D4/D5) on the back indicate activity
- The device is warm to the touch near the ESP32-C3 module

---

## Disassembly Instructions

### Tools Required

- Phillips #0 screwdriver (M2 screws)
- Plastic spudger or pry tool
- Anti-static wrist strap (recommended)

### Disassembly Steps

1. **Power off** the device using the power switch (short press) and disconnect the USB-C cable
2. **Remove the back cover** — pry gently at the seam using a plastic spudger, working around the edges. Take care near the USB-C port and switch openings.
3. **Disconnect the battery** — locate the lithium battery connector and carefully unplug it from the PCB header. Do this before handling the board.
4. **Remove the PCB** — unscrew any fasteners securing the PCB to the enclosure. Note the positions of:
   - eInk display ribbon cable (fragile, top side)
   - NFC antenna connection
5. **Separate the eInk display** — if needed, gently release the FPC connector latch and slide the ribbon cable out. Do not bend or crease the ribbon cable.

### Key Internal Components

| Component | Location | Notes |
|-----------|----------|-------|
| ESP32-C3 module | Center of PCB | Main processor, WiFi antenna integrated |
| STM25DV64KC | Near top of PCB | NFC IC with I2C connection |
| BQ24040 | Near USB-C port | Battery charging IC |
| eInk display (400x300) | Front, connected via FPC ribbon | Fragile — handle by edges only |
| Lithium battery | Between PCB and back cover | Do not puncture or short-circuit |
| Power switch | Back left side | Mechanical switch |
| LEDs (D2-D5) | Back right side | D2 = charge, D3-D5 = status |

### Reassembly Notes

- Reconnect the battery connector before securing the PCB
- Ensure the eInk ribbon cable is fully seated and the FPC latch is closed
- Verify the power switch and USB-C port align with their enclosure openings
- After reassembly, short-press the power switch to verify the device powers on

---

**Wanyeki Technologies LLC**
Contact: babs@wanyekitech.com
