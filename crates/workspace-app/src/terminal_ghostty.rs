//! Owned, single-thread C-core adapter. Foreign cell bytes never outlive a callback.
use crate::terminal::{Cell, Snapshot};
use crate::terminal_image::{self, ImageKey, Pixels, Placement};
use std::{
    collections::HashMap,
    ffi::c_void,
    marker::PhantomData,
    ptr::NonNull,
    rc::Rc,
    sync::{Arc, OnceLock},
    time::Instant,
};

#[repr(C)]
#[derive(Default)]
struct Info {
    rows: u16,
    cols: u16,
    cursor_row: u16,
    cursor_col: u16,
    flags: u32,
    cursor_color: u32,
}
type CellCallback =
    unsafe extern "C" fn(*mut c_void, u16, u16, *const u8, usize, u32, u32, u32) -> bool;
#[repr(C)]
struct RawImage {
    generation: u64,
    col: i32,
    row: i32,
    z: i32,
    layer: u32,
    offset_x: u32,
    offset_y: u32,
    width: u32,
    height: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    image_width: u32,
    image_height: u32,
    format: u32,
    data: *const u8,
    len: usize,
}
type ImageCallback = unsafe extern "C" fn(*mut c_void, *const RawImage) -> bool;
#[repr(C)]
struct DecodedPng {
    width: u32,
    height: u32,
    data: *mut u8,
    len: usize,
}
type PngCallback = unsafe extern "C" fn(*const c_void, *const u8, usize, *mut DecodedPng) -> bool;
unsafe extern "C" {
    fn tw_ghostty_set_png_decoder(decoder: PngCallback) -> bool;
    fn tw_ghostty_allocate_png(allocator: *const c_void, len: usize) -> *mut u8;
    fn tw_ghostty_new(rows: u16, cols: u16) -> *mut c_void;
    fn tw_ghostty_free(value: *mut c_void);
    fn tw_ghostty_write(value: *mut c_void, data: *const u8, len: usize) -> bool;
    fn tw_ghostty_resize(value: *mut c_void, rows: u16, cols: u16, width: u32, height: u32)
    -> bool;
    fn tw_ghostty_scroll(value: *mut c_void, rows_from_bottom: usize) -> bool;
    fn tw_ghostty_selection_begin(value: *mut c_void, row: u16, col: u16) -> bool;
    fn tw_ghostty_selection_extend(value: *mut c_void, row: u16, col: u16) -> bool;
    fn tw_ghostty_selection_clear(value: *mut c_void) -> bool;
    fn tw_ghostty_selection_copy(
        value: *mut c_void,
        out: *mut u8,
        capacity: usize,
        written: *mut usize,
    ) -> bool;
    fn tw_ghostty_replies(value: *mut c_void, output: *mut u8, capacity: usize) -> usize;
    fn tw_ghostty_animation_tick(
        value: *mut c_void,
        now_ms: u64,
        playing: *mut bool,
        next_ms: *mut u64,
    ) -> bool;
    fn tw_ghostty_snapshot(
        value: *mut c_void,
        info: *mut Info,
        cell: CellCallback,
        context: *mut c_void,
        image: ImageCallback,
        image_context: *mut c_void,
    ) -> bool;
}

