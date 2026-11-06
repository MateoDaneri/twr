/* Synthetic VT bytes only. No PTY, clipboard callback, PNG decoder or network. */
#include <ghostty/vt.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct { char bytes[16384]; size_t len; } Buffer;
static int failures;
static void require(GhosttyResult result) {
  if (result != GHOSTTY_SUCCESS) {
    fprintf(stderr, "C API returned %d\n", result);
    exit(2);
  }
}
static void check(const char *name, bool passed) {
  printf("{\"check\":\"%s\",\"passed\":%s}\n", name, passed ? "true" : "false");
  if (!passed) failures++;
}
static bool append(void *context, const uint8_t *bytes, size_t len) {
  Buffer *buffer = context;
  if (len >= sizeof(buffer->bytes) - buffer->len) return false;
  memcpy(buffer->bytes + buffer->len, bytes, len);
  buffer->len += len;
  buffer->bytes[buffer->len] = 0;
  return true;
}
static void reply(GhosttyTerminal terminal, void *context, const uint8_t *bytes, size_t len) {
  (void)terminal;
  if (!append(context, bytes, len)) exit(3);
}
static void write_vt(GhosttyTerminal terminal, const char *text) {
  ghostty_terminal_vt_write(terminal, (const uint8_t *)text, strlen(text));
}
static Buffer text(GhosttyTerminal terminal, bool unwrap) {
  Buffer buffer = {0};
  GhosttyFormatterTerminalOptions options = GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalOptions);
  options.emit = GHOSTTY_FORMATTER_FORMAT_PLAIN;
  options.trim = true;
  options.unwrap = unwrap;
  GhosttyFormatter formatter = NULL;
  require(ghostty_formatter_terminal_new(NULL, &formatter, terminal, options));
  GhosttyWriter writer = {.write = append, .userdata = &buffer};
  require(ghostty_formatter_format(formatter, writer));
  ghostty_formatter_free(formatter);
  return buffer;
}
static uint16_t coordinate(GhosttyTerminal terminal, GhosttyTerminalData field) {
  uint16_t value = 0;
  require(ghostty_terminal_get(terminal, field, &value));
  return value;
}
static bool mode(GhosttyTerminal terminal, uint16_t number) {
  GhosttyTerminalModeConfig value = {.mode = ghostty_mode_new(number, false)};
  require(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_MODE, &value));
  return value.value;
}
static GhosttyKittyGraphics graphics(GhosttyTerminal terminal) {
  GhosttyKittyGraphics value = NULL;
  require(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS, &value));
  return value;
}
static void large_rgba(GhosttyTerminal terminal, unsigned id) {
  /* 4096 zero RGBA bytes: a valid 1x1024 direct image, 5464 base64 bytes. */
  char command[6000];
  int prefix = snprintf(command, sizeof(command), "\033_Ga=t,f=32,s=1,v=1024,i=%u;", id);
  if (prefix < 0 || prefix > 100) exit(2);
  memset(command + prefix, 'A', 5462);
  memcpy(command + prefix + 5462, "==\033\\", 5);
  write_vt(terminal, command);
}

