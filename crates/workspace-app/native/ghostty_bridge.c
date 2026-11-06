/* The pinned C API is isolated behind this small ABI owned by the application. */
#include "ghostty_bridge.h"
#include <ghostty/vt.h>
#include <stdlib.h>
#include <string.h>

struct TwGhostty {
  GhosttyTerminal terminal;
  GhosttyRenderState render;
  GhosttyRenderStateRowIterator rows;
  GhosttyRenderStateRowCells cells;
  GhosttyRenderStateImageIterator images;
  GhosttyTrackedGridRef selection_anchor;
  GhosttyTerminalScreen selection_screen;
  uint8_t replies[4096];
  size_t replies_len;
};
#define REQUIRE(call) do { if ((call) != GHOSTTY_SUCCESS) return false; } while (0)
/* Installed once before any terminal is created; no per-terminal borrowed data. */
static TwGhosttyPngFn png_decoder;
static bool decode_png(void *userdata, const GhosttyAllocator *allocator,
    const uint8_t *data, size_t len, GhosttySysImage *output) {
  (void)userdata;
  TwGhosttyPng result = {0};
  if (!png_decoder || !png_decoder(allocator, data, len, &result)) return false;
  output->width = result.width; output->height = result.height;
  output->data = result.data; output->data_len = result.len;
  return true;
}
bool tw_ghostty_set_png_decoder(TwGhosttyPngFn decoder) {
  png_decoder = decoder;
  return ghostty_sys_set(GHOSTTY_SYS_OPT_DECODE_PNG, (const void *)decode_png) == GHOSTTY_SUCCESS;
}
uint8_t *tw_ghostty_allocate_png(const void *allocator, size_t len) {
  if (!allocator || len == 0 || len > 8 * 1024 * 1024) return NULL;
  return ghostty_alloc(allocator, len);
}

