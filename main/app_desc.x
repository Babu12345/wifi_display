/* Place esp_app_desc_t at the start of RODATA segment */
SECTIONS {
  .rodata_desc : ALIGN(4)
  {
    . = ALIGN(4);
    KEEP(*(.rodata_desc .rodata_desc.*))
    . = ALIGN(4);
  } > RODATA
}
INSERT BEFORE .rodata;
