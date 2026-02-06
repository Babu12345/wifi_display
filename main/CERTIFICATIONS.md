# Device Certification Guide

This document outlines the certifications required to sell the WiFi display device commercially.

## Overview

For a WiFi-enabled ESP32-C3 device, certifications vary by target market. Using a pre-certified module significantly reduces costs and complexity.

---

## Certifications by Market

### United States

| Certification | Purpose | Link |
|---------------|---------|------|
| **FCC Part 15** | Radio frequency emissions | [FCC Equipment Authorization](https://www.fcc.gov/engineering-technology/laboratory-division/general/equipment-authorization) |
| **FCC ID** | Required for intentional radiators (WiFi) | [FCC ID Search/Application](https://www.fcc.gov/oet/ea/fccid) |

### European Union

| Certification | Purpose | Link |
|---------------|---------|------|
| **CE Marking** | General market access | [CE Marking Overview](https://single-market-economy.ec.europa.eu/single-market/ce-marking_en) |
| **RED** | Radio Equipment Directive 2014/53/EU | [RED Directive](https://single-market-economy.ec.europa.eu/sectors/electrical-and-electronic-engineering-industries-eei/radio-equipment-directive-red_en) |
| **RoHS** | Hazardous substances restriction | [RoHS Directive](https://environment.ec.europa.eu/topics/waste-and-recycling/rohs-directive_en) |

### Other Markets

| Market | Certification | Link |
|--------|---------------|------|
| **UK** | UKCA | [UKCA Marking](https://www.gov.uk/guidance/using-the-ukca-marking) |
| **Canada** | ISED (IC) | [ISED Certification](https://ised-isde.canada.ca/site/spectrum-management-telecommunications/en/learn-more/key-documents/procedures/radio-standards-specification-rss-documents) |
| **Japan** | TELEC/MIC | [TELEC](https://www.telec.or.jp/eng/) |
| **Australia** | RCM | [ACMA RCM](https://www.acma.gov.au/rcm-mark) |

### Additional Certifications

| Type | When Needed |
|------|-------------|
| **UL/ETL** (Safety) | Required by major US retailers |
| **WEEE** | EU e-waste compliance |
| **Bluetooth SIG** | If using Bluetooth features |

---

## Pre-Certified Modules

The ESP32-C3 module may already be certified by Espressif for FCC/CE/IC, which significantly reduces testing costs.

**Check existing certifications:** [Espressif Certifications](https://www.espressif.com/en/support/documents/certificates)

### Requirements to Claim Module Certification

- Do not modify the antenna
- Use Espressif's reference design
- Keep module shielding intact
- Stay within module's rated power levels

Even with a pre-certified module, you still need:
1. Final product testing (unintentional emissions, safety)
2. Integration testing to ensure your design doesn't invalidate module certification

---

## Testing Labs

### Major Accredited Labs (US & International)

| Lab | Specialization | Link |
|-----|----------------|------|
| **Element Materials Technology** | Full EMC/RF testing, startup-friendly | [element.com](https://www.element.com/connected-technologies/wireless-connectivity) |
| **UL Solutions** | Safety + EMC, good for retail | [ul.com](https://www.ul.com/services/emc-testing) |
| **Intertek (ETL)** | EMC, RF, safety - full service | [intertek.com](https://www.intertek.com/electrical/emc/) |
| **TÜV Rheinland** | Strong for CE/EU markets | [tuv.com](https://www.tuv.com/world/en/emc-testing.html) |
| **Bureau Veritas** | Global coverage | [bureauveritas.com](https://www.bureauveritas.com/needs/certification-of-products-and-services/electrical-electronic) |
| **Eurofins \| MET Labs** | EMC specialists | [eurofins.com](https://www.eurofins.com/electrical-and-wireless/services/emc-testing/) |

### Budget-Friendly US Labs

| Lab | Location | Pre-scan | Full Test | Link |
|-----|----------|----------|-----------|------|
| **F2 Labs** | California | ~$400 | ~$2,500 | [yourfcc.com](https://www.yourfcc.com/) |
| **Rhein Tech Laboratories** | Virginia | ~$500 | ~$3,000 | [rheintech.com](https://www.rheintech.com/) |
| **EMC Integrity** | California | ~$500 | ~$2,800 | [emcintegrity.com](https://emcintegrity.com/) |
| **Compliance Testing** | Colorado | ~$450 | ~$2,500 | [compliancetesting.com](https://www.compliancetesting.com/) |
| **Washington Laboratories** | Maryland | ~$500 | ~$3,000 | [wll.com](https://www.wll.com/) |
| **NTS** | Multiple US | ~$600 | ~$3,500 | [nts.com](https://www.nts.com/services/testing/emc/) |

---

## Testing Process

```
1. Pre-scan testing ($500-2K)
         ↓
2. Fix any issues found
         ↓
3. Formal compliance testing ($3K-15K)
         ↓
4. Lab issues test report
         ↓
5. File for FCC ID / CE marking
```

### What to Request from Labs

1. **Pre-scan / Pre-compliance testing** (~$500-2,000)
   - Identifies issues before formal testing
   - Highly recommended first step

2. **FCC Part 15 Subpart B** (Unintentional emissions)
   - Required even with pre-certified WiFi module

3. **FCC Part 15 Subpart C** (Intentional radiator)
   - Module integration verification
   - Confirms antenna/enclosure doesn't break module certification

4. **CE EMC + RED testing** (If targeting EU)

5. **Safety testing** (If needed for retailers)

---

## Cost Estimates

### By Market

| Market | Minimum Cost |
|--------|--------------|
| **US only (FCC)** | $2,500 - $4,000 |
| **US + EU (FCC + CE)** | $5,000 - $8,000 |
| **US + EU + Safety (UL)** | $10,000+ |

### By Approach

| Approach | Cost | Notes |
|----------|------|-------|
| Pre-certified module + unintentional emissions only | $500 - $2,000 | Cheapest option |
| Full testing at budget lab | $2,500 - $4,000 | Recommended |
| Full testing at major lab (UL, Intertek) | $5,000 - $15,000 | Required for some retailers |

---

## Cheapest Certification Strategy

1. **Use a pre-certified ESP32-C3 module** (e.g., ESP32-C3-WROOM-02)
2. **Do not modify the antenna** - use module's built-in or reference design
3. **Get a pre-scan first** (~$400-500) to catch issues early
4. **Test at a regional lab** (not UL/Intertek)
5. **Target US market only initially** - add CE later if sales justify it

**Estimated minimum budget:** ~$2,500 - $3,500 for FCC with pre-certified module

---

## Tips

- **Get 2-3 quotes** - prices vary significantly between labs
- **Ask about turnaround time** - typically 2-4 weeks
- **Send your module datasheet** - labs will confirm what testing applies
- **Pre-scan first** - catching issues early saves thousands
- **Rent lab time** ($150-300/hour) for DIY pre-compliance debugging

---

## Resources

- [Espressif Module Certifications](https://www.espressif.com/en/support/documents/certificates)
- [FCC Equipment Authorization](https://www.fcc.gov/engineering-technology/laboratory-division/general/equipment-authorization)
- [CE Marking Guide](https://single-market-economy.ec.europa.eu/single-market/ce-marking_en)
