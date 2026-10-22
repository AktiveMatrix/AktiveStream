#ifndef AKTIVE_CVIRTUALDISPLAY_H
#define AKTIVE_CVIRTUALDISPLAY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Creates a software virtual display using the private CoreGraphics CGVirtualDisplay API, so a
// headless / clamshell-closed Mac with no attached screen still has a display for ScreenCaptureKit
// to capture. Returns the CGDirectDisplayID of the new display, or 0 on failure. The display
// persists for the life of the process (an internal strong reference retains it).
uint32_t aktive_virtual_display_create(uint32_t width, uint32_t height, double refresh_hz);

#ifdef __cplusplus
}
#endif

#endif
