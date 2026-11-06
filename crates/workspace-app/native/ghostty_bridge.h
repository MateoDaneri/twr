#ifndef TW_GHOSTTY_BRIDGE_H
#define TW_GHOSTTY_BRIDGE_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
typedef struct TwGhostty TwGhostty;
typedef struct {
  uint16_t rows, cols, cursor_row, cursor_col;
  uint32_t flags, cursor_color;
} TwGhosttyInfo;
/* Colors use 0xffffffff for the app's theme default, otherwise 0x00rrggbb. */
typedef bool (*TwGhosttyCellFn)(void *, uint16_t, uint16_t, const uint8_t *, size_t, uint32_t, uint32_t, uint32_t);
typedef struct {
  uint64_t generation;
  int32_t col, row, z;
  uint32_t layer, offset_x, offset_y, width, height;
  uint32_t source_x, source_y, source_width, source_height;
  uint32_t image_width, image_height, format;
  const uint8_t *data;
  size_t len;
} TwGhosttyImage;
typedef bool (*TwGhosttyImageFn)(void *, const TwGhosttyImage *);
typedef struct { uint32_t width, height; uint8_t *data; size_t len; } TwGhosttyPng;
typedef bool (*TwGhosttyPngFn)(const void *, const uint8_t *, size_t, TwGhosttyPng *);
bool tw_ghostty_set_png_decoder(TwGhosttyPngFn decoder);
uint8_t *tw_ghostty_allocate_png(const void *allocator, size_t len);
TwGhostty *tw_ghostty_new(uint16_t rows, uint16_t cols);
void tw_ghostty_free(TwGhostty *terminal);
bool tw_ghostty_write(TwGhostty *terminal, const uint8_t *data, size_t len);
bool tw_ghostty_resize(TwGhostty *terminal, uint16_t rows, uint16_t cols, uint32_t width, uint32_t height);
bool tw_ghostty_scroll(TwGhostty *terminal, size_t rows_from_bottom);
/* These calls belong to the terminal's single worker thread. Selection text
   is returned only on explicit request and is bounded by caller capacity.
   On copy failure, written is zero and callers must ignore the output buffer. */
bool tw_ghostty_selection_begin(TwGhostty *terminal, uint16_t row, uint16_t col);
bool tw_ghostty_selection_extend(TwGhostty *terminal, uint16_t row, uint16_t col);
bool tw_ghostty_selection_clear(TwGhostty *terminal);
bool tw_ghostty_selection_copy(TwGhostty *terminal, uint8_t *out, size_t capacity, size_t *written);
size_t tw_ghostty_replies(TwGhostty *terminal, uint8_t *output, size_t capacity);
bool tw_ghostty_animation_tick(TwGhostty *terminal, uint64_t now_ms, bool *playing, uint64_t *next_ms);
bool tw_ghostty_snapshot(TwGhostty *terminal, TwGhosttyInfo *info, TwGhosttyCellFn cell, void *context,
                         TwGhosttyImageFn image, void *image_context);
#endif
