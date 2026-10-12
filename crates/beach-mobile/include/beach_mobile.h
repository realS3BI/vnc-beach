// C interface of the VNC Beach client (crates/beach-mobile).
// Keep in step with src/ffi.rs; the Swift app imports this via module.modulemap.
#ifndef BEACH_MOBILE_H
#define BEACH_MOBILE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct BeachSession BeachSession;

enum {
    BEACH_EVENT_FRAME = 0,
    BEACH_EVENT_CLIPBOARD = 1,
    BEACH_EVENT_CLOSED = 2,
    BEACH_EVENT_RESIZED = 3,
};

enum {
    BEACH_REMOTE_OTHER = 0,
    BEACH_REMOTE_BEACH_MAC = 1,
    BEACH_REMOTE_APPLE_SCREEN_SHARING = 2,
};

// Runs on the reader thread. `text` is set for CLIPBOARD and CLOSED and is
// only valid during the call.
typedef void (*BeachEventCallback)(void *context, int32_t kind, const char *text);

// One of the server's displays inside the framebuffer.
typedef struct BeachScreen {
    uint32_t id;
    uint32_t x;
    uint32_t y;
    uint32_t width;
    uint32_t height;
} BeachScreen;

typedef void (*BeachFrameCallback)(void *context, const uint8_t *rgba, uint32_t width, uint32_t height,
                                   uint32_t dirty_x, uint32_t dirty_y, uint32_t dirty_width,
                                   uint32_t dirty_height);

BeachSession *beach_session_connect(const char *address, const char *password, uint8_t depth,
                                    BeachEventCallback callback, void *context, char *error,
                                    size_t error_len);
void beach_session_free(BeachSession *session);
void beach_session_name(BeachSession *session, char *buf, size_t len);
int32_t beach_session_remote_kind(BeachSession *session);
void beach_session_size(BeachSession *session, uint32_t *width, uint32_t *height);
bool beach_session_take_frame(BeachSession *session, BeachFrameCallback callback, void *context);
size_t beach_session_screens(BeachSession *session, BeachScreen *out, size_t max);
void beach_session_set_region(BeachSession *session, uint32_t x, uint32_t y, uint32_t width, uint32_t height);
void beach_session_pointer(BeachSession *session, uint8_t buttons, uint16_t x, uint16_t y);
void beach_session_wheel(BeachSession *session, uint8_t buttons, uint8_t wheel_bit, uint16_t x, uint16_t y);
void beach_session_key(BeachSession *session, uint32_t keysym, bool down);
void beach_session_cut_text(BeachSession *session, const char *text);
void beach_session_refresh(BeachSession *session);
uint32_t beach_keysym_for_char(uint32_t scalar);
void beach_normalize_address(const char *input, char *buf, size_t len);

// RFB pointer button mask bits.
#define BEACH_BUTTON_LEFT 1
#define BEACH_BUTTON_MIDDLE 2
#define BEACH_BUTTON_RIGHT 4
#define BEACH_WHEEL_UP 8
#define BEACH_WHEEL_DOWN 16
#define BEACH_WHEEL_LEFT 32
#define BEACH_WHEEL_RIGHT 64

// X11 keysyms (same values as crates/rfb/src/keysym.rs).
#define BEACH_KEY_BACKSPACE 0xff08
#define BEACH_KEY_TAB 0xff09
#define BEACH_KEY_RETURN 0xff0d
#define BEACH_KEY_ESCAPE 0xff1b
#define BEACH_KEY_INSERT 0xff63
#define BEACH_KEY_DELETE 0xffff
#define BEACH_KEY_HOME 0xff50
#define BEACH_KEY_END 0xff57
#define BEACH_KEY_PAGE_UP 0xff55
#define BEACH_KEY_PAGE_DOWN 0xff56
#define BEACH_KEY_LEFT 0xff51
#define BEACH_KEY_UP 0xff52
#define BEACH_KEY_RIGHT 0xff53
#define BEACH_KEY_DOWN 0xff54
#define BEACH_KEY_F1 0xffbe
#define BEACH_KEY_SHIFT_L 0xffe1
#define BEACH_KEY_CONTROL_L 0xffe3
#define BEACH_KEY_CAPS_LOCK 0xffe5
#define BEACH_KEY_META_L 0xffe7
#define BEACH_KEY_ALT_L 0xffe9
#define BEACH_KEY_SUPER_L 0xffeb

#ifdef __cplusplus
}
#endif

#endif
