# S31-Korvo display configuration

`bsp_display_factory.c` is based on `src/bsp_display.c` from the Apache-2.0
licensed `espressif/esp32_s31_korvo_1` BSP, version 1.1.0, which is pinned in
the project. The build substitutes this file for that BSP source; it does not
change the managed component or the ESP-IDF driver.

The pixel clock, panel timings, DMA burst size, and direct PSRAM framebuffer
configuration follow Espressif's S31-Korvo factory example:

- https://github.com/espressif/esp-dev-kits/blob/master/examples/esp32-s31-korvo/examples/common_components/esp32_s31_korvo/esp32_s31_korvo.c
- https://github.com/espressif/esp-dev-kits/blob/master/examples/esp32-s31-korvo/examples/common_components/esp32_s31_korvo/include/bsp/display.h

The application keeps its existing LVGL adapter, framebuffer count, GPIOs,
and touch setup. A boot log records the actual panel configuration. When
updating the BSP, compare its display adapter and these factory settings and
rebase this file before updating the dependency pin.