int main(int argc, char **argv) {
  if (argc != 3 || strlen(argv[1]) > 1024 || strlen(argv[2]) > 1024) return 2;
  GhosttyTerminal terminal = NULL;
  require(ghostty_terminal_new(NULL, &terminal, 8, 8));
  write_vt(terminal, "abcdefghijklmno");
  Buffer wrapped = text(terminal, false), logical = text(terminal, true);
  check("soft_wrap_initial", strcmp(wrapped.bytes, "abcdefgh\nijklmno") == 0);
  check("soft_wrap_logical_text", strcmp(logical.bytes, "abcdefghijklmno") == 0);
  require(ghostty_terminal_resize(terminal, 20, 8, 9, 18));
  logical = text(terminal, true);
  check("reflow_widen_text", strcmp(logical.bytes, "abcdefghijklmno") == 0);
  check("reflow_widen_cursor", coordinate(terminal, GHOSTTY_TERMINAL_DATA_CURSOR_X) == 15 && coordinate(terminal, GHOSTTY_TERMINAL_DATA_CURSOR_Y) == 0);
  require(ghostty_terminal_resize(terminal, 5, 8, 9, 18));
  wrapped = text(terminal, false); logical = text(terminal, true);
  check("reflow_narrow_rows", strcmp(wrapped.bytes, "abcde\nfghij\nklmno") == 0);
  check("reflow_narrow_text", strcmp(logical.bytes, "abcdefghijklmno") == 0);
  ghostty_terminal_free(terminal);

  require(ghostty_terminal_new(NULL, &terminal, 20, 8));
  const char *unicode = "ñ界e\xcc\x81😀";
  write_vt(terminal, unicode);
  logical = text(terminal, true);
  check("unicode_preserved", strcmp(logical.bytes, unicode) == 0);
  check("unicode_cell_width", coordinate(terminal, GHOSTTY_TERMINAL_DATA_CURSOR_X) == 6);
  write_vt(terminal, "\033[?2004h\033[?1h");
  check("bracketed_paste_mode", mode(terminal, 2004));
  check("application_cursor_mode", mode(terminal, 1));
  write_vt(terminal, "\033[?1049hALT");
  logical = text(terminal, true);
  GhosttyTerminalScreen active_screen;
  require(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &active_screen));
  check("alternate_screen", active_screen == GHOSTTY_TERMINAL_SCREEN_ALTERNATE && strcmp(logical.bytes, "      ALT") == 0);
  write_vt(terminal, "\033[?1049l");
  logical = text(terminal, true);
  check("primary_screen_restored", strcmp(logical.bytes, unicode) == 0);
  Buffer replies = {0};
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_USERDATA, &replies));
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_WRITE_PTY, (const void *)reply));
  write_vt(terminal, "\033[6n");
  check("cursor_report", strcmp(replies.bytes, "\033[1;7R") == 0);
  ghostty_terminal_free(terminal);

  require(ghostty_terminal_new(NULL, &terminal, 20, 8));
  replies = (Buffer){0};
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_USERDATA, &replies));
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_WRITE_PTY, (const void *)reply));
  bool disabled = false;
  uint64_t storage_limit = 65536;
  size_t apc_limit = 4096;
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, &storage_limit));
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE, &disabled));
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE, NULL));
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM, &disabled));
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES, &apc_limit));
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES_KITTY, &apc_limit));
  bool file_enabled = true, shared_enabled = true;
  GhosttyString temp_dir = {0};
  require(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_MEDIUM_FILE, &file_enabled));
  require(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_MEDIUM_SHARED_MEM, &shared_enabled));
  require(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_MEDIUM_TEMP_FILE, &temp_dir));
  check("host_image_transports_disabled", !file_enabled && !shared_enabled && temp_dir.len == 0);
  write_vt(terminal, "\033_Ga=T,f=32,s=1,v=1,i=7;/wAA/w==\033\\");
  GhosttyKittyGraphicsImage image = ghostty_kitty_graphics_image(graphics(terminal), 7);
  check("kitty_raw_rgba_image", image != NULL);
  if (image) {
    uint32_t width = 0, height = 0; size_t length = 0; const uint8_t *pixels = NULL;
    require(ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_IMAGE_DATA_WIDTH, &width));
    require(ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_IMAGE_DATA_HEIGHT, &height));
    require(ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_IMAGE_DATA_DATA_LEN, &length));
    require(ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_IMAGE_DATA_DATA_PTR, &pixels));
    const uint8_t expected[] = {255, 0, 0, 255};
    check("kitty_raw_rgba_pixels", width == 1 && height == 1 && length == 4 && memcmp(pixels, expected, 4) == 0);
    GhosttyKittyGraphicsPlacementIterator iterator = NULL;
    require(ghostty_kitty_graphics_placement_iterator_new(NULL, &iterator));
    require(ghostty_kitty_graphics_get(graphics(terminal), GHOSTTY_KITTY_GRAPHICS_DATA_PLACEMENT_ITERATOR, &iterator));
    check("kitty_image_placement", ghostty_kitty_graphics_placement_next(iterator));
    ghostty_kitty_graphics_placement_iterator_free(iterator);
  }
  const char media[] = {'f', 't', 's'};
  const char *names[] = {"kitty_file_rejected", "kitty_temp_file_rejected", "kitty_shared_memory_rejected"};
  for (size_t i = 0; i < sizeof(media); i++) {
    char command[2048]; replies = (Buffer){0};
    snprintf(command, sizeof(command), "\033_Ga=t,t=%c,f=32,s=1,v=1,i=%u;%s\033\\", media[i], (unsigned)(8 + i), i == 2 ? argv[2] : argv[1]);
    write_vt(terminal, command);
    check(names[i], strstr(replies.bytes, "unsupported medium") != NULL && ghostty_kitty_graphics_image(graphics(terminal), (uint32_t)(8 + i)) == NULL);
  }
  write_vt(terminal, "\033_Ga=t,f=32,s=4000000000,v=4000000000,i=11;AAAA\033\\");
  check("kitty_oversized_dimensions_rejected", ghostty_kitty_graphics_image(graphics(terminal), 11) == NULL);
  large_rgba(terminal, 12);
  check("kitty_apc_budget_rejects_valid_large_payload", ghostty_kitty_graphics_image(graphics(terminal), 12) == NULL);
  write_vt(terminal, "AFTER");
  logical = text(terminal, true);
  check("parser_recovers_after_apc_budget", strstr(logical.bytes, "AFTER") != NULL);
  apc_limit = 32768;
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES, &apc_limit));
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES_KITTY, &apc_limit));
  large_rgba(terminal, 12);
  check("kitty_same_payload_positive_control", ghostty_kitty_graphics_image(graphics(terminal), 12) != NULL);
  storage_limit = 1024;
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, &storage_limit));
  large_rgba(terminal, 13);
  check("kitty_storage_budget_rejects_valid_payload", ghostty_kitty_graphics_image(graphics(terminal), 13) == NULL);
  storage_limit = 0;
  require(ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, &storage_limit));
  check("kitty_storage_disable_clears_images", ghostty_kitty_graphics_image(graphics(terminal), 7) == NULL);
  ghostty_terminal_free(terminal);
  return failures ? 1 : 0;
}
