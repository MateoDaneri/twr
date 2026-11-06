//! Bounded PTY transport. The UI never blocks on reads, writes or terminal parsing.
#[cfg(feature = "ghostty")]
use crate::terminal_ghostty::GhosttyParser as LiveParser;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
#[cfg(any(not(feature = "ghostty"), test))]
use std::num::NonZeroU16;
use std::{
    io::{Read, Write},
    ops::Range,
    path::Path,
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender, TryRecvError},
    },
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    /// Whether the parser reports an actual grapheme, independently of rendering.
    pub has_grapheme: bool,
    pub fg: vt100::Color,
    pub bg: vt100::Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub wide_continuation: bool,
    pub wide: bool,
    /// Selection state from the native terminal's render snapshot.
    pub selected: bool,
}

/// Expand a half-open cell range to include a complete wide grapheme.
/// Spacer heads are left alone unless they follow an actual wide cell.
pub fn selection_cell_range(cells: &[Cell], from: usize, to: usize) -> Range<usize> {
    let mut start = from.min(cells.len());
    let mut end = to.min(cells.len()).max(start);
    if start == end {
        return start..end;
    }
    if start > 0 && cells[start].wide_continuation && cells[start - 1].wide {
        start -= 1;
    }
    if end < cells.len() && cells[end].wide_continuation && cells[end - 1].wide {
        end += 1;
    }
    start..end
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub revision: u64,
    /// Changes only when the terminal content or viewport may have moved.
    pub content_revision: u64,
    /// Gesture whose native selection produced this frame, if any.
    pub selection_token: Option<u64>,
    pub published_at: std::time::Instant,
    pub rows: u16,
    pub cols: u16,
    pub cells: Vec<Vec<Cell>>,
    pub cursor: (u16, u16),
    pub hide_cursor: bool,
    pub cursor_style: u8,
    pub cursor_blink: bool,
    pub cursor_color: Option<u32>,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub exited: bool,
    pub error: Option<String>,
    pub images: Vec<crate::terminal_image::Placement>,
    pub images_limited: bool,
}
impl Snapshot {
    /// Publication replaces the Arc even when an early error keeps revision 0.
    pub fn is_new_frame(current: &Arc<Self>, previous: &Arc<Self>) -> bool {
        !Arc::ptr_eq(current, previous)
    }
    fn blank(rows: u16, cols: u16) -> Self {
        let cell = Cell {
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
        Self {
            revision: 0,
            content_revision: 0,
            selection_token: None,
            published_at: std::time::Instant::now(),
            rows,
            cols,
            cells: vec![vec![cell; cols as usize]; rows as usize],
            cursor: (0, 0),
            hide_cursor: false,
            cursor_style: 1,
            cursor_blink: true,
            cursor_color: None,
            application_cursor: false,
            bracketed_paste: false,
            exited: false,
            error: None,
            images: Vec::new(),
            images_limited: false,
        }
    }
    #[cfg(any(not(feature = "ghostty"), test))]
    fn from_parser(
        parser: &vt100::Parser<Replies>,
        revision: u64,
        exited: bool,
        error: Option<String>,
    ) -> Self {
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let cells = (0..rows.get())
            .map(|row| {
                (0..cols.get())
                    .filter_map(|col| screen.cell(row, col))
                    .map(|c| Cell {
                        has_grapheme: !c.contents().is_empty(),
                        text: if c.contents().is_empty() {
                            " ".into()
                        } else {
                            c.contents().into()
                        },
                        fg: c.fgcolor(),
                        bg: c.bgcolor(),
                        bold: c.bold(),
                        italic: c.italic(),
                        underline: c.underline(),
                        inverse: c.inverse(),
                        wide_continuation: c.is_wide_continuation(),
                        wide: c.is_wide(),
                        selected: false,
                    })
                    .collect()
            })
            .collect();
        Self {
            revision,
            content_revision: revision,
            selection_token: None,
            published_at: std::time::Instant::now(),
            rows: rows.get(),
            cols: cols.get(),
            cells,
            cursor: screen.cursor_position(),
            hide_cursor: screen.hide_cursor(),
            cursor_style: 1,
            cursor_blink: true,
            cursor_color: None,
            application_cursor: screen.application_cursor(),
            bracketed_paste: screen.bracketed_paste(),
            exited,
            error,
            images: Vec::new(),
            images_limited: false,
        }
    }
    pub fn text(&self) -> String {
        self.cells
            .iter()
            .map(|row| {
                row.iter()
                    .filter(|c| !c.wide_continuation)
                    .map(|c| c.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
#[derive(Default)]
#[cfg(any(not(feature = "ghostty"), test))]
struct Replies(Vec<u8>);
#[cfg(any(not(feature = "ghostty"), test))]
impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        let reply = match (i1, c, first) {
            (None, 'n', 5) => Some("\x1b[0n".into()),
            (None, 'n', 6) => {
                let (r, c) = screen.cursor_position();
                Some(format!("\x1b[{};{}R", r + 1, c + 1))
            }
            (None, 'c', 0) => Some("\x1b[?1;2c".into()),
            _ => None,
        };
        if let Some(reply) = reply
            && self.0.len() + reply.len() <= 4096
        {
            self.0.extend_from_slice(reply.as_bytes());
        }
    }
    // OSC 52 and title changes have no host-side effect. No clipboard reads or writes.
}
#[cfg(not(feature = "ghostty"))]
struct LiveParser(vt100::Parser<Replies>);
#[cfg(not(feature = "ghostty"))]
impl LiveParser {
    fn new(rows: u16, cols: u16) -> Result<Self, String> {
        Ok(Self(vt100::Parser::new_with_callbacks(
            NonZeroU16::new(rows).unwrap(),
            NonZeroU16::new(cols).unwrap(),
            2000,
            Replies::default(),
        )))
    }
    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.0.process(bytes);
        Ok(())
    }
    fn resize(&mut self, rows: u16, cols: u16, _width: u32, _height: u32) -> Result<(), String> {
        self.0.set_size(
            NonZeroU16::new(rows).unwrap(),
            NonZeroU16::new(cols).unwrap(),
        );
        Ok(())
    }
    fn scroll(&mut self, rows: usize) -> Result<(), String> {
        self.0.screen_mut().set_scrollback(rows.min(2000));
        Ok(())
    }
    fn replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0.callbacks_mut().0)
    }
    fn animation_tick(&mut self, _now_ms: u64) -> Result<Option<u64>, String> {
        Ok(None)
    }
    fn snapshot(
        &mut self,
        revision: u64,
        exited: bool,
        error: Option<String>,
    ) -> Result<Snapshot, String> {
        Ok(Snapshot::from_parser(&self.0, revision, exited, error))
    }
}
#[derive(Debug)]
enum Command {
    Input(Vec<u8>),
    InputTracked {
        bytes: Vec<u8>,
        reply: SyncSender<Result<(), String>>,
    },
    #[cfg(feature = "ghostty")]
    SelectionBegin {
        token: u64,
        frame: u64,
        row: u16,
        col: u16,
        reply: SyncSender<Result<(), String>>,
    },
    #[cfg(feature = "ghostty")]
    SelectionExtend {
        token: u64,
        frame: u64,
        row: u16,
        col: u16,
        reply: SyncSender<Result<(), String>>,
    },
    #[cfg(feature = "ghostty")]
    SelectionClear {
        token: u64,
        reply: SyncSender<Result<(), String>>,
    },
    #[cfg(feature = "ghostty")]
    SelectionCopy {
        token: u64,
        reply: SyncSender<Result<String, String>>,
    },
    #[cfg(test)]
    WriterBarrier(std::sync::mpsc::SyncSender<()>),
    Resize(u16, u16, u16, u16),
    Scroll(usize),
    Shutdown,
}
pub struct Terminal {
    commands: SyncSender<Command>,
    identity: Arc<()>,
    latest: Arc<Mutex<Arc<Snapshot>>>,
    initial: Arc<Snapshot>,
    child_pid: Option<u32>,
    size: (u16, u16),
    cell_size: (u16, u16),
}
impl Terminal {
    pub fn spawn(cwd: &Path, label: &str) -> Result<Self, String> {
        let mut command = CommandBuilder::new("/bin/sh");
        command.arg("-i");
        command.cwd(cwd);
        command.env_clear();
        command.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin");
        command.env("TERM", "xterm-256color");
        command.env("LANG", "en_US.UTF-8");
        command.env("PS1", format!("{label} > "));
        Self::spawn_command(command)
    }
    pub fn spawn_command(command: CommandBuilder) -> Result<Self, String> {
        Self::spawn_notifying(command, None)
    }
    pub fn spawn_notifying(
        command: CommandBuilder,
        wake: Option<async_channel::Sender<()>>,
    ) -> Result<Self, String> {
        Self::spawn_inner(
            command,
            wake,
            #[cfg(test)]
            None,
        )
    }
    #[cfg(all(test, feature = "ghostty"))]
    pub(crate) fn spawn_capturing(
        command: CommandBuilder,
        capture: Arc<Mutex<Vec<u8>>>,
    ) -> Result<Self, String> {
        Self::spawn_inner(command, None, Some(capture))
    }
    fn spawn_inner(
        command: CommandBuilder,
        wake: Option<async_channel::Sender<()>>,
        #[cfg(test)] capture: Option<Arc<Mutex<Vec<u8>>>>,
    ) -> Result<Self, String> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| e.to_string())?;
        let child_pid = child.process_id();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let initial = Arc::new(Snapshot::blank(24, 80));
        let latest = Arc::new(Mutex::new(initial.clone()));
        let published = latest.clone();
        let (commands_tx, commands_rx) = mpsc::sync_channel(64);
        let (output_tx, output_rx) = mpsc::sync_channel::<Result<Vec<u8>, String>>(64);
        std::thread::spawn(move || {
            loop {
                let mut buffer = [0; 8192];
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        #[cfg(test)]
                        if let Some(capture) = &capture
                            && let Ok(mut captured) = capture.lock()
                        {
                            let count = n.min(2_097_152usize.saturating_sub(captured.len()));
                            captured.extend_from_slice(&buffer[..count]);
                        }
                        if output_tx.send(Ok(buffer[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = output_tx.send(Err(error.to_string()));
                        break;
                    }
                }
            }
        });
        std::thread::spawn(move || {
            // Construct the selected core inside its sole worker. The C handle is !Send/!Sync.
            let mut parser = match LiveParser::new(24, 80) {
                Ok(parser) => parser,
                Err(message) => {
                    let mut frame = Snapshot::blank(24, 80);
                    frame.error = Some(message);
                    frame.exited = true;
                    if let Ok(mut current) = published.lock() {
                        *current = Arc::new(frame);
                    }
                    if let Some(wake) = &wake {
                        let _ = wake.try_send(());
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
            };
            let mut revision = 0;
            // Saturation permanently rejects viewport-coordinate gestures,
            // rather than making an old generation valid again after wraparound.
            let mut content_revision: u64 = 0;
            #[cfg(feature = "ghostty")]
            let mut selection_token: Option<u64> = None;
            let mut error = None;
            let mut exited = false;
            // A viewport explicitly moved into history must be republished
            // when input returns it to the bottom, even without PTY output.
            let mut viewport_may_be_scrolled = false;
            let animation_clock = std::time::Instant::now();
            let mut next_animation: Option<u64> = None;
            // The post-exit wait must not consume and discard a command. Hold
            // its result for the ordinary FIFO handler on the next iteration.
            let mut deferred_command = None;
            'worker: loop {
                let mut changed = false;
                for _ in 0..64 {
                    match deferred_command
                        .take()
                        .map_or_else(|| commands_rx.try_recv(), Ok)
                    {
                        Ok(command @ (Command::Input(_) | Command::InputTracked { .. })) => {
                            let (bytes, reply) = match command {
                                Command::Input(bytes) => (bytes, None),
                                Command::InputTracked { bytes, reply } => (bytes, Some(reply)),
                                _ => unreachable!(),
                            };
                            if viewport_may_be_scrolled {
                                content_revision = content_revision.saturating_add(1);
                            }
                            match parser.scroll(0) {
                                Ok(()) => {
                                    changed |= viewport_may_be_scrolled;
                                    viewport_may_be_scrolled = false;
                                }
                                Err(e) => {
                                    error = Some(e);
                                    changed = true;
                                }
                            }
                            let result = writer
                                .write_all(&bytes)
                                .and_then(|_| writer.flush())
                                .map_err(|e| e.to_string());
                            if let Err(e) = &result {
                                error = Some(e.clone());
                                changed = true;
                            }
                            if let Some(reply) = reply {
                                let _ = reply.send(result);
                                if let Some(wake) = &wake {
                                    let _ = wake.try_send(());
                                }
                            }
                        }
                        #[cfg(feature = "ghostty")]
                        Ok(Command::SelectionBegin {
                            token,
                            frame,
                            row,
                            col,
                            reply,
                        }) => {
                            let result = if token == 0
                                || content_revision == u64::MAX
                                || frame != content_revision
                            {
                                Err("Terminal viewport changed; start the selection again".into())
                            } else {
                                parser.selection_begin(row, col)
                            };
                            if result.is_ok() {
                                selection_token = Some(token);
                                changed = true;
                            }
                            let _ = reply.try_send(result);
                        }
                        #[cfg(feature = "ghostty")]
                        Ok(Command::SelectionExtend {
                            token,
                            frame,
                            row,
                            col,
                            reply,
                        }) => {
                            let result = if selection_token != Some(token)
                                || content_revision == u64::MAX
                                || frame != content_revision
                            {
                                Err("Terminal selection or viewport changed".into())
                            } else {
                                parser.selection_extend(row, col)
                            };
                            changed |= result.is_ok();
                            let _ = reply.try_send(result);
                        }
                        #[cfg(feature = "ghostty")]
                        Ok(Command::SelectionClear { token, reply }) => {
                            let result = if selection_token != Some(token) {
                                Err("Terminal selection changed".into())
                            } else {
                                parser.selection_clear()
                            };
                            if result.is_ok() {
                                selection_token = None;
                                changed = true;
                            }
                            let _ = reply.try_send(result);
                        }
                        #[cfg(feature = "ghostty")]
                        Ok(Command::SelectionCopy { token, reply }) => {
                            let result = if selection_token != Some(token) {
                                Err("Terminal selection changed".into())
                            } else {
                                parser.selection_copy()
                            };
                            let _ = reply.try_send(result);
                        }
                        #[cfg(test)]
                        Ok(Command::WriterBarrier(done)) => {
                            let _ = done.send(());
                        }
                        Ok(Command::Resize(rows, cols, width, height)) => {
                            content_revision = content_revision.saturating_add(1);
                            let rows = rows.clamp(2, 200);
                            let cols = cols.clamp(2, 400);
                            if let Err(e) = pair.master.resize(PtySize {
                                rows,
                                cols,
                                pixel_width: cols.saturating_mul(width),
                                pixel_height: rows.saturating_mul(height),
                            }) {
                                error = Some(e.to_string());
                            } else {
                                if let Err(e) =
                                    parser.resize(rows, cols, width as u32, height as u32)
                                {
                                    error = Some(e);
                                }
                            }
                            changed = true;
                        }
                        Ok(Command::Scroll(rows)) => {
                            content_revision = content_revision.saturating_add(1);
                            match parser.scroll(rows.min(2000)) {
                                Ok(()) => viewport_may_be_scrolled = rows > 0,
                                Err(e) => error = Some(e),
                            }
                            changed = true;
                        }
                        Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => break 'worker,
                        Err(TryRecvError::Empty) => break,
                    }
                }
                match output_rx.recv_timeout(Duration::from_millis(16)) {
                    Ok(chunk) => {
                        match chunk {
                            Ok(bytes) => {
                                content_revision = content_revision.saturating_add(1);
                                if let Err(e) = parser.process(&bytes) {
                                    error = Some(e);
                                }
                            }
                            Err(e) => error = Some(e),
                        };
                        changed = true;
                        for _ in 0..31 {
                            match output_rx.try_recv() {
                                Ok(Ok(bytes)) => {
                                    content_revision = content_revision.saturating_add(1);
                                    if let Err(e) = parser.process(&bytes) {
                                        error = Some(e);
                                    }
                                }
                                Ok(Err(e)) => error = Some(e),
                                Err(_) => break,
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if !exited {
                            exited = true;
                            changed = true;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                let now_ms = animation_clock.elapsed().as_millis().min(u64::MAX as u128) as u64;
                let animation_due = next_animation.is_some_and(|next| now_ms >= next);
                if changed || animation_due {
                    next_animation = match parser.animation_tick(now_ms) {
                        // Timer-only redraws wait at least16ms, so a zero-gap
                        // animation cannot spin. New PTY output can also tick.
                        Ok(next) => next.map(|next| next.max(now_ms.saturating_add(16))),
                        Err(message) => {
                            error = Some(message);
                            None
                        }
                    };
                    changed = true;
                }
                let replies = parser.replies();
                if !replies.is_empty() {
                    let _ = writer.write_all(&replies);
                    let _ = writer.flush();
                }
                if changed {
                    revision += 1;
                    let (snapshot, fatal) = match parser.snapshot(revision, exited, error.clone()) {
                        Ok(mut frame) => {
                            frame.content_revision = content_revision;
                            #[cfg(feature = "ghostty")]
                            {
                                frame.selection_token = selection_token;
                            }
                            (Arc::new(frame), false)
                        }
                        Err(message) => {
                            let mut frame = published
                                .lock()
                                .ok()
                                .map(|s| (**s).clone())
                                .unwrap_or_else(|| Snapshot::blank(24, 80));
                            frame.error = Some(message);
                            frame.exited = true;
                            frame.revision = revision;
                            frame.content_revision = content_revision;
                            frame.published_at = std::time::Instant::now();
                            exited = true;
                            (Arc::new(frame), true)
                        }
                    };
                    if let Ok(mut current) = published.lock() {
                        *current = snapshot;
                    }
                    if let Some(wake) = &wake {
                        let _ = wake.try_send(());
                    }
                    if fatal {
                        break 'worker;
                    }
                }
                if exited {
                    let wait =
                        next_animation.map_or(50, |next| next.saturating_sub(now_ms).clamp(1, 50));
                    match commands_rx.recv_timeout(Duration::from_millis(wait)) {
                        Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Ok(command) => deferred_command = Some(command),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        });
        Ok(Self {
            commands: commands_tx,
            identity: Arc::new(()),
            latest,
            initial,
            child_pid,
            size: (24, 80),
            cell_size: (0, 0),
        })
    }
    pub fn snapshot(&self) -> Option<Arc<Snapshot>> {
        self.latest.try_lock().ok().map(|s| s.clone())
    }
    /// Never block the UI or panic if the PTY worker holds the snapshot lock.
    pub fn initial_snapshot(&self) -> Arc<Snapshot> {
        self.snapshot().unwrap_or_else(|| self.initial.clone())
    }
    pub fn process_id(&self) -> Option<u32> {
        self.child_pid
    }
    /// Distinguishes a reattached terminal from an earlier instance with the same label.
    pub fn instance_identity(&self) -> Arc<()> {
        self.identity.clone()
    }
    #[cfg(test)]
    pub(crate) fn worker_lifetime(&self) -> std::sync::Weak<Mutex<Arc<Snapshot>>> {
        Arc::downgrade(&self.latest)
    }
    /// Test-only: confirms earlier input was written to the PTY master, not
    /// that tmux consumed, parsed or delivered it to the pane.
    #[cfg(test)]
    pub(crate) fn wait_for_writer(&self) -> Result<(), String> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command::WriterBarrier(tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .map_err(|e| e.to_string())
    }
    pub fn input(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > 65536 {
            return Err("input exceeds 64 KiB per event".into());
        }
        self.commands
            .try_send(Command::Input(bytes.to_vec()))
            .map_err(|e| e.to_string())
    }
    /// A successful reply means the PTY writer accepted all bytes and flush
    /// returned successfully. Failure can follow a partial write; do not retry.
    pub fn input_tracked(
        &self,
        bytes: &[u8],
    ) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        if bytes.len() > 65536 {
            return Err("input exceeds 64 KiB per event".into());
        }
        let (reply, received) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command::InputTracked {
                bytes: bytes.to_vec(),
                reply,
            })
            .map_err(|error| error.to_string())?;
        Ok(received)
    }
    #[cfg(feature = "ghostty")]
    pub fn selection_begin(
        &self,
        token: u64,
        frame: u64,
        row: u16,
        col: u16,
    ) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        let (reply, received) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command::SelectionBegin {
                token,
                frame,
                row,
                col,
                reply,
            })
            .map_err(|_| "Terminal command queue is unavailable".to_owned())?;
        Ok(received)
    }
    #[cfg(feature = "ghostty")]
    pub fn selection_extend(
        &self,
        token: u64,
        frame: u64,
        row: u16,
        col: u16,
    ) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        let (reply, received) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command::SelectionExtend {
                token,
                frame,
                row,
                col,
                reply,
            })
            .map_err(|_| "Terminal command queue is unavailable".to_owned())?;
        Ok(received)
    }
    #[cfg(feature = "ghostty")]
    pub fn selection_clear(
        &self,
        token: u64,
    ) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        let (reply, received) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command::SelectionClear { token, reply })
            .map_err(|_| "Terminal command queue is unavailable".to_owned())?;
        Ok(received)
    }
    #[cfg(feature = "ghostty")]
    pub fn selection_copy(
        &self,
        token: u64,
    ) -> Result<mpsc::Receiver<Result<String, String>>, String> {
        let (reply, received) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command::SelectionCopy { token, reply })
            .map_err(|_| "Terminal command queue is unavailable".to_owned())?;
        Ok(received)
    }
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.resize_cells(rows, cols, 0, 0)
    }
    pub fn resize_cells(
        &mut self,
        rows: u16,
        cols: u16,
        width: u16,
        height: u16,
    ) -> Result<(), String> {
        let next = (rows.clamp(2, 200), cols.clamp(2, 400));
        let cells = (width.min(256), height.min(256));
        if next == self.size && cells == self.cell_size {
            return Ok(());
        }
        self.commands
            .try_send(Command::Resize(next.0, next.1, cells.0, cells.1))
            .map_err(|e| e.to_string())?;
        self.size = next;
        self.cell_size = cells;
        Ok(())
    }
    pub fn scroll(&self, rows: usize) -> Result<(), String> {
        self.commands
            .try_send(Command::Scroll(rows))
            .map_err(|e| e.to_string())
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.commands.try_send(Command::Shutdown);
    }
}