static void reply(GhosttyTerminal terminal, void *context, const uint8_t *bytes, size_t len) {
  (void)terminal;
  TwGhostty *value = context;
  if (len > sizeof(value->replies) - value->replies_len) return;
  memcpy(value->replies + value->replies_len, bytes, len);
  value->replies_len += len;
}
void tw_ghostty_free(TwGhostty *value) {
  if (!value) return;
  if (value->selection_anchor) ghostty_tracked_grid_ref_free(value->selection_anchor);
  ghostty_render_state_row_cells_free(value->cells);
  ghostty_render_state_image_iterator_free(value->images);
  ghostty_render_state_row_iterator_free(value->rows);
  ghostty_render_state_free(value->render);
  ghostty_terminal_free(value->terminal);
  free(value);
}
TwGhostty *tw_ghostty_new(uint16_t rows, uint16_t cols) {
  if (rows < 2 || rows > 200 || cols < 2 || cols > 400) return NULL;
  TwGhostty *value = calloc(1, sizeof(*value));
  if (!value) return NULL;
  /* The verified Firefox Rose base palette used by this Mac's Ghostty.
     Indexed terminal colors must match too, not just the surrounding GPUI theme. */
  GhosttyColorRgb palette[256];
  ghostty_color_palette_default(palette);
  const uint32_t colors[16] = {0x021c23,0xff889a,0x56e6bd,0xffdc73,0x8dccff,0xff79bd,0x56e6bd,0xf2f8fc,
    0xa7c0cc,0xff889a,0x56e6bd,0xffdc73,0x8dccff,0xbb9bff,0x56e6bd,0xffffff};
  for (size_t i = 0; i < 16; ++i) {
    palette[i] = (GhosttyColorRgb){.r=colors[i]>>16,.g=(colors[i]>>8)&255,.b=colors[i]&255};
  }
  bool disabled = false;
  uint64_t storage = 8 * 1024 * 1024;
  size_t apc = 4 * 1024 * 1024, scroll_bytes = 16 * 1024 * 1024;
  size_t scroll_lines = 2000;
  if (ghostty_terminal_new(NULL, &value->terminal, cols, rows) != GHOSTTY_SUCCESS ||
      ghostty_render_state_new(NULL, &value->render) != GHOSTTY_SUCCESS ||
      ghostty_render_state_row_iterator_new(NULL, &value->rows) != GHOSTTY_SUCCESS ||
      ghostty_render_state_row_cells_new(NULL, &value->cells) != GHOSTTY_SUCCESS ||
      ghostty_render_state_image_iterator_new(NULL, &value->images) != GHOSTTY_SUCCESS ||
      ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, palette) != GHOSTTY_SUCCESS ||
      ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_USERDATA, value) != GHOSTTY_SUCCESS ||
      ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_WRITE_PTY, (const void *)reply) != GHOSTTY_SUCCESS ||
      ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, &storage) != GHOSTTY_SUCCESS ||
      ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE, &disabled) != GHOSTTY_SUCCESS ||
      ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE, NULL) != GHOSTTY_SUCCESS ||
      ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM, &disabled) != GHOSTTY_SUCCESS ||
      ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES, &apc) != GHOSTTY_SUCCESS ||
      ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES_KITTY, &apc) != GHOSTTY_SUCCESS ||
      ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &scroll_bytes) != GHOSTTY_SUCCESS ||
      ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, &scroll_lines) != GHOSTTY_SUCCESS) {
    tw_ghostty_free(value);
    return NULL;
  }
  const uint8_t blinking[] = "\033[?12h";
  ghostty_terminal_vt_write(value->terminal, blinking, sizeof(blinking)-1);
  return value;
}
bool tw_ghostty_write(TwGhostty *value, const uint8_t *data, size_t len) {
  if (!value || (!data && len) || len > 1024 * 1024) return false;
  ghostty_terminal_vt_write(value->terminal, data, len);
  return true;
}
bool tw_ghostty_resize(TwGhostty *value, uint16_t rows, uint16_t cols, uint32_t width, uint32_t height) {
  if (!value || rows < 2 || rows > 200 || cols < 2 || cols > 400 || width > 256 || height > 256) return false;
  return ghostty_terminal_resize(value->terminal, cols, rows, width, height) == GHOSTTY_SUCCESS;
}
bool tw_ghostty_scroll(TwGhostty *value, size_t rows_from_bottom) {
  if (!value || rows_from_bottom > 2000) return false;
  /* A row at today's bottom is a pinned history position. BOTTOM follows
     future output, including when the scrollback prunes old pages. */
  if (rows_from_bottom == 0) {
    GhosttyTerminalScrollViewport request = {.tag = GHOSTTY_SCROLL_VIEWPORT_BOTTOM};
    ghostty_terminal_scroll_viewport(value->terminal, request);
    return true;
  }
  GhosttyTerminalScrollbar scrollbar;
  REQUIRE(ghostty_terminal_get(value->terminal, GHOSTTY_TERMINAL_DATA_SCROLLBAR, &scrollbar));
  uint64_t bottom = scrollbar.total > scrollbar.len ? scrollbar.total - scrollbar.len : 0;
  GhosttyTerminalScrollViewport request = {.tag = GHOSTTY_SCROLL_VIEWPORT_ROW};
  request.value.row = bottom > rows_from_bottom ? (size_t)(bottom - rows_from_bottom) : 0;
  ghostty_terminal_scroll_viewport(value->terminal, request);
  return true;
}
static GhosttyPoint viewport_point(uint16_t row, uint16_t col) {
  return (GhosttyPoint){.tag = GHOSTTY_POINT_TAG_VIEWPORT,
    .value = {.coordinate = {.x = col, .y = row}}};
}
bool tw_ghostty_selection_begin(TwGhostty *value, uint16_t row, uint16_t col) {
  if (!value) return false;
  GhosttyTerminalScreen screen;
  REQUIRE(ghostty_terminal_get(value->terminal, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &screen));
  GhosttyTrackedGridRef anchor = NULL;
  REQUIRE(ghostty_terminal_grid_ref_track(value->terminal, viewport_point(row, col), &anchor));
  GhosttyGridRef first = GHOSTTY_INIT_SIZED(GhosttyGridRef);
  if (!anchor || ghostty_tracked_grid_ref_snapshot(anchor, &first) != GHOSTTY_SUCCESS) {
    if (anchor) ghostty_tracked_grid_ref_free(anchor);
    return false;
  }
  GhosttySelection selection = GHOSTTY_INIT_SIZED(GhosttySelection);
  selection.start = first;
  selection.end = first;
  if (ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_SELECTION, &selection) != GHOSTTY_SUCCESS) {
    ghostty_tracked_grid_ref_free(anchor);
    return false;
  }
  if (value->selection_anchor) ghostty_tracked_grid_ref_free(value->selection_anchor);
  value->selection_anchor = anchor;
  value->selection_screen = screen;
  return true;
}
bool tw_ghostty_selection_extend(TwGhostty *value, uint16_t row, uint16_t col) {
  if (!value || !value->selection_anchor) return false;
  GhosttyTerminalScreen screen;
  REQUIRE(ghostty_terminal_get(value->terminal, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &screen));
  if (screen != value->selection_screen) return false;
  GhosttyGridRef first = GHOSTTY_INIT_SIZED(GhosttyGridRef);
  GhosttyGridRef last = GHOSTTY_INIT_SIZED(GhosttyGridRef);
  REQUIRE(ghostty_tracked_grid_ref_snapshot(value->selection_anchor, &first));
  REQUIRE(ghostty_terminal_grid_ref(value->terminal, viewport_point(row, col), &last));
  GhosttySelection selection = GHOSTTY_INIT_SIZED(GhosttySelection);
  selection.start = first;
  selection.end = last;
  return ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_SELECTION, &selection) == GHOSTTY_SUCCESS;
}
bool tw_ghostty_selection_clear(TwGhostty *value) {
  if (!value) return false;
  if (!value->selection_anchor) return true;
  GhosttyTerminalScreen screen;
  REQUIRE(ghostty_terminal_get(value->terminal, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &screen));
  if (screen != value->selection_screen) return false;
  REQUIRE(ghostty_terminal_set(value->terminal, GHOSTTY_TERMINAL_OPT_SELECTION, NULL));
  ghostty_tracked_grid_ref_free(value->selection_anchor);
  value->selection_anchor = NULL;
  return true;
}
bool tw_ghostty_selection_copy(TwGhostty *value, uint8_t *out, size_t capacity, size_t *written) {
  if (!written) return false;
  *written = 0;
  if (!value || !value->selection_anchor || !out || capacity > 2 * 1024 * 1024) return false;
  GhosttyTerminalScreen screen;
  REQUIRE(ghostty_terminal_get(value->terminal, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &screen));
  if (screen != value->selection_screen) return false;
  GhosttyTerminalSelectionFormatOptions options = GHOSTTY_INIT_SIZED(GhosttyTerminalSelectionFormatOptions);
  options.emit = GHOSTTY_FORMATTER_FORMAT_PLAIN;
  options.unwrap = true;
  options.trim = true;
  size_t len = 0;
  if (ghostty_terminal_selection_format_buf(value->terminal, options, out, capacity, &len) != GHOSTTY_SUCCESS || len > capacity) return false;
  *written = len;
  return true;
}
size_t tw_ghostty_replies(TwGhostty *value, uint8_t *output, size_t capacity) {
  if (!value || !output || capacity < value->replies_len) return 0;
  size_t len = value->replies_len;
  memcpy(output, value->replies, len);
  value->replies_len = 0;
  return len;
}
bool tw_ghostty_animation_tick(TwGhostty *value, uint64_t now_ms, bool *playing, uint64_t *next_ms) {
  if (!value || !playing || !next_ms) return false;
  /* Writes and ticks invalidate foreign image handles. Reacquire storage on
     every call; snapshots below copy pixels after ticking, never before it. */
  GhosttyKittyGraphics graphics = NULL;
  REQUIRE(ghostty_terminal_get(value->terminal, GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS, &graphics));
  if (!graphics) { *playing = false; return true; }
  GhosttyResult result = ghostty_kitty_graphics_animation_tick(graphics, now_ms, next_ms);
  if (result != GHOSTTY_SUCCESS && result != GHOSTTY_NO_VALUE) return false;
  *playing = result == GHOSTTY_SUCCESS;
  return true;
}
static uint32_t color(GhosttyRenderStateRowCells cell, GhosttyRenderStateRowCellsData kind) {
  GhosttyColorRgb rgb;
  if (ghostty_render_state_row_cells_get(cell, kind, &rgb) != GHOSTTY_SUCCESS) return UINT32_MAX;
  return ((uint32_t)rgb.r << 16) | ((uint32_t)rgb.g << 8) | rgb.b;
}
static bool snapshot_images(TwGhostty *value, TwGhosttyInfo *info, TwGhosttyImageFn image, void *context) {
  GhosttyKittyGraphics storage;
  REQUIRE(ghostty_terminal_get(value->terminal, GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS, &storage));
  REQUIRE(ghostty_render_state_get(value->render, GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR, &value->images));
  size_t count = 0;
  while (ghostty_render_state_image_iterator_next(value->images)) {
    if (++count > 64) { info->flags |= 8; break; }
    GhosttyImageProtocol protocol;
    GhosttyRenderStateImageLayer layer;
    uint32_t id;
    TwGhosttyImage item = {0};
    GhosttyRenderStateImageData keys[] = {
      GHOSTTY_RENDER_STATE_IMAGE_DATA_PROTOCOL, GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_ID,
      GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_GENERATION, GHOSTTY_RENDER_STATE_IMAGE_DATA_LAYER,
      GHOSTTY_RENDER_STATE_IMAGE_DATA_Z, GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_COL,
      GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_ROW, GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_X,
      GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_Y, GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_WIDTH,
      GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_HEIGHT, GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_X,
      GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_Y, GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_WIDTH,
      GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_HEIGHT,
    };
    void *outputs[] = {&protocol, &id, &item.generation, &layer, &item.z, &item.col, &item.row,
      &item.offset_x, &item.offset_y, &item.width, &item.height, &item.source_x, &item.source_y,
      &item.source_width, &item.source_height};
    REQUIRE(ghostty_render_state_image_get_multi(value->images, 15, keys, outputs, NULL));
    if (protocol != GHOSTTY_IMAGE_PROTOCOL_KITTY) { info->flags |= 8; continue; }
    item.layer = (uint32_t)layer;
    GhosttyKittyGraphicsImage handle = ghostty_kitty_graphics_image(storage, id);
    if (!handle) continue;
    GhosttyKittyImageFormat format;
    REQUIRE(ghostty_kitty_graphics_image_get(handle, GHOSTTY_KITTY_IMAGE_DATA_WIDTH, &item.image_width));
    REQUIRE(ghostty_kitty_graphics_image_get(handle, GHOSTTY_KITTY_IMAGE_DATA_HEIGHT, &item.image_height));
    REQUIRE(ghostty_kitty_graphics_image_get(handle, GHOSTTY_KITTY_IMAGE_DATA_FORMAT, &format));
    item.format = (uint32_t)format;
    REQUIRE(ghostty_kitty_graphics_image_get(handle, GHOSTTY_KITTY_IMAGE_DATA_DATA_LEN, &item.len));
    GhosttyResult result = ghostty_kitty_graphics_image_get(handle, GHOSTTY_KITTY_IMAGE_DATA_DATA_PTR, &item.data);
    if (result == GHOSTTY_NO_VALUE) continue;
    if (result != GHOSTTY_SUCCESS) return false;
    if (!image(context, &item)) info->flags |= 8;
  }
  return true;
}
bool tw_ghostty_snapshot(TwGhostty *value, TwGhosttyInfo *info, TwGhosttyCellFn cell, void *context,
                         TwGhosttyImageFn image, void *image_context) {
  if (!value || !info || !cell || !image) return false;
  REQUIRE(ghostty_render_state_update(value->render, value->terminal));
  REQUIRE(ghostty_render_state_get(value->render, GHOSTTY_RENDER_STATE_DATA_ROWS, &info->rows));
  REQUIRE(ghostty_render_state_get(value->render, GHOSTTY_RENDER_STATE_DATA_COLS, &info->cols));
  if (info->rows > 200 || info->cols > 400) return false;
  GhosttyRenderStateCursor cursor = GHOSTTY_INIT_SIZED(GhosttyRenderStateCursor);
  REQUIRE(ghostty_render_state_get(value->render, GHOSTTY_RENDER_STATE_DATA_CURSOR, &cursor));
  info->cursor_row = cursor.viewport_has_value ? cursor.viewport_y : 0;
  info->cursor_col = cursor.viewport_has_value ? cursor.viewport_x : 0;
  info->flags = cursor.visible && cursor.viewport_has_value ? 0 : 1;
  GhosttyRenderStateCursorVisualStyle cursor_style;
  bool blinking = false, has_color = false;
  REQUIRE(ghostty_render_state_get(value->render, GHOSTTY_RENDER_STATE_DATA_CURSOR_VISUAL_STYLE, &cursor_style));
  REQUIRE(ghostty_render_state_get(value->render, GHOSTTY_RENDER_STATE_DATA_CURSOR_BLINKING, &blinking));
  REQUIRE(ghostty_render_state_get(value->render, GHOSTTY_RENDER_STATE_DATA_COLOR_CURSOR_HAS_VALUE, &has_color));
  info->flags |= ((uint32_t)cursor_style & 3) << 4;
  if (blinking) info->flags |= 64;
  info->cursor_color = 0xffffffff;
  if (has_color) {
    GhosttyColorRgb cursor_color;
    REQUIRE(ghostty_render_state_get(value->render, GHOSTTY_RENDER_STATE_DATA_COLOR_CURSOR, &cursor_color));
    info->cursor_color = ((uint32_t)cursor_color.r << 16) | ((uint32_t)cursor_color.g << 8) | cursor_color.b;
  }
  GhosttyTerminalModeConfig mode = {.mode = ghostty_mode_new(1, false)};
  REQUIRE(ghostty_terminal_get(value->terminal, GHOSTTY_TERMINAL_DATA_MODE, &mode));
  if (mode.value) info->flags |= 2;
  mode.mode = ghostty_mode_new(2004, false);
  REQUIRE(ghostty_terminal_get(value->terminal, GHOSTTY_TERMINAL_DATA_MODE, &mode));
  if (mode.value) info->flags |= 4;
  REQUIRE(ghostty_render_state_get(value->render, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &value->rows));
  while (ghostty_render_state_row_iterator_next(value->rows)) {
    int32_t y;
    REQUIRE(ghostty_render_state_row_get(value->rows, GHOSTTY_RENDER_STATE_ROW_DATA_VIEWPORT_Y, &y));
    if (y < 0 || y >= info->rows) return false;
    REQUIRE(ghostty_render_state_row_get(value->rows, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS, &value->cells));
    GhosttyRenderStateRowSelection selected = GHOSTTY_INIT_SIZED(GhosttyRenderStateRowSelection);
    GhosttyResult selected_result = ghostty_render_state_row_get(value->rows, GHOSTTY_RENDER_STATE_ROW_DATA_SELECTION, &selected);
    if (selected_result != GHOSTTY_SUCCESS && selected_result != GHOSTTY_NO_VALUE) return false;
    uint16_t x = 0;
    while (ghostty_render_state_row_cells_next(value->cells)) {
      if (x >= info->cols) return false;
      GhosttyCell raw;
      GhosttyCellWide wide;
      REQUIRE(ghostty_render_state_row_cells_get(value->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW, &raw));
      REQUIRE(ghostty_cell_get(raw, GHOSTTY_CELL_DATA_WIDE, &wide));
      uint8_t bytes[1024];
      GhosttyBuffer buffer = {.ptr = bytes, .cap = sizeof(bytes)};
      GhosttyResult text_result = ghostty_render_state_row_cells_get(value->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8, &buffer);
      if (text_result == GHOSTTY_OUT_OF_SPACE) {
        memcpy(bytes, "\xef\xbf\xbd", 3); buffer.len = 3;
      } else if (text_result != GHOSTTY_SUCCESS) return false;
      bool has_grapheme = buffer.len != 0;
      if (buffer.len == 0) { bytes[0] = ' '; buffer.len = 1; }
      GhosttyStyle style = GHOSTTY_INIT_SIZED(GhosttyStyle);
      REQUIRE(ghostty_render_state_row_cells_get(value->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &style));
      uint32_t flags = (wide == GHOSTTY_CELL_WIDE_WIDE ? 1 : 0) |
          (wide == GHOSTTY_CELL_WIDE_SPACER_TAIL || wide == GHOSTTY_CELL_WIDE_SPACER_HEAD ? 2 : 0) |
          (style.bold ? 4 : 0) | (style.italic ? 8 : 0) | (style.underline ? 16 : 0) | (style.inverse ? 32 : 0) |
          (has_grapheme ? 64 : 0) |
          (selected_result == GHOSTTY_SUCCESS && x >= selected.start_x && x <= selected.end_x ? 128 : 0);
      if (!cell(context, (uint16_t)y, x, bytes, buffer.len,
          color(value->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR),
          color(value->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR), flags)) return false;
      x++;
    }
  }
  if (!snapshot_images(value, info, image, image_context)) return false;
  REQUIRE(ghostty_render_state_clean(value->render));
  return true;
}