pub struct GhosttyParser {
    raw: NonNull<c_void>,
    size: (u16, u16),
    cell_size: (u32, u32),
    image_cache: HashMap<ImageKey, Arc<Pixels>>,
    animation_ms: u64,
    // Never share or move an initialized handle between threads. Construct in its worker.
    _single_thread: PhantomData<Rc<()>>,
}
impl GhosttyParser {
    pub fn new(rows: u16, cols: u16) -> Result<Self, String> {
        if !(2..=200).contains(&rows) || !(2..=400).contains(&cols) {
            return Err("Terminal dimensions exceed adapter limits".into());
        }
        static PNG: OnceLock<Result<(), String>> = OnceLock::new();
        PNG.get_or_init(|| {
            // SAFETY: OnceLock installs one stateless callback before handles
            // exist. All later workers observe the completed initialization.
            if unsafe { tw_ghostty_set_png_decoder(decode_png_callback) } {
                Ok(())
            } else {
                Err("Could not initialize bounded PNG decoder".into())
            }
        })
        .clone()?;
        // SAFETY: C validates dimensions and returns a new owned handle or NULL.
        let raw = NonNull::new(unsafe { tw_ghostty_new(rows, cols) })
            .ok_or("Could not initialize pinned terminal core")?;
        Ok(Self {
            raw,
            size: (rows, cols),
            cell_size: (0, 0),
            image_cache: HashMap::new(),
            animation_ms: 0,
            _single_thread: PhantomData,
        })
    }
    pub fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > 1024 * 1024 {
            return Err("Terminal chunk exceeds 1 MiB".into());
        }
        // SAFETY: the slice is alive for this synchronous call; self uniquely owns the handle.
        if unsafe { tw_ghostty_write(self.raw.as_ptr(), bytes.as_ptr(), bytes.len()) } {
            Ok(())
        } else {
            Err("Terminal core rejected output".into())
        }
    }
    pub fn resize(
        &mut self,
        rows: u16,
        cols: u16,
        cell_width: u32,
        cell_height: u32,
    ) -> Result<(), String> {
        if !(2..=200).contains(&rows)
            || !(2..=400).contains(&cols)
            || cell_width > 256
            || cell_height > 256
        {
            return Err("Terminal geometry exceeds adapter limits".into());
        }
        // SAFETY: unique live handle and validated scalar arguments.
        if !unsafe { tw_ghostty_resize(self.raw.as_ptr(), rows, cols, cell_width, cell_height) } {
            return Err("Terminal core resize failed".into());
        }
        self.size = (rows, cols);
        self.cell_size = (cell_width, cell_height);
        Ok(())
    }
    pub fn scroll(&mut self, rows_from_bottom: usize) -> Result<(), String> {
        if rows_from_bottom > 2000 {
            return Err("Scroll exceeds adapter limit".into());
        }
        // SAFETY: unique live handle, no borrowed snapshot retained by C.
        if unsafe { tw_ghostty_scroll(self.raw.as_ptr(), rows_from_bottom) } {
            Ok(())
        } else {
            Err("Terminal core scroll failed".into())
        }
    }
    pub(crate) fn selection_begin(&mut self, row: u16, col: u16) -> Result<(), String> {
        if row >= self.size.0 || col >= self.size.1 {
            return Err("Selection is outside the terminal viewport".into());
        }
        // SAFETY: the parser and its selection are owned by this worker alone.
        if unsafe { tw_ghostty_selection_begin(self.raw.as_ptr(), row, col) } {
            Ok(())
        } else {
            Err("Terminal selection could not be started".into())
        }
    }
    pub(crate) fn selection_extend(&mut self, row: u16, col: u16) -> Result<(), String> {
        if row >= self.size.0 || col >= self.size.1 {
            return Err("Selection is outside the terminal viewport".into());
        }
        // SAFETY: the parser and its selection are owned by this worker alone.
        if unsafe { tw_ghostty_selection_extend(self.raw.as_ptr(), row, col) } {
            Ok(())
        } else {
            Err("Terminal selection could not be extended".into())
        }
    }
    pub(crate) fn selection_clear(&mut self) -> Result<(), String> {
        // SAFETY: the parser and its selection are owned by this worker alone.
        if unsafe { tw_ghostty_selection_clear(self.raw.as_ptr()) } {
            Ok(())
        } else {
            Err("Terminal selection could not be cleared".into())
        }
    }
    pub(crate) fn selection_copy(&mut self) -> Result<String, String> {
        let mut bytes = vec![0; 2 * 1024 * 1024];
        let mut written = 0;
        // SAFETY: C receives an exclusive bounded buffer and cannot retain it.
        if !unsafe {
            tw_ghostty_selection_copy(
                self.raw.as_ptr(),
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut written,
            )
        } || written > bytes.len()
        {
            return Err("Terminal selection could not be copied".into());
        }
        bytes.truncate(written);
        String::from_utf8(bytes).map_err(|_| "Terminal selection is not valid UTF-8".into())
    }
    pub fn replies(&mut self) -> Vec<u8> {
        let mut bytes = vec![0; 4096];
        // SAFETY: C writes at most capacity and owns no reference after returning.
        let len = unsafe { tw_ghostty_replies(self.raw.as_ptr(), bytes.as_mut_ptr(), bytes.len()) };
        bytes.truncate(len.min(bytes.len()));
        bytes
    }
    pub fn animation_tick(&mut self, now_ms: u64) -> Result<Option<u64>, String> {
        if now_ms < self.animation_ms {
            return Err("Animation clock must be monotonic".into());
        }
        let (mut playing, mut next_ms) = (false, 0);
        // SAFETY: unique worker-owned handle; no foreign image pointer survives
        // a snapshot callback. The bridge reacquires storage after every write.
        if !unsafe {
            tw_ghostty_animation_tick(self.raw.as_ptr(), now_ms, &mut playing, &mut next_ms)
        } {
            return Err("Terminal animation tick failed".into());
        }
        self.animation_ms = now_ms;
        Ok(playing.then_some(next_ms))
    }
    pub fn snapshot(
        &mut self,
        revision: u64,
        exited: bool,
        error: Option<String>,
    ) -> Result<Snapshot, String> {
        let (rows, cols) = self.size;
        let empty = Cell {
            text: " ".into(),
            has_grapheme: false,
            fg: vt100::Color::Default,
            bg: vt100::Color::Default,
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
            wide_continuation: false,
            wide: false,
            selected: false,
        };
        let mut cells = vec![vec![empty; cols as usize]; rows as usize];
        let mut info = Info::default();
        let mut images = ImageCollector {
            previous: &self.image_cache,
            cache: HashMap::new(),
            placements: Vec::new(),
            bytes: 0,
            cell_size: self.cell_size,
        };
        // SAFETY: all outputs and context remain live until synchronous C iteration ends.
        let ok = unsafe {
            tw_ghostty_snapshot(
                self.raw.as_ptr(),
                &mut info,
                collect_cell,
                &mut cells as *mut _ as *mut c_void,
                collect_image,
                &mut images as *mut _ as *mut c_void,
            )
        };
        if !ok || (info.rows, info.cols) != self.size || info.flags & !127 != 0 {
            return Err("Terminal core returned an invalid frame".into());
        }
        images.placements.sort_by_key(|p| (p.layer, p.z));
        let placements = images.placements;
        self.image_cache = images.cache;
        Ok(Snapshot {
            revision,
            content_revision: revision,
            selection_token: None,
            published_at: Instant::now(),
            rows,
            cols,
            cells,
            cursor: (info.cursor_row.min(rows - 1), info.cursor_col.min(cols - 1)),
            hide_cursor: info.flags & 1 != 0,
            cursor_style: ((info.flags >> 4) & 3) as u8,
            cursor_blink: info.flags & 64 != 0,
            cursor_color: (info.cursor_color <= 0xffffff).then_some(info.cursor_color),
            application_cursor: info.flags & 2 != 0,
            bracketed_paste: info.flags & 4 != 0,
            exited,
            error,
            images: placements,
            images_limited: info.flags & 8 != 0,
        })
    }
}
unsafe extern "C" fn decode_png_callback(
    allocator: *const c_void,
    data: *const u8,
    len: usize,
    out: *mut DecodedPng,
) -> bool {
    if allocator.is_null() || data.is_null() || out.is_null() || len > 4 * 1024 * 1024 {
        return false;
    }
    std::panic::catch_unwind(|| {
        // SAFETY: the pinned core supplies the complete PNG slice for this
        // synchronous callback. It never survives the call.
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        let Some(decoded) = terminal_image::decode_png(bytes) else {
            return false;
        };
        let (width, height) = decoded.dimensions();
        let bytes = decoded.as_raw();
        // SAFETY: allocation uses the exact allocator supplied by the core.
        // On success the core owns/frees it, including subsequent rejection.
        let target = unsafe { tw_ghostty_allocate_png(allocator, bytes.len()) };
        if target.is_null() {
            return false;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
            *out = DecodedPng {
                width,
                height,
                data: target,
                len: bytes.len(),
            };
        }
        true
    })
    .unwrap_or(false)
}