/// Encode non-text keys. Text/IME commits remain UTF-8 and bypass this encoder.
pub fn key_bytes(
    key: &str,
    control: bool,
    alt: bool,
    shift: bool,
    application_cursor: bool,
) -> Option<Vec<u8>> {
    // Ghostty's legacy PC-style function keys encode Shift, Alt and Ctrl as
    // 1 + their bitmask. Modified cursor keys use CSI even in application
    // cursor mode; only the unmodified form switches between CSI and SS3.
    let modifiers = u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(control);
    let cursor = match key {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => None,
    };
    if let Some(final_byte) = cursor {
        return Some(
            if modifiers == 0 {
                format!(
                    "\x1b{}{final_byte}",
                    if application_cursor { "O" } else { "[" }
                )
            } else {
                format!("\x1b[1;{}{final_byte}", modifiers + 1)
            }
            .into_bytes(),
        );
    }
    let number_key = match key {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        "f13" => Some(25),
        "f14" => Some(26),
        "f15" => Some(28),
        "f16" => Some(29),
        "f17" => Some(31),
        "f18" => Some(32),
        "f19" => Some(33),
        "f20" => Some(34),
        "f21" => Some(42),
        "f22" => Some(43),
        "f23" => Some(44),
        "f24" => Some(45),
        "f25" => Some(46),
        _ => None,
    };
    if let Some(number) = number_key {
        return Some(
            if modifiers == 0 {
                format!("\x1b[{number}~")
            } else {
                format!("\x1b[{number};{}~", modifiers + 1)
            }
            .into_bytes(),
        );
    }
    let function = match key {
        "f1" => Some(('P', 1)),
        "f2" => Some(('Q', 1)),
        "f3" => Some(('R', 13)),
        "f4" => Some(('S', 1)),
        _ => None,
    };
    if let Some((final_byte, number)) = function {
        return Some(
            if modifiers == 0 {
                format!("\x1bO{final_byte}")
            } else if number == 13 {
                format!("\x1b[13;{}~", modifiers + 1)
            } else {
                format!("\x1b[1;{}{final_byte}", modifiers + 1)
            }
            .into_bytes(),
        );
    }
    let mut bytes = if control {
        let b = match key {
            "@" | "space" => 0,
            "[" => 27,
            "\\" => 28,
            "]" => 29,
            "^" => 30,
            "/" | "_" => 31,
            "?" => 127,
            k if k.len() == 1 && k.as_bytes()[0].is_ascii_alphabetic() => {
                k.as_bytes()[0].to_ascii_lowercase() - b'a' + 1
            }
            _ => return None,
        };
        vec![b]
    } else {
        match key {
            "enter" => vec![13],
            "backspace" => vec![127],
            "escape" => vec![27],
            "tab" if shift => b"\x1b[Z".to_vec(),
            "tab" => vec![9],
            _ => return None,
        }
    };
    if alt {
        bytes.insert(0, 27);
    }
    Some(bytes)
}
/// One-based SGR coordinates; explicit Shift-selection stays local in the UI.
pub fn mouse_event(button: u8, row: u16, col: u16, released: bool) -> Vec<u8> {
    format!(
        "\x1b[<{button};{};{}{}",
        u32::from(col) + 1,
        u32::from(row) + 1,
        if released { 'm' } else { 'M' }
    )
    .into_bytes()
}