struct ImageCollector<'a> {
    previous: &'a HashMap<ImageKey, Arc<Pixels>>,
    cache: HashMap<ImageKey, Arc<Pixels>>,
    placements: Vec<Placement>,
    bytes: usize,
    cell_size: (u32, u32),
}
unsafe extern "C" fn collect_image(context: *mut c_void, raw: *const RawImage) -> bool {
    if context.is_null() || raw.is_null() {
        return false;
    }
    // SAFETY: snapshot supplies this live collector; the bridge's stack record is
    // borrowed only for this callback, and all scalar lengths are checked below.
    let frame = unsafe { &mut *context.cast::<ImageCollector<'_>>() };
    let item = unsafe { &*raw };
    let (cw, ch) = frame.cell_size;
    if item.data.is_null()
        || item.generation == 0
        || item.layer > 2
        || cw == 0
        || ch == 0
        || item.width == 0
        || item.height == 0
        || item.width > 200_000
        || item.height > 100_000
        || item.offset_x > 4096
        || item.offset_y > 4096
        || item.col.unsigned_abs() > 32768
        || item.row.unsigned_abs() > 32768
        || frame.placements.len() >= terminal_image::MAX_PLACEMENTS
    {
        return false;
    }
    let Some(crop) = terminal_image::render_crop(
        item.image_width,
        item.image_height,
        [
            item.source_x,
            item.source_y,
            item.source_width,
            item.source_height,
        ],
    ) else {
        return false;
    };
    let Some((_, crop)) = terminal_image::layout(
        item.image_width,
        item.image_height,
        item.format,
        item.len,
        crop,
    ) else {
        return false;
    };
    let key = ImageKey {
        generation: item.generation,
        crop,
    };
    let pixels = if let Some(pixels) = frame.cache.get(&key) {
        pixels.clone()
    } else {
        let bytes = crop[2] as usize * crop[3] as usize * 4;
        if frame.bytes + bytes > terminal_image::MAX_FRAME_BYTES {
            return false;
        }
        let pixels = if let Some(pixels) = frame.previous.get(&key) {
            pixels.clone()
        } else {
            // SAFETY: the pinned core provides this decoded byte range; layout
            // validated its exact pixel length and 8 MiB bound before this read.
            let bytes = unsafe { std::slice::from_raw_parts(item.data, item.len) };
            let Some(pixels) = terminal_image::crop_bgra(
                key,
                item.image_width,
                item.image_height,
                item.format,
                bytes,
            ) else {
                return false;
            };
            Arc::new(pixels)
        };
        frame.bytes += bytes;
        frame.cache.insert(key, pixels.clone());
        pixels
    };
    frame.placements.push(Placement {
        pixels,
        col: item.col as f32 + item.offset_x as f32 / cw as f32,
        row: item.row as f32 + item.offset_y as f32 / ch as f32,
        width: item.width as f32 / cw as f32,
        height: item.height as f32 / ch as f32,
        layer: item.layer,
        z: item.z,
    });
    true
}
impl Drop for GhosttyParser {
    fn drop(&mut self) {
        // SAFETY: this handle has exactly one owner; no callback or borrowed frame survives a call.
        unsafe { tw_ghostty_free(self.raw.as_ptr()) }
    }
}
unsafe extern "C" fn collect_cell(
    context: *mut c_void,
    row: u16,
    col: u16,
    data: *const u8,
    len: usize,
    fg: u32,
    bg: u32,
    flags: u32,
) -> bool {
    if context.is_null() || data.is_null() || len > 1024 || flags & !255 != 0 {
        return false;
    }
    // SAFETY: snapshot supplies this live Vec and the pinned bridge supplies len readable bytes.
    let cells = unsafe { &mut *context.cast::<Vec<Vec<Cell>>>() };
    let Some(cell) = cells
        .get_mut(row as usize)
        .and_then(|r| r.get_mut(col as usize))
    else {
        return false;
    };
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    // Kitty placeholder cells carry image coordinates in combining marks;
    // their rectangles are painted separately, never as a missing-font glyph.
    let text = if text.starts_with('\u{10eeee}') {
        " "
    } else {
        text
    };
    let color = |value| {
        if value == u32::MAX {
            vt100::Color::Default
        } else {
            vt100::Color::Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
        }
    };
    *cell = Cell {
        text: text.into(),
        has_grapheme: flags & 64 != 0,
        fg: color(fg),
        bg: color(bg),
        bold: flags & 4 != 0,
        italic: flags & 8 != 0,
        underline: flags & 16 != 0,
        inverse: flags & 32 != 0,
        wide: flags & 1 != 0,
        wide_continuation: flags & 2 != 0,
        selected: flags & 128 != 0,
    };
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn cursor_style_blink_and_osc_color_follow_the_program() {
        let mut p = super::GhosttyParser::new(24, 80).unwrap();
        p.process(b"\x1b[5 q").unwrap();
        let frame = p.snapshot(1, false, None).unwrap();
        assert_eq!(frame.cursor_style, 0);
        assert!(frame.cursor_blink);
        p.process(b"\x1b[6 q\x1b]12;#123456\x1b\\").unwrap();
        let frame = p.snapshot(2, false, None).unwrap();
        assert_eq!(frame.cursor_style, 0);
        assert!(!frame.cursor_blink);
        assert_eq!(frame.cursor_color, Some(0x123456));
        p.process(b"\x1b[2 q").unwrap();
        assert_eq!(p.snapshot(3, false, None).unwrap().cursor_style, 1);
    }

    use super::*;
    #[test]
    fn animation_clock_advances_owned_pixels_generation_and_stops_scheduling() {
        let mut p = GhosttyParser::new(24, 80).unwrap();
        p.resize(24, 80, 9, 18).unwrap();
        assert_eq!(p.animation_tick(0).unwrap(), None);
        p.process(b"\x1b_Ga=T,f=32,i=2,s=1,v=1,c=8,r=6,q=2;/wAA/w==\x1b\\\x1b_Ga=f,f=32,i=2,s=1,v=1,z=40,q=2;AAD//w==\x1b\\\x1b_Ga=a,i=2,r=1,z=30,q=2\x1b\\\x1b_Ga=a,i=2,s=3,q=2\x1b\\").unwrap();
        assert_eq!(p.animation_tick(0).unwrap(), Some(30));
        let red = p.snapshot(1, false, None).unwrap();
        assert_eq!(red.images[0].pixels.bgra, [0, 0, 255, 255]);
        assert_eq!(p.animation_tick(29).unwrap(), Some(30));
        let unchanged = p.snapshot(2, false, None).unwrap();
        assert!(Arc::ptr_eq(
            &red.images[0].pixels,
            &unchanged.images[0].pixels
        ));
        assert_eq!(p.animation_tick(30).unwrap(), Some(70));
        let blue = p.snapshot(3, false, None).unwrap();
        assert_eq!(blue.images[0].pixels.bgra, [255, 0, 0, 255]);
        assert_ne!(red.images[0].pixels.key, blue.images[0].pixels.key);
        assert_eq!(red.images[0].pixels.bgra, [0, 0, 255, 255]);
        assert_eq!(p.animation_tick(70).unwrap(), Some(100));
        let looped = p.snapshot(4, false, None).unwrap();
        assert_eq!(looped.images[0].pixels.bgra, [0, 0, 255, 255]);
        assert_ne!(looped.images[0].pixels.key, blue.images[0].pixels.key);
        assert!(p.animation_tick(69).is_err());
        p.process(b"\x1b_Ga=a,i=2,s=1,q=2\x1b\\").unwrap();
        assert_eq!(p.animation_tick(75).unwrap(), None);
        assert_eq!(p.animation_tick(500).unwrap(), None);
        let paused = p.snapshot(5, false, None).unwrap();
        assert!(Arc::ptr_eq(
            &looped.images[0].pixels,
            &paused.images[0].pixels
        ));
        p.process(b"\x1b_Ga=d,d=I,i=2,q=2\x1b\\after-animation")
            .unwrap();
        assert_eq!(p.animation_tick(501).unwrap(), None);
        let deleted = p.snapshot(6, false, None).unwrap();
        assert!(deleted.images.is_empty());
        assert!(deleted.text().contains("after-animation"));
    }
    #[test]
    fn relative_image_follows_a_virtual_text_anchor_and_disappears_with_it() {
        let mut p = GhosttyParser::new(16, 40).unwrap();
        p.resize(16, 40, 9, 18).unwrap();
        p.process(
            concat!(
                "\x1b_Ga=T,U=1,f=32,s=1,v=1,i=200,p=1,c=1,r=1,q=2;AAAAAA==\x1b\\",
                "\x1b[2;1H\x1b[38;5;200m\u{10eeee}\u{305}\u{305}\x1b[0m",
                "\x1b_Ga=T,f=32,s=1,v=1,i=7,p=1,c=8,r=6,z=0,P=200,Q=1,H=1,V=1,q=2;/wAA/w==\x1b\\"
            )
            .as_bytes(),
        )
        .unwrap();
        let frame = p.snapshot(1, false, None).unwrap();
        let child = frame.images.iter().find(|image| image.layer == 2).unwrap();
        assert_eq!(
            (child.col, child.row, child.width, child.height),
            (1., 2., 8., 6.)
        );
        p.process(b"\x1b[2;1H\x1b[2K").unwrap();
        assert!(p.snapshot(2, false, None).unwrap().images.is_empty());
    }
    #[test]
    fn malformed_png_is_rejected_and_text_after_it_is_preserved() {
        let mut p = GhosttyParser::new(8, 20).unwrap();
        p.resize(8, 20, 9, 18).unwrap();
        p.process(b"\x1b_Ga=T,f=100,i=21;aGVsbG8=\x1b\\AFTER-BAD-PNG")
            .unwrap();
        let response = String::from_utf8(p.replies()).unwrap();
        assert!(!response.is_empty());
        assert!(!response.contains("OK"));
        let frame = p.snapshot(1, false, None).unwrap();
        assert!(frame.images.is_empty());
        assert!(frame.text().contains("AFTER-BAD-PNG"));
    }
    #[test]
    fn kitty_unicode_placeholders_use_image_rectangles_without_text_glyphs() {
        let mut p = GhosttyParser::new(8, 20).unwrap();
        p.resize(8, 20, 9, 18).unwrap();
        p.process(
            concat!(
                "\x1b_Ga=T,U=1,f=32,i=3,s=1,v=1,c=2,r=1,q=2;AP8A/w==\x1b\\",
                "\r\n\x1b[38;5;3m",
                "\u{10eeee}\u{305}\u{305}",
                "\u{10eeee}\u{305}\u{30d}",
                "\x1b[39m"
            )
            .as_bytes(),
        )
        .unwrap();
        let frame = p.snapshot(1, false, None).unwrap();
        assert!(!frame.images.is_empty());
        assert!(!frame.images_limited);
        assert_eq!(frame.images.iter().map(|p| p.width).sum::<f32>(), 2.);
        assert!(frame.images.iter().all(|p| p.layer == 1 && p.row == 1.));
        assert!(!frame.text().contains('\u{10eeee}'));
        p.process(b"\x1b[2;1H\x1b[2K").unwrap();
        assert!(p.snapshot(2, false, None).unwrap().images.is_empty());
    }
    #[test]
    fn small_unicode_raster_preserves_letterbox_crops_and_following_text() {
        let mut p = GhosttyParser::new(24, 80).unwrap();
        p.resize(24, 80, 8, 21).unwrap();
        // 16x8 RGB expanded over24x8 cells at the actual native cell geometry.
        let command = format!(
            "\x1b_Ga=T,U=1,f=24,i=42,s=16,v=8,c=24,r=8,q=2;{}\x1b\\",
            "/wAA".repeat(112) + &"AAD/".repeat(16)
        );
        p.process(command.as_bytes()).unwrap();
        let marks = [
            0x305, 0x30d, 0x30e, 0x310, 0x312, 0x33d, 0x33e, 0x33f, 0x346, 0x34a, 0x34b, 0x34c,
            0x350, 0x351, 0x352, 0x357, 0x35b, 0x363, 0x364, 0x365, 0x366, 0x367, 0x368, 0x369,
        ];
        for row in 0..8 {
            let mut cells = format!("\x1b[{};1H\x1b[38;5;42m", row + 3);
            for col in 0..24 {
                cells.extend([
                    '\u{10eeee}',
                    char::from_u32(marks[row]).unwrap(),
                    char::from_u32(marks[col]).unwrap(),
                ]);
            }
            p.process(cells.as_bytes()).unwrap();
        }
        p.process(b"\x1b[0m\x1b[15;1HAFTER-SMALL-RASTER").unwrap();
        let frame = p.snapshot(1, false, None).unwrap();
        assert_eq!(frame.images.len(), 6);
        assert!(!frame.images_limited);
        let last = frame.images.last().unwrap();
        assert_eq!(last.pixels.key.crop, [0, 7, 16, 1]);
        assert_eq!(last.pixels.bgra[..4], [255, 0, 0, 255]);
        assert!(frame.text().contains("AFTER-SMALL-RASTER"));
    }
    #[test]
    fn kitty_pixels_crops_layers_replacement_and_deletion_reach_owned_frames() {
        let mut p = GhosttyParser::new(8, 20).unwrap();
        p.resize(8, 20, 9, 18).unwrap();
        p.process(b"\x1b_Ga=t,f=32,s=2,v=2,i=7;/wAA/wD/AIAAAP9A//8A/w==\x1b\\")
            .unwrap();
        p.process(b"\x1b_Ga=p,i=7,p=1,c=2,r=2,z=-1\x1b\\").unwrap();
        p.process(b"\x1b[1;1H\x1b_Ga=p,i=7,p=2,x=1,y=0,w=1,h=2,c=3,r=2,z=1\x1b\\")
            .unwrap();
        p.process(b"\x1b[1;1H\x1b_Ga=p,i=7,p=3,c=2,r=2,z=-2147483648\x1b\\")
            .unwrap();
        let frame = p.snapshot(1, false, None).unwrap();
        assert!(!frame.images_limited);
        assert_eq!(frame.images.len(), 3);
        assert_eq!(
            frame
                .images
                .iter()
                .map(|p| (p.layer, p.z))
                .collect::<Vec<_>>(),
            [(0, i32::MIN), (1, -1), (2, 1)]
        );
        let crop = &frame.images[2];
        assert_eq!(
            (crop.col, crop.row, crop.width, crop.height),
            (0., 0., 3., 2.)
        );
        assert_eq!((crop.pixels.width, crop.pixels.height), (1, 2));
        assert_eq!(crop.pixels.bgra, [0, 255, 0, 128, 0, 255, 255, 255]);
        let again = p.snapshot(2, false, None).unwrap();
        assert!(Arc::ptr_eq(
            &frame.images[0].pixels,
            &again.images[0].pixels
        ));
        let old_key = frame.images[0].pixels.key;
        // Retransmission removes old placements in the pinned core. T creates
        // a new visible placement as part of the replacement transmission.
        p.process(b"\x1b[1;1H\x1b_Ga=T,f=32,s=2,v=2,i=7,c=2,r=2;AP///wD///8A////AP///w==\x1b\\")
            .unwrap();
        let replaced = p.snapshot(3, false, None).unwrap();
        assert_ne!(replaced.images[0].pixels.key, old_key);
        assert_eq!(replaced.images[0].pixels.bgra, [255, 255, 0, 255].repeat(4));
        // A previous UI snapshot owns its original bytes across C mutation.
        assert_eq!(frame.images[0].pixels.bgra[0..4], [0, 0, 255, 255]);
        p.process(b"\x1b[?1049h").unwrap();
        assert!(p.snapshot(4, false, None).unwrap().images.is_empty());
        p.process(b"\x1b[?1049l\x1b_Ga=d,d=I,i=7\x1b\\").unwrap();
        assert!(p.snapshot(5, false, None).unwrap().images.is_empty());
        assert!(p.image_cache.is_empty());
    }
    #[test]
    fn kitty_placement_frame_budget_does_not_discard_following_text() {
        let mut p = GhosttyParser::new(8, 20).unwrap();
        p.resize(8, 20, 9, 18).unwrap();
        p.process(b"\x1b_Ga=t,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\")
            .unwrap();
        for id in 1..=100 {
            p.process(format!("\x1b[1;1H\x1b_Ga=p,i=1,p={id},c=2,r=2\x1b\\").as_bytes())
                .unwrap();
        }
        p.process(b"\x1b[5;1HAFTER-IMAGES").unwrap();
        let frame = p.snapshot(1, false, None).unwrap();
        assert_eq!(frame.images.len(), 64);
        assert!(frame.images_limited);
        assert!(frame.text().contains("AFTER-IMAGES"));
        assert_eq!(p.image_cache.len(), 1);
        p.process(b"\x1b_Ga=d,d=A\x1b\\").unwrap();
        let cleared = p.snapshot(2, false, None).unwrap();
        assert!(cleared.images.is_empty());
        assert!(!cleared.images_limited);
    }
    #[test]
    fn adapter_owns_unicode_styles_modes_and_replies() {
        let mut p = GhosttyParser::new(8, 20).unwrap();
        p.process("\x1b[1;31mñ界e\u{301}😀\x1b[0m\x1b[?2004h\x1b[?1h\x1b[6n".as_bytes())
            .unwrap();
        let s = p.snapshot(1, false, None).unwrap();
        assert_eq!(s.cells[0][0].text, "ñ");
        assert!(s.cells[0][0].bold);
        assert!(matches!(s.cells[0][0].fg, vt100::Color::Rgb(..)));
        assert!(s.cells[0][1].wide && s.cells[0][2].wide_continuation);
        assert_eq!(s.cells[0][3].text, "e\u{301}");
        assert_eq!(s.cursor, (0, 6));
        assert!(s.application_cursor && s.bracketed_paste);
        assert_eq!(p.replies(), b"\x1b[1;7R");
        assert!(p.replies().is_empty());
        p.process(b"\x1b[?1049hALT\x1b[?1049l").unwrap();
        assert_eq!(p.snapshot(2, false, None).unwrap().cells, s.cells);
    }
    #[test]
    fn snapshot_distinguishes_written_spaces_from_erased_cells() {
        let mut p = GhosttyParser::new(8, 20).unwrap();
        p.process(b"xy    ").unwrap();
        let written = p.snapshot(1, false, None).unwrap();
        assert!(written.cells[0][2..6].iter().all(|c| c.has_grapheme));
        assert!(!written.cells[0][6].has_grapheme);
        p.process(b"\r123456\rxy\x1b[K").unwrap();
        let erased = p.snapshot(2, false, None).unwrap();
        assert!(erased.cells[0][0..2].iter().all(|c| c.has_grapheme));
        assert!(erased.cells[0][2..6].iter().all(|c| !c.has_grapheme));
        assert_eq!(written.cells[0][2].text, erased.cells[0][2].text);
    }

    #[test]
    fn copy_mode_overlay_writes_spaces_not_original_content() {
        let mut p = GhosttyParser::new(8, 40).unwrap();
        // A synthetic tmux copy-mode redraw erases cells before its indicator,
        // then writes spaces over part of the row to update that indicator.
        p.process(b"xy\x1b[33X\x1b[33C[0/0]").unwrap();
        let before = p.snapshot(1, false, None).unwrap();
        assert!(!before.cells[0][10].has_grapheme);
        p.process(b"\x1b[1;11H                              ")
            .unwrap();
        let after = p.snapshot(2, false, None).unwrap();
        assert!(after.cells[0][10].has_grapheme);
        assert_eq!(before.cells[0][10].text, after.cells[0][10].text);
        // This flag describes the rendered frame; it is not trustworthy as
        // source content where tmux has overlaid its own interface.
    }

    #[test]
    fn synthetic_starship_prompt_keeps_unicode_glyph_and_redraws_after_failure() {
        let mut p = GhosttyParser::new(8, 20).unwrap();
        // Bytes produced by Starship 1.24.2 with an isolated fixture config:
        // success ❯ in bold green; error ✗ in bold red. No user profile loaded.
        p.process(b"\n\x1b[1;32m\xe2\x9d\xaf\x1b[0m ").unwrap();
        let success = p.snapshot(1, false, None).unwrap();
        assert_eq!(success.cells[1][0].text, "❯");
        assert!(success.cells[1][0].bold);
        assert_eq!(success.cursor, (1, 2));
        p.process(b"\r\x1b[2K\x1b[1;31m\xe2\x9c\x97\x1b[0m ")
            .unwrap();
        let error = p.snapshot(2, false, None).unwrap();
        assert_eq!(error.cells[1][0].text, "✗");
        assert!(error.cells[1][0].bold);
        assert_eq!(error.cursor, (1, 2));
        assert_ne!(success.cells[1][0].fg, error.cells[1][0].fg);
    }
    #[test]
    fn adapter_reflows_and_snapshots_clean_rows_again() {
        let mut p = GhosttyParser::new(8, 8).unwrap();
        p.process(b"abcdefghijklmno").unwrap();
        assert_eq!(
            p.snapshot(1, false, None).unwrap().text().trim_end(),
            "abcdefgh\nijklmno"
        );
        p.resize(8, 20, 9, 18).unwrap();
        let wide = p.snapshot(2, false, None).unwrap();
        assert_eq!(wide.text().trim_end(), "abcdefghijklmno");
        assert_eq!(wide.cursor, (0, 15));
        assert_eq!(p.snapshot(3, false, None).unwrap().cells, wide.cells);
        p.resize(8, 5, 9, 18).unwrap();
        assert_eq!(
            p.snapshot(4, false, None).unwrap().text().trim_end(),
            "abcde\nfghij\nklmno"
        );
    }
    #[test]
    fn adapter_bounds_chunks_graphemes_replies_and_scrollback() {
        assert!(GhosttyParser::new(201, 80).is_err());
        let mut p = GhosttyParser::new(4, 20).unwrap();
        assert!(p.process(&vec![0; 1024 * 1024 + 1]).is_err());
        assert!(p.resize(4, 401, 9, 18).is_err());
        assert!(p.scroll(2001).is_err());
        let huge = format!("e{}", "\u{301}".repeat(1000));
        p.process(huge.as_bytes()).unwrap();
        // The pinned page.zig bounds a cell to 64 suffix codepoints.
        assert_eq!(
            p.snapshot(1, false, None).unwrap().cells[0][0].text,
            format!("e{}", "\u{301}".repeat(64))
        );
        p.process(b"\x1bc").unwrap();
        for row in 0..100 {
            p.process(format!("row{row:03}\r\n").as_bytes()).unwrap();
        }
        let bottom = p.snapshot(2, false, None).unwrap().text();
        p.scroll(20).unwrap();
        assert_ne!(p.snapshot(3, false, None).unwrap().text(), bottom);
        p.scroll(0).unwrap();
        assert_eq!(p.snapshot(4, false, None).unwrap().text(), bottom);
        p.process(&b"\x1b[6n".repeat(2000)).unwrap();
        assert!(p.replies().len() <= 4096);
    }
    #[test]
    fn returning_to_bottom_follows_future_output_after_history_pruning() {
        let mut p = GhosttyParser::new(6, 20).unwrap();
        p.scroll(0).unwrap();
        for batch in 0..12 {
            let output = (batch * 1000..(batch + 1) * 1000)
                .map(|row| format!("ROW-{row:05}\r\n"))
                .collect::<String>();
            p.process(output.as_bytes()).unwrap();
        }
        assert!(
            p.snapshot(1, false, None)
                .unwrap()
                .text()
                .contains("ROW-11999")
        );
        p.scroll(20).unwrap();
        assert!(
            !p.snapshot(2, false, None)
                .unwrap()
                .text()
                .contains("ROW-11999")
        );
        p.scroll(0).unwrap();
        p.process(b"FOLLOWING-OUTPUT\r\n").unwrap();
        assert!(
            p.snapshot(3, false, None)
                .unwrap()
                .text()
                .contains("FOLLOWING-OUTPUT")
        );
    }
}