/// SGR mouse wheel events consumed by tmux, routed to copy-mode or pane application.
pub fn mouse_wheel(delta: f32, row: u16, col: u16, steps: usize) -> Vec<u8> {
    if !delta.is_finite() || delta == 0. {
        return Vec::new();
    }
    let button = if delta > 0. { 64 } else { 65 };
    format!(
        "\x1b[<{button};{};{}M",
        u32::from(col) + 1,
        u32::from(row) + 1
    )
    .repeat(steps.min(8))
    .into_bytes()
}

#[cfg(test)]
mod tests {
    #[test]
    fn initial_snapshot_survives_busy_worker_and_sees_revision_zero_error() {
        use super::{Snapshot, Terminal};
        use std::sync::{Arc, Mutex, mpsc};
        let initial = Arc::new(Snapshot::blank(24, 80));
        let latest = Arc::new(Mutex::new(initial.clone()));
        let (commands, _receiver) = mpsc::sync_channel(2);
        let terminal = Terminal {
            commands,
            latest: latest.clone(),
            initial: initial.clone(),
            child_pid: None,
            size: (24, 80),
            cell_size: (0, 0),
        };
        let mut held = latest.lock().unwrap();
        assert!(terminal.snapshot().is_none());
        let fallback = terminal.initial_snapshot();
        assert!(Arc::ptr_eq(&fallback, &initial));
        let mut failed = Snapshot::blank(24, 80);
        failed.error = Some("synthetic parser failure".into());
        failed.exited = true;
        *held = Arc::new(failed);
        drop(held);
        let current = terminal.snapshot().unwrap();
        assert_eq!(current.revision, fallback.revision);
        assert!(Snapshot::is_new_frame(&current, &fallback));
        assert!(!Snapshot::is_new_frame(&current, &current.clone()));
        assert!(current.exited);
    }

    #[test]
    fn shared_terminal_remains_usable_after_one_view_closes() {
        use crate::buffer_registry::{Buffer, BufferRegistry};
        use crate::editor_panes::{Content, Panes};
        use portable_pty::CommandBuilder;
        use std::time::{Duration, Instant};
        use workspace_core::editor_panes::Axis;

        let mut command = CommandBuilder::new("/bin/cat");
        command.env_clear();
        let terminal = super::Terminal::spawn_command(command).unwrap();
        let mut panes = Panes::terminal(42);
        let mut registry = BufferRegistry::default();
        registry.register(Buffer::Terminal(42));
        let first = panes.layout.active();
        let second = panes.layout.split(Axis::Row, true).unwrap();
        registry.duplicate_pane(first, second);
        terminal.input(b"BEFORE_CLOSE_TOKEN\n").unwrap();
        let wait_for = |token: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(frame) = terminal.snapshot() {
                    assert!(!frame.exited, "synthetic PTY exited unexpectedly");
                    assert!(frame.error.is_none(), "{:?}", frame.error);
                    if frame.text().contains(token) {
                        break;
                    }
                }
                assert!(Instant::now() < deadline, "PTY did not display {token}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        wait_for("BEFORE_CLOSE_TOKEN");
        let closed = panes.layout.close().unwrap();
        registry.close_pane(closed);
        assert_eq!(closed, second);
        assert_eq!(panes.content(first), Some(&Content::Terminal(42)));
        assert_eq!(registry.listed(), &[Buffer::Terminal(42)]);
        terminal.input(b"AFTER_CLOSE_TOKEN\n").unwrap();
        wait_for("AFTER_CLOSE_TOKEN");
    }
    #[test]
    fn wheel_routes_direction_position_and_bounds_to_tmux() {
        assert_eq!(
            super::mouse_wheel(3., 4, 7, 2),
            b"\x1b[<64;8;5M\x1b[<64;8;5M"
        );
        assert_eq!(super::mouse_wheel(-3., 0, 0, 1), b"\x1b[<65;1;1M");
        assert!(super::mouse_wheel(0., 0, 0, 1).is_empty());
        assert!(super::mouse_wheel(f32::NAN, 0, 0, 1).is_empty());
        assert_eq!(super::mouse_wheel(1., 0, 0, usize::MAX).len(), 8 * 10);
    }

    use super::*;
    #[cfg(feature = "ghostty")]
    #[test]
    fn real_pty_animation_advances_without_more_program_output() {
        let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../Resources/Helpers/terminal_animation_demo.py");
        let mut command = CommandBuilder::new("/usr/bin/python3");
        command.env_clear();
        command.env("PATH", "/usr/bin:/bin");
        command.args(["-I", "-S"]);
        command.arg(helper);
        command.args(["--once", "--frame-ms", "100"]);
        let mut terminal = Terminal::spawn_command(command).unwrap();
        terminal.resize_cells(24, 80, 9, 18).unwrap();
        let mut first: Option<Arc<Snapshot>> = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(frame) = terminal.snapshot() {
                assert!(frame.error.is_none(), "{:?}", frame.error);
                if frame.text().contains("FRAME-READY") && !frame.images.is_empty() {
                    if let Some(first) = &first {
                        if first.images[0].pixels.bgra != frame.images[0].pixels.bgra
                            && frame.exited
                        {
                            assert!(frame.revision > first.revision);
                            assert_ne!(frame.images[0].pixels.key, first.images[0].pixels.key);
                            break;
                        }
                    } else {
                        first = Some(frame);
                    }
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Animation PTY deadline"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[cfg(feature = "ghostty")]
    #[test]
    fn real_pty_collects_chunked_rgba_from_the_bundled_synthetic_demo() {
        let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../Resources/Helpers/terminal_graphics_demo.py");
        let mut command = CommandBuilder::new("/usr/bin/python3");
        command.env_clear();
        command.env("PATH", "/usr/bin:/bin");
        command.args(["-I", "-S"]);
        command.arg(helper);
        command.arg("--once");
        let mut terminal = Terminal::spawn_command(command).unwrap();
        terminal.resize_cells(24, 80, 9, 18).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(frame) = terminal.snapshot() {
                assert!(frame.error.is_none(), "{:?}", frame.error);
                if frame.text().contains("FRAME-READY") {
                    assert_eq!(frame.images.len(), 1);
                    assert!(!frame.images_limited);
                    let image = &frame.images[0];
                    assert_eq!(
                        (image.col, image.row, image.width, image.height),
                        (0., 2., 24., 8.)
                    );
                    assert_eq!((image.pixels.width, image.pixels.height), (64, 64));
                    assert_eq!(image.pixels.bgra[0..4], [0, 0, 255, 255]);
                    assert_eq!(
                        image.pixels.bgra[64 * 32 * 4..64 * 32 * 4 + 4],
                        [255, 0, 0, 255]
                    );
                    break;
                }
            }
            assert!(std::time::Instant::now() < deadline, "RGBA PTY deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn key_control_and_application_cursor_modes() {
        assert_eq!(key_bytes("c", true, false, false, false), Some(vec![3]));
        assert_eq!(
            key_bytes("up", false, false, false, true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            key_bytes("backspace", false, false, false, false),
            Some(vec![127])
        );
    }
    #[test]
    fn fallback_snapshot_keeps_written_space_and_erasure_distinct() {
        let mut p = vt100::Parser::new_with_callbacks(
            NonZeroU16::new(8).unwrap(),
            NonZeroU16::new(20).unwrap(),
            10,
            Replies::default(),
        );
        p.process(b"xy    ");
        let written = Snapshot::from_parser(&p, 1, false, None);
        assert!(written.cells[0][2..6].iter().all(|c| c.has_grapheme));
        assert!(!written.cells[0][6].has_grapheme);
        p.process(b"\r123456\rxy\x1b[K");
        let erased = Snapshot::from_parser(&p, 2, false, None);
        assert!(erased.cells[0][2..6].iter().all(|c| !c.has_grapheme));
        assert_eq!(written.cells[0][2].text, erased.cells[0][2].text);
    }

    #[test]
    fn parser_handles_alt_screen_utf8_attributes_and_responses() {
        let mut p = vt100::Parser::new_with_callbacks(
            NonZeroU16::new(6).unwrap(),
            NonZeroU16::new(20).unwrap(),
            10,
            Replies::default(),
        );
        p.process("\x1b[31mñ界\x1b[0m".as_bytes());
        let s = Snapshot::from_parser(&p, 1, false, None);
        assert_eq!(s.cells[0][0].text, "ñ");
        assert_eq!(s.cells[0][0].fg, vt100::Color::Idx(1));
        assert!(s.cells[0][2].wide_continuation);
        p.process(b"\x1b[?1049hALT\x1b[?1049l");
        assert!(p.screen().contents().contains("ñ界"));
        p.process(b"\x1b[6n");
        assert!(!p.callbacks().0.is_empty());
    }
    #[test]
    fn real_pty_resizes_and_reads_input() {
        let mut command = CommandBuilder::new("/bin/sh");
        command.env_clear();
        command.env("PATH", "/usr/bin:/bin");
        command.arg("-c");
        command.arg("IFS= read -r x; /bin/stty size; printf 'ECHO:%s\\n' \"$x\"");
        let mut t = Terminal::spawn_command(command).unwrap();
        t.resize(30, 90).unwrap();
        t.input(b"terminal-probe\n").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(s) = t.snapshot()
                && s.text().contains("ECHO:terminal-probe")
            {
                assert!(s.text().contains("30 90"));
                break;
            }
            assert!(std::time::Instant::now() < deadline, "PTY deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn output_backpressure_keeps_input_resize_and_unicode_tail() {
        let mut command = CommandBuilder::new("/usr/bin/python3");
        command.env_clear();
        command.env("PATH", "/usr/bin:/bin");
        command.env("LANG", "en_US.UTF-8");
        command.args(["-I", "-S", "-c", r#"
import os, sys
payload = ''.join('ROW-%05d %s\n' % (i, 'x' * 100) for i in range(12000))
sys.stdout.buffer.write(payload.encode('utf-8'))
sys.stdout.buffer.flush()
line = sys.stdin.buffer.readline().decode('ascii').strip()
size = os.get_terminal_size()
sys.stdout.buffer.write(('TAIL ñ界 e\u0301\nACK:%s SIZE:%dx%d\n' % (line, size.lines, size.columns)).encode('utf-8'))
sys.stdout.buffer.flush()
"#]);
        let mut terminal = Terminal::spawn_command(command).unwrap();
        for n in 0..12 {
            terminal.resize(30 + n, 100 + n).unwrap();
        }
        terminal.resize(50, 140).unwrap();
        terminal.input(b"after-load\n").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(snapshot) = terminal.snapshot() {
                let text = snapshot.text();
                if text.contains("ACK:after-load SIZE:50x140") {
                    assert_eq!((snapshot.rows, snapshot.cols), (50, 140));
                    assert!(text.contains("ROW-11999"));
                    assert!(text.contains("TAIL ñ界 e\u{301}"));
                    assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "PTY output/backpressure deadline; last frame: {:?}",
                terminal
                    .snapshot()
                    .map(|s| (s.revision, s.exited, s.error.clone(), s.text()))
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
