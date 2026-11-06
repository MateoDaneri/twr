//! Small native modal editor. Explicit subset, no Lua or hidden disk access.
use std::{ops::Range, path::Path, sync::Arc};
#[path = "editor_block.rs"]
mod block;
pub use block::WrappedBlockBounds;
#[path = "editor_cells.rs"]
mod cells;
pub use cells::CellPiece;
#[path = "editor_change_journal.rs"]
mod change_journal;
pub use change_journal::{
    ApplyOutcome as JumpApplyOutcome, Bridge as JumpBridge, Change as JumpChange,
    Impact as JumpImpact, LiveTracking as LiveJumpTracking, Origin as JumpOrigin,
    apply_to_layouts as apply_jump_changes,
};
#[path = "editor_clipboard.rs"]
mod clipboard;
#[path = "editor_commands.rs"]
mod commands;
#[path = "editor_folds.rs"]
mod folds;
#[path = "editor_grapheme.rs"]
mod grapheme;
#[cfg(test)]
#[path = "editor_grapheme_reference.rs"]
mod grapheme_reference;
#[path = "editor_indent.rs"]
mod indent;
#[path = "editor_input.rs"]
mod input;
pub use input::InputDisplay;
#[path = "editor_jump.rs"]
mod jump;
#[path = "editor_paragraph.rs"]
mod paragraph;
#[path = "editor_prompt.rs"]
mod prompt;
#[path = "editor_put.rs"]
mod put;
#[path = "editor_scroll.rs"]
mod scroll;
#[path = "editor_search.rs"]
mod search;
pub use scroll::{WheelAccumulator, followed_column};
pub use search::{SearchJob, SearchResult};
#[path = "editor_snippets.rs"]
mod snippets;
#[path = "editor_visual.rs"]
mod visual;
#[path = "editor_visual_memory.rs"]
mod visual_memory;
#[path = "editor_wrap.rs"]
mod wrap;
#[path = "editor_wrap_wheel.rs"]
mod wrap_wheel;
pub use wrap::{
    WrapCursor, WrapPosition, WrappedCell, WrappedCursorCell, WrappedMouseHit,
    WrappedSelectionCells, WrappedTop, WrappedViewportRow, WrappedVirtualCursor,
    move_in_wrapped_line, move_in_wrapped_lines, wrapped_row_cells, wrapped_selection_cells,
};
pub use wrap_wheel::WrappedWheelRequest;
#[path = "editor_word_search.rs"]
mod word_search;
pub use commands::ReplayContinuation;
use commands::{Await, EditEvent, ReplayFrame};
#[path = "editor_registers.rs"]
mod registers;
#[path = "editor_replay_bank.rs"]
mod replay_bank;
pub use registers::RegisterBank;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
    VisualBlock,
    Command,
    Search,
}
impl Mode {
    pub fn workspace_chord(self, chord: &str) -> bool {
        (self == Self::Normal && matches!(chord, "<C-h>" | "<C-j>" | "<C-k>" | "<C-l>" | "<C-/>"))
            || (self.window_navigation() && chord == "<C-w>")
    }
    pub fn window_navigation(self) -> bool {
        matches!(
            self,
            Self::Normal | Self::Visual | Self::VisualLine | Self::VisualBlock
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    NewFile,
    NewTerminal,
    Split(bool),
    SplitNew(bool),
    SplitNewSized(bool, usize),
    SplitPath(bool, String),
    SplitPathSized(bool, usize, String),
    Only,
    CloseWindow,
    WindowCommand(char, usize),
    WrappedMove(bool, usize),
    WrappedOperator(char, bool, usize),
    WrappedScroll(bool, usize),
    WrappedCenter,
    WrappedTop,
    WrappedBottom,
    OpenPath(String),
    Write(String, bool),
    WriteCopy(String),
    SaveAs(String),
    Save,
    Close(bool),
    SaveClose,
    Format,
    TexPrepare,
    OpenExternal,
    Search,
    JumpHistory(bool, usize),
    ClearJumps,
    Lsp(String),
    Markdown(String),
    Video(String),
    Git(String),
    ReconnectTerminal,
    RecoverTerminals,
    Agents(String),
    Diagnostics(bool),
    Metrics(bool),
    MetricsExport,
    SyntaxToggle,
    Wrap(Option<bool>),
    Theme(String),
    Typography(String),
    Viewport(usize),
    ClipboardRead,
    BufferList(bool),
    VimBufferDelete(bool),
    VimBufferDeleteTarget(bool, VimBufferTarget),
    Buffer(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VimBufferTarget {
    Number(u64),
    Alternate,
}
#[derive(Clone)]
struct Revision {
    text: Vec<char>,
    cursor: usize,
    redo_cursor_line: Option<usize>,
    visual: Option<visual_memory::Area>,
    visual_generation: u64,
}
/// Identity survives renames/moves of this editor but not replacing it.
#[derive(Clone, Debug)]
pub struct EditorIdentity(Arc<()>);
impl EditorIdentity {
    pub fn matches(&self, editor: &Editor) -> bool {
        Arc::ptr_eq(&self.0, &editor.identity)
    }
}
pub struct Editor {
    identity: Arc<()>,
    pub text: Vec<char>,
    pub cursor: usize,
    pub mode: Mode,
    pub command: String,
    prompt_cursor: usize,
    prompt_history: Option<prompt::HistoryNavigation>,
    prompt_register_pending: bool,
    prompt_clipboard_pending: Option<(String, usize, Mode, u64, usize)>,
    pub message: String,
    pub version: u64,
    pub saving: bool,
    original: String,
    saved: String,
    crlf: bool,
    anchor: usize,
    folds: Option<folds::Folds>,
    fold_level: usize,
    fold_language: Option<crate::syntax::Language>,
    parser_fold_result: Option<(u64, Result<Vec<crate::syntax_folds::Definition>, String>)>,
    block: Option<block::Selection>,
    block_insert: Option<block::Insertion>,
    last_visual: Option<visual_memory::Area>,
    visual_generation: u64,
    jump_changes: Option<change_journal::Journal>,
    jump_history_view: workspace_core::editor_jumplist::View,
    pending: String,
    count: usize,
    register: String,
    line_register: bool,
    undo: Vec<Revision>,
    redo: Vec<Revision>,
    search_reverse: bool,
    search_count: usize,
    search_job: Option<SearchJob>,
    search_pending: Option<search::Ticket>,
    search_generation: u64,
    search_followups: std::collections::VecDeque<bool>,
    search_preview: Option<search::Preview>,
    search_restore_view: Option<(usize, usize)>,
    search_highlights: Option<search::Highlights>,
    awaiting: Option<Await>,
    registers: RegisterBank,
    chosen_register: Option<char>,
    change_events: Vec<EditEvent>,
    change_failed: bool,
    change_version: u64,
    internal: bool,
    replay_depth: usize,
    replay_budget: usize,
    replay_stack: Vec<ReplayFrame>,
    last_find: Option<(char, char)>,
    marks: std::collections::BTreeMap<char, usize>,
    line_starts: Vec<usize>,
    grapheme_cache: std::cell::RefCell<Option<grapheme::Cache>>,
    modified: bool,
    jump: Option<jump::Jump>,
    jump_view: (usize, usize, usize, usize),
    scroll_lines: Option<usize>,
    desired_column: Option<(usize, usize)>,
    operator_goal_column: Option<usize>,
    wrap_input: bool,
    operator_count_explicit: bool,
    automatic_indent: Option<(u64, Range<usize>)>,
    input: Option<input::Composition>,
    snippet: Option<snippets::Active>,
    indent_style: indent::Style,
    clipboard_enabled: bool,
    clipboard_pending: Option<(String, usize, char, u64, usize, Mode)>,
    clipboard_ready: bool,
    clipboard_write: Option<(String, bool, Option<usize>)>,
    pub csv_enabled: bool,
    csv_delimiter: char,
    pub csv: Option<crate::csv::Table>,
}
fn targeted_buffer_delete(command: &str) -> Option<(bool, VimBufferTarget)> {
    let index = command.find(|c: char| !c.is_ascii_alphabetic())?;
    let (name, rest) = command.split_at(index);
    if !matches!(name, "bd" | "bdel" | "bdele" | "bdelet" | "bdelete") {
        return None;
    }
    let (force, rest) = rest
        .strip_prefix('!')
        .map_or((false, rest), |rest| (true, rest));
    let argument = rest.trim();
    if argument == "#" {
        return Some((force, VimBufferTarget::Alternate));
    }
    if argument.is_empty() || !argument.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((
        force,
        VimBufferTarget::Number(argument.parse().unwrap_or(u64::MAX)),
    ))
}

fn numbered_buffer_command(command: &str) -> Option<u64> {
    let index = command.find(|c: char| !c.is_ascii_alphabetic())?;
    let (name, rest) = command.split_at(index);
    let rest = rest.trim();
    if !matches!(name, "b" | "bu" | "buf" | "buff" | "buffe" | "buffer")
        || rest.is_empty()
        || !rest.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(rest.parse().unwrap_or(u64::MAX))
}

fn counted_split_command(
    command: &str,
) -> Option<Result<(char, usize, Option<String>), &'static str>> {
    let digits = command.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = command[digits..].trim_start();
    let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let key = match &rest[..name_end] {
        "sp" | "split" => 's',
        "vs" | "vsplit" => 'v',
        "new" => 'n',
        "vne" | "vnew" => 'N',
        _ => return None,
    };
    let argument = rest[name_end..].trim();
    if argument.starts_with('+') || matches!(argument, "#" | "%") || argument.contains('|') {
        return Some(Err("Argumento de split con conteo aún no disponible"));
    }
    let path = (!argument.is_empty()).then(|| argument.to_owned());
    Some(match command[..digits].parse::<usize>() {
        Ok(count @ 1..=9999) => Ok((key, count, path)),
        _ => Err("Conteo fuera de rango para el split"),
    })
}

/// Parse the supported subset of :[count]winc[md] [count] {arg}.
/// Unsupported counted operations fail explicitly instead of acting on the
/// wrong pane or silently ignoring a requested size.
fn window_command(command: &str) -> Option<Result<(char, usize), &'static str>> {
    let prefix_len = command.bytes().take_while(u8::is_ascii_digit).count();
    let rest = &command[prefix_len..];
    let name_len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    if !matches!(&rest[..name_len], "winc" | "wincm" | "wincmd") {
        return None;
    }
    let rest = rest[name_len..].trim_start();
    let suffix_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    if prefix_len > 0 && suffix_len > 0 {
        return Some(Err("Usá un solo conteo con :wincmd"));
    }
    let digits = if prefix_len > 0 {
        &command[..prefix_len]
    } else {
        &rest[..suffix_len]
    };
    let count = if digits.is_empty() {
        0
    } else {
        match digits.parse::<usize>() {
            Ok(value @ 1..=9999) => value,
            _ => return Some(Err("Conteo fuera de rango para :wincmd")),
        }
    };
    let argument = rest[suffix_len..].trim();
    let mut chars = argument.chars();
    let Some(key) = chars.next() else {
        return Some(Err("Falta el argumento de :wincmd"));
    };
    if chars.next().is_some()
        || !matches!(
            key,
            'h' | 'j'
                | 'k'
                | 'l'
                | 'w'
                | 'W'
                | 'p'
                | 't'
                | 'b'
                | 'x'
                | 'r'
                | 'R'
                | 'H'
                | 'J'
                | 'K'
                | 'L'
                | 'v'
                | 's'
                | 'S'
                | 'n'
                | 'c'
                | 'q'
                | 'o'
                | '>'
                | '<'
                | '+'
                | '-'
                | '|'
                | '_'
                | '='
        )
    {
        return Some(Err("Argumento de :wincmd aún no disponible"));
    }
    if count > 0 && matches!(key, 'o' | 'H' | 'J' | 'K' | 'L') {
        return Some(Err("Conteo de :wincmd aún no disponible para esta acción"));
    }
    Some(Ok((if key == 'S' { 's' } else { key }, count)))
}

impl Editor {
    pub fn identity(&self) -> EditorIdentity {
        EditorIdentity(self.identity.clone())
    }
    pub fn new(text: String) -> Self {
        let crlf = text.contains("\r\n");
        let normalized = text.replace("\r\n", "\n");
        let line_starts = std::iter::once(0)
            .chain(
                normalized
                    .chars()
                    .enumerate()
                    .filter_map(|(i, c)| (c == '\n').then_some(i + 1)),
            )
            .collect();
        Self {
            identity: Arc::new(()),
            text: normalized.chars().collect(),
            cursor: 0,
            mode: Mode::Normal,
            command: String::new(),
            prompt_cursor: 0,
            prompt_history: None,
            prompt_register_pending: false,
            prompt_clipboard_pending: None,
            message: String::new(),
            version: 0,
            saving: false,
            original: text,
            saved: normalized,
            crlf,
            anchor: 0,
            folds: None,
            fold_level: 99,
            fold_language: None,
            parser_fold_result: None,
            block: None,
            block_insert: None,
            last_visual: None,
            visual_generation: 0,
            jump_changes: None,
            jump_history_view: Default::default(),
            pending: String::new(),
            count: 0,
            register: String::new(),
            line_register: false,
            undo: Vec::new(),
            redo: Vec::new(),
            search_reverse: false,
            search_count: 1,
            search_job: None,
            search_pending: None,
            search_generation: 0,
            search_followups: std::collections::VecDeque::new(),
            search_preview: None,
            search_restore_view: None,
            search_highlights: None,
            awaiting: None,
            registers: Default::default(),
            chosen_register: None,
            change_events: Vec::new(),
            change_failed: false,
            change_version: 0,
            internal: false,
            replay_depth: 0,
            replay_budget: 0,
            replay_stack: Vec::new(),
            last_find: None,
            marks: Default::default(),
            line_starts,
            grapheme_cache: std::cell::RefCell::new(None),
            modified: false,
            jump: None,
            jump_view: (0, 0, 100, 200),
            scroll_lines: None,
            desired_column: None,
            operator_goal_column: None,
            wrap_input: false,
            operator_count_explicit: false,
            automatic_indent: None,
            input: None,
            snippet: None,
            indent_style: Default::default(),
            clipboard_enabled: false,
            clipboard_pending: None,
            clipboard_ready: false,
            clipboard_write: None,
            csv_enabled: false,
            csv_delimiter: ',',
            csv: None,
        }
    }
    pub fn with_registers(text: String, registers: RegisterBank) -> Self {
        let mut editor = Self::new(text);
        editor.registers = registers;
        editor
    }
    pub fn using_registers(mut self, registers: RegisterBank) -> Self {
        self.registers = registers;
        self
    }
    pub fn apply_lsp_edit(
        &mut self,
        range: &serde_json::Value,
        text: &str,
        version: u64,
    ) -> Result<(), String> {
        if self.version != version {
            return Err("Completion obsoleta: el buffer cambió".into());
        }
        let start =
            crate::lsp::offset(&self.text, &range["start"]).ok_or("Posición LSP inválida")?;
        let end = crate::lsp::offset(&self.text, &range["end"]).ok_or("Posición LSP inválida")?;
        if start > self.cursor || end < self.cursor || start > end {
            return Err("Completion fuera del cursor actual".into());
        }
        self.replace_completion(
            self.cursor - start,
            end - self.cursor,
            &text.replace("\r\n", "\n"),
        )
    }
    pub fn toggle_csv(&mut self, path: &Path) {
        if self.csv_enabled {
            self.csv_enabled = false;
            self.csv = None;
            return;
        }
        self.csv_delimiter = match path.extension().and_then(|e| e.to_str()) {
            Some("tsv") => '\t',
            Some("csv_pipe") => '|',
            Some("csv_semicolon" | "tsv_semicolon") => ';',
            _ => ',',
        };
        self.csv_enabled = true;
        self.reindex_csv();
    }
    fn reindex_csv(&mut self) {
        if self.csv_enabled {
            match crate::csv::Table::parse(&self.text, self.csv_delimiter) {
                Ok(t) => self.csv = Some(t),
                Err(e) => {
                    self.csv = None;
                    self.message = e;
                }
            }
        }
    }
    pub fn command_pending(&self) -> bool {
        self.jump.is_some() || !self.pending.is_empty() || self.awaiting.is_some() || self.count > 0
    }
    /// Commit a wheel-computed cursor without editing text or changing the visual anchor.
    pub fn commit_wrapped_wheel(&mut self, offset: usize, mode: Mode) -> bool {
        if self.mode != mode
            || self.composing()
            || offset > self.text.len()
            || !matches!(
                mode,
                Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::Insert
            )
        {
            return false;
        }
        let previous = self.cursor;
        self.cursor = offset;
        if mode == Mode::Normal {
            self.clamp();
        } else if offset < self.text.len() && self.text[offset] != '\n' {
            self.cursor = self.grapheme_floor(offset).unwrap_or(offset);
        }
        if self.cursor != offset {
            self.cursor = previous;
            return false;
        }
        self.desired_column = None;
        true
    }
    pub fn commit_wrapped_motion(&mut self, offset: usize) -> bool {
        if offset > self.text.len()
            || !matches!(self.mode, Mode::Normal | Mode::Visual | Mode::VisualLine)
        {
            return false;
        }
        let previous = self.cursor;
        self.cursor = offset;
        self.clamp();
        if self.cursor != offset {
            self.cursor = previous;
            return false;
        }
        self.desired_column = None;
        true
    }
    pub fn contents(&self) -> String {
        let s: String = self.text.iter().collect();
        if self.crlf {
            s.replace('\n', "\r\n")
        } else {
            s
        }
    }
    pub fn original(&self) -> String {
        self.original.clone()
    }
    pub fn from_recovery(original: String, text: String, cursor: usize) -> Self {
        let mut e = Self::new(original);
        e.text = text.replace("\r\n", "\n").chars().collect();
        e.cursor = cursor.min(e.text.len());
        e.changed();
        e.message = "Buffer recuperado · :w verifica cambios externos antes de guardar".into();
        e
    }
    pub fn replace_formatted(&mut self, text: String) {
        self.checkpoint();
        let text = text.replace("\r\n", "\n").chars().collect::<Vec<_>>();
        self.track_visual_replacement(&text);
        self.text = text;
        self.cursor = self.cursor.min(self.text.len());
        self.changed();
    }
    pub fn dirty(&self) -> bool {
        self.modified
    }
    pub fn saved(&mut self, contents: String) {
        self.original = contents.clone();
        self.saved = contents.replace("\r\n", "\n");
        self.saving = false;
        self.modified = !self.text.iter().copied().eq(self.saved.chars());
        self.message = if self.modified {
            "Versión guardada; quedan cambios nuevos"
        } else {
            "Guardado"
        }
        .into();
    }
    pub fn position(&self) -> (usize, usize) {
        let row = self
            .line_starts
            .partition_point(|p| *p <= self.cursor)
            .saturating_sub(1);
        (row, self.cursor - self.line_start(self.cursor))
    }
    /// Resolve an LSP UTF-16 position with the cached line index, scanning only its line.
    pub fn lsp_position(&self, offset: usize) -> serde_json::Value {
        let offset = offset.min(self.text.len());
        let row = self
            .line_starts
            .partition_point(|p| *p <= offset)
            .saturating_sub(1);
        let column = self.text[self.line_starts[row]..offset]
            .iter()
            .map(|c| c.len_utf16())
            .sum::<usize>();
        serde_json::json!({"line":row,"character":column})
    }
    pub fn label(&self) -> &'static str {
        match self.mode {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
            Mode::VisualBlock => "V-BLOCK",
            Mode::Command => "COMMAND",
            Mode::Search => "SEARCH",
        }
    }
    fn line_number(&self, n: usize) -> usize {
        self.line_starts
            .get(n.saturating_sub(1))
            .copied()
            .unwrap_or_else(|| *self.line_starts.last().unwrap_or(&0))
    }
    fn line_start(&self, p: usize) -> usize {
        self.line_starts[self
            .line_starts
            .partition_point(|s| *s <= p.min(self.text.len()))
            .saturating_sub(1)]
    }
    fn line_end(&self, p: usize) -> usize {
        self.line_starts
            .get(
                self.line_starts
                    .partition_point(|s| *s <= p.min(self.text.len())),
            )
            .map_or(self.text.len(), |p| p - 1)
    }
    fn grapheme_move(&self, p: usize, count: usize, right: bool, allow_end: bool) -> Option<usize> {
        self.with_grapheme_index(p, |index, offset, start, _| {
            index
                .move_by(offset, count, right, allow_end)
                .map(|p| start + p)
        })
    }
    fn grapheme_floor(&self, p: usize) -> Option<usize> {
        self.with_grapheme_index(p, |index, offset, start, _| {
            index.floor(offset).map(|p| start + p)
        })
    }
    fn grapheme_exact_forward(&self, p: usize, count: usize) -> Option<usize> {
        self.with_grapheme_index(p, |index, offset, start, _| {
            index.exact_forward(offset, count).map(|p| start + p)
        })
    }
    // Derived from clean Neovim 0.12.1 charclass() with its default iskeyword.
    // The run-length table has one entry per class transition; runtime Neovim
    // settings can differ and are intentionally not inferred from user files.
    fn word_class_at(&self, position: usize, big: bool) -> Option<u32> {
        use std::sync::OnceLock;
        static CLASSES: OnceLock<Vec<(u32, u32, u32)>> = OnceLock::new();
        let classes = CLASSES.get_or_init(|| {
            include_str!("../../../Resources/Fixtures/nvim-word-classes-0.12.1.txt")
                .lines()
                .filter(|line| !line.starts_with('#'))
                .map(|line| {
                    let values: Vec<u32> = line
                        .split_ascii_whitespace()
                        .map(|value| u32::from_str_radix(value, 16).expect("valid character class"))
                        .collect();
                    assert_eq!(values.len(), 3);
                    (values[0], values[1], values[2])
                })
                .collect()
        });
        let mut ch = *self.text.get(position)?;
        if ch == '\n' {
            return Some(0);
        }
        if !ch.is_ascii() {
            ch = self.text[self.grapheme_floor(position)?];
        }
        let code = ch as u32;
        let index = classes.partition_point(|(start, _, _)| *start <= code);
        let class = index.checked_sub(1).and_then(|i| {
            let (_, end, class) = classes[i];
            (code <= end).then_some(class)
        })?;
        Some(if big && class != 0 { 2 } else { class })
    }
    fn empty_line_at(&self, position: usize) -> bool {
        self.text.get(position) == Some(&'\n') && self.line_start(position) == position
    }
    fn operate_forward_word(&mut self, op: char, key: &str, count: usize) {
        let origin = self.cursor;
        let logical_end = self
            .text
            .len()
            .saturating_sub(usize::from(self.text.last() == Some(&'\n')));
        let start = self.line_start(origin);
        let first_nonblank = start
            + self.text[start..self.line_end(start)]
                .iter()
                .take_while(|c| matches!(c, ' ' | '\t'))
                .count();
        let mut end = origin;
        let mut crossed_before = false;
        let mut forced_linewise = false;
        let mut ended_on_empty = false;
        for step in 0..count {
            self.cursor = end;
            let Some(next) = self.motion(key, 1) else {
                self.cursor = origin;
                self.chosen_register = None;
                return;
            };
            let crossed = self.source_line_at(next) > self.source_line_at(end);
            if step + 1 == count && self.empty_line_at(end) {
                if end < logical_end {
                    end += 1;
                }
                ended_on_empty = true;
                break;
            }
            if step + 1 == count && crossed {
                if op == 'd' && crossed_before && origin <= first_nonblank {
                    end = self.line_start(next);
                    forced_linewise = true;
                } else if op != 'd' || !self.empty_line_at(origin) {
                    end = self.line_end(end);
                } else {
                    end = next;
                }
                break;
            }
            if next == end {
                break;
            }
            crossed_before |= crossed;
            end = next;
        }
        self.cursor = origin;
        let crosses_line = self.source_line_at(end) > self.source_line_at(origin);
        let end_at_line_start = crosses_line && end == self.line_start(end);
        let delete_to_eof = op == 'd' && crosses_line && end >= logical_end;
        let linewise =
            forced_linewise || origin <= first_nonblank && (end_at_line_start || delete_to_eof);
        let ends_before_last_empty = end == logical_end && self.empty_line_at(logical_end);
        if linewise
            && end == logical_end
            && self.text.last() == Some(&'\n')
            && !ends_before_last_empty
        {
            end += 1;
        }
        if end_at_line_start && !linewise {
            end = end.saturating_sub(1);
        }
        if end > origin {
            self.operate(
                op,
                if linewise { start..end } else { origin..end },
                linewise,
            );
            if op == 'd' && linewise && ends_before_last_empty && self.text == ['\n'] {
                self.text.clear();
                self.cursor = 0;
                self.changed();
            }
            if op == 'd'
                && linewise
                && self.cursor == self.text.len()
                && self.text.last() == Some(&'\n')
            {
                self.cursor = self.line_start(self.cursor.saturating_sub(1));
                self.clamp();
            }
            if op == 'd' && linewise && ended_on_empty {
                let start = self.line_start(self.cursor);
                self.cursor = start
                    + self.text[start..self.line_end(start)]
                        .iter()
                        .take_while(|c| matches!(c, ' ' | '\t'))
                        .count();
                self.clamp();
            }
        } else {
            self.chosen_register = None;
            if op == 'c' {
                self.checkpoint();
                self.mode = Mode::Insert;
            }
        }
    }
    fn operate_change_word(&mut self, key: &str, count: usize) {
        let origin = self.cursor;
        if self.text.get(origin).is_none_or(|c| c.is_whitespace()) {
            self.operate_forward_word('c', key, count);
            return;
        }
        let big = key == "W";
        let Some(class) = self.word_class_at(origin, big) else {
            self.chosen_register = None;
            return;
        };
        let mut end = origin;
        let line_end = self.line_end(origin);
        while end < line_end && self.word_class_at(end, big) == Some(class) {
            end += 1;
        }
        let Some(mut last) = self.grapheme_floor(end.saturating_sub(1)) else {
            self.chosen_register = None;
            return;
        };
        for _ in 1..count {
            self.cursor = last;
            let Some(mut next) = self.motion(if big { "E" } else { "e" }, 1) else {
                self.cursor = origin;
                self.chosen_register = None;
                return;
            };
            if next == self.line_end(next) && next > self.line_start(next) {
                let Some(previous) = self.grapheme_move(next, 1, false, true) else {
                    self.cursor = origin;
                    self.chosen_register = None;
                    return;
                };
                next = previous;
            }
            if next == last {
                break;
            }
            last = next;
        }
        self.cursor = origin;
        let Some(end) = self.grapheme_move(last, 1, true, true) else {
            self.chosen_register = None;
            return;
        };
        self.operate('c', origin..end, false);
    }
    fn grapheme_column(&self, p: usize) -> Option<usize> {
        self.with_grapheme_index(p, |index, offset, start, end| {
            index.column(
                offset,
                &self.text[start..end],
                self.indent_style.tabstop,
                self.mode != Mode::Insert,
            )
        })
    }
    fn grapheme_offset_at_column(&self, start: usize, column: usize) -> Option<usize> {
        self.with_grapheme_index(start, |index, _, start, end| {
            Some(
                start
                    + index.offset_at_column(
                        column,
                        &self.text[start..end],
                        self.indent_style.tabstop,
                    ),
            )
        })
    }
    fn with_grapheme_index<T>(
        &self,
        p: usize,
        f: impl FnOnce(&grapheme::LineIndex, usize, usize, usize) -> Option<T>,
    ) -> Option<T> {
        let start = self.line_start(p);
        let end = self.line_end(p);
        if p > end {
            return None;
        }
        let mut cache = self.grapheme_cache.borrow_mut();
        if !cache.as_ref().is_some_and(|c| {
            c.version == self.version
                && c.start == start
                && c.end == end
                && c.tabstop == self.indent_style.tabstop
        }) {
            let index = grapheme::LineIndex::new(&self.text[start..end], self.indent_style.tabstop);
            *cache = Some(grapheme::Cache {
                version: self.version,
                start,
                end,
                tabstop: self.indent_style.tabstop,
                index,
            });
        }
        f(cache.as_ref()?.index.as_ref()?, p - start, start, end)
    }
    pub fn goto(&mut self, row: usize, column: usize) {
        self.desired_column = None;
        let start = self.line_number(row + 1);
        self.cursor = (start + column).min(self.line_end(start));
        self.clamp();
    }
    pub fn visible_lines(&self, top: usize, count: usize) -> Vec<Range<usize>> {
        let first = self.visible_row(top);
        (first..first.saturating_add(count))
            .map(|row| self.source_row(row))
            .scan(None, |previous, row| {
                if *previous == Some(row) {
                    return None;
                }
                *previous = Some(row);
                Some(row)
            })
            .map(|row| {
                let start = self.line_starts[row];
                start..self.line_end(start)
            })
            .collect()
    }
    fn checkpoint(&mut self) {
        // Bound retained history by character count as well as revision count.
        while self.undo.len() >= 100
            || self.undo.iter().map(|r| r.text.len()).sum::<usize>() + self.text.len() > 4_000_000
        {
            if self.undo.is_empty() {
                break;
            }
            self.undo.remove(0);
        }
        self.undo.push(Revision {
            text: self.text.clone(),
            cursor: self.cursor,
            redo_cursor_line: None,
            visual: self.last_visual,
            visual_generation: self.visual_generation,
        });
        self.redo.clear();
    }
    fn changed(&mut self) {
        self.retain_folds_after_change();
        self.cancel_search();
        self.dismiss_search_preview(false);
        self.cancel_input();
        self.snippet = None;
        self.desired_column = None;
        self.reindex_csv();
        let before = self.version;
        self.version = self.version.wrapping_add(1);
        if let Some(journal) = &mut self.jump_changes {
            journal.changed(before, self.version);
        }
        self.line_starts = std::iter::once(0)
            .chain(
                self.text
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| (*c == '\n').then_some(i + 1)),
            )
            .collect();
        self.modified = !self.text.iter().copied().eq(self.saved.chars());
        self.clamp();
    }
    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.text.len());
        if matches!(self.mode, Mode::Normal | Mode::Visual | Mode::VisualLine)
            && self.cursor > self.line_start(self.cursor)
            && !self.folded_visual_eol()
            && (self.cursor == self.text.len() || self.text.get(self.cursor) == Some(&'\n'))
        {
            self.cursor = self
                .grapheme_move(self.cursor, 1, false, true)
                .unwrap_or(self.cursor - 1);
        }
        if matches!(self.mode, Mode::Normal | Mode::Visual | Mode::VisualLine) {
            self.cursor = self.grapheme_floor(self.cursor).unwrap_or(self.cursor);
        }
    }
    pub fn selection(&self) -> Option<Range<usize>> {
        let range = if self.mode == Mode::Visual {
            let last = self.anchor.max(self.cursor);
            let end = if last == self.line_end(last) {
                (last + 1).min(self.text.len())
            } else {
                self.grapheme_move(last, 1, true, true)?
            };
            self.anchor.min(self.cursor)..end
        } else if self.mode == Mode::VisualLine {
            self.line_start(self.anchor.min(self.cursor))
                ..(self.line_end(self.anchor.max(self.cursor)) + 1).min(self.text.len())
        } else {
            return None;
        };
        Some(self.expand_folded_selection(range))
    }
    fn insert_base(&mut self, text: &str) {
        self.insert_base_tracked(text, false, true);
    }
    fn insert_base_tracked(&mut self, text: &str, before_line: bool, exact_newlines: bool) {
        self.automatic_indent = None;
        if self.mode == Mode::Command || self.mode == Mode::Search {
            if self.prompt_clipboard_pending.is_some() {
                return;
            }
            if self.consume_prompt_register_text(text) {
                return;
            }
            if self.command.chars().count() + text.chars().count() < 1024 {
                self.prompt_history = None;
                self.insert_prompt_text(text);
                if self.mode == Mode::Search {
                    self.refresh_search_preview(None);
                }
            }
            return;
        }
        if self.mode != Mode::Insert {
            return;
        }
        let chars = text.replace("\r\n", "\n").chars().collect::<Vec<_>>();
        if self.text.len() + chars.len() > 2_000_000 {
            self.message = "Límite de edición: 2 millones de caracteres".into();
            return;
        }
        let n = chars.len();
        let added = chars.iter().filter(|c| **c == '\n').count();
        let row = self.source_line_at(self.cursor);
        if added == 0 || exact_newlines {
            self.prepare_jump_change(JumpImpact::Lines {
                start: row + usize::from(added > 0 && !before_line),
                removed: 0,
                added,
            });
        }
        self.track_visual_edit(self.cursor..self.cursor, added, before_line);
        self.text.splice(self.cursor..self.cursor, chars);
        self.cursor += n;
        self.changed();
    }
    fn motion(&mut self, key: &str, n: usize) -> Option<usize> {
        let mut p = self.cursor;
        let vertical = matches!(key, "j" | "k" | "<Down>" | "<Up>");
        let col = if vertical {
            let cached = self
                .desired_column
                .filter(|(expected, _)| *expected == p)
                .map(|(_, col)| col);
            let Some(column) = cached.or_else(|| self.grapheme_column(p)) else {
                self.message = "Movimiento Unicode fuera del límite; texto conservado".into();
                return None;
            };
            column
        } else {
            p - self.line_start(p)
        };
        match key {
            "h" | "<Left>" | "l" | "<Right>" | "<Space>" => {
                let right = matches!(key, "l" | "<Right>" | "<Space>");
                let Some(target) = self.grapheme_move(p, n, right, self.mode == Mode::Insert)
                else {
                    self.message = "Movimiento Unicode fuera del límite o cursor dentro de una secuencia; texto conservado".into();
                    return None;
                };
                p = target;
            }
            "j" | "<Down>" | "k" | "<Up>" => {
                let row = self
                    .line_starts
                    .partition_point(|start| *start <= p)
                    .saturating_sub(1);
                let visible = self.visible_row(row);
                let mut target = if matches!(key, "j" | "<Down>") {
                    visible.saturating_add(n)
                } else {
                    visible.saturating_sub(n)
                };
                if matches!(self.mode, Mode::Normal | Mode::Visual | Mode::VisualLine) {
                    let last = self.source_line_at(self.text.len().saturating_sub(1));
                    target = target.min(self.visible_row(last));
                }
                let start = self.line_starts[self.source_row(target)];
                let Some(boundary) = self.grapheme_offset_at_column(start, col) else {
                    self.message = "Movimiento Unicode fuera del límite; texto conservado".into();
                    return None;
                };
                p = boundary;
            }
            "0" => p = self.line_start(p),
            "^" => {
                p = self.line_start(p);
                while self
                    .text
                    .get(p)
                    .is_some_and(|c| *c != '\n' && c.is_whitespace())
                {
                    p += 1;
                }
            }
            "$" => {
                let row = self.visible_row(self.source_line_at(p));
                let last = self.visible_row(self.source_line_at(self.text.len().saturating_sub(1)));
                let target = row.saturating_add(n.saturating_sub(1));
                if target > last {
                    self.desired_column = Some((self.cursor, usize::MAX));
                    return None;
                }
                let start = self.line_starts[self.source_row(target)];
                let end = self.line_end(start);
                p = if end > start {
                    let Some(last) = self.grapheme_move(end, 1, false, true) else {
                        self.message =
                            "Movimiento Unicode fuera del límite; texto conservado".into();
                        return None;
                    };
                    last
                } else {
                    end
                };
            }
            "gg" => p = 0,
            "G" => p = self.line_start(self.text.len()),
            "%" => p = self.match_bracket_unambiguous()?,
            "w" | "b" | "e" | "W" | "B" | "E" => {
                let logical_end = self
                    .text
                    .len()
                    .saturating_sub(usize::from(self.text.last() == Some(&'\n')));
                let big = matches!(key, "W" | "B" | "E");
                let key = match key {
                    "W" => "w",
                    "B" => "b",
                    "E" => "e",
                    k => k,
                };
                for _ in 0..n {
                    if key == "b" {
                        p = p.saturating_sub(1);
                        while p > 0 && self.word_class_at(p, big)? == 0 && !self.empty_line_at(p) {
                            p -= 1;
                        }
                        while p > 0
                            && !self.empty_line_at(p)
                            && self.word_class_at(p - 1, big)? == self.word_class_at(p, big)?
                        {
                            p -= 1;
                        }
                    } else if key == "w" {
                        if p < self.text.len() {
                            let c = self.word_class_at(p, big)?;
                            if self.empty_line_at(p) {
                                p += 1;
                            } else {
                                while p < self.text.len()
                                    && self.word_class_at(p, big)? == c
                                    && !(c == 0 && self.empty_line_at(p))
                                {
                                    p += 1;
                                }
                            }
                            while p < self.text.len()
                                && self.word_class_at(p, big)? == 0
                                && !self.empty_line_at(p)
                            {
                                p += 1;
                            }
                        }
                    } else {
                        p = if p < self.text.len() && self.text[p] != '\n' {
                            self.grapheme_move(p, 1, true, true)?
                        } else {
                            (p + 1).min(self.text.len())
                        };
                        while p < self.text.len() && self.word_class_at(p, big)? == 0 {
                            p += 1;
                        }
                        while p + 1 < self.text.len()
                            && self.word_class_at(p + 1, big)? == self.word_class_at(p, big)?
                        {
                            p += 1;
                        }
                    }
                    p = self.grapheme_floor(p.min(logical_end))?;
                }
            }
            _ => return None,
        }
        p = p.min(self.text.len());
        if vertical || key == "$" {
            let expected = if matches!(self.mode, Mode::Normal | Mode::Visual | Mode::VisualLine)
                && p > self.line_start(p)
                && (p == self.text.len() || self.text.get(p) == Some(&'\n'))
            {
                p - 1
            } else {
                p
            };
            self.desired_column = Some((expected, if key == "$" { usize::MAX } else { col }));
        } else {
            self.desired_column = None;
        }
        Some(p)
    }

    /// Conservative bracket-only % motion. Complex quoting, escapes and comment
    /// delimiters are rejected until their Neovim semantics are modeled.
    fn match_bracket_unambiguous(&self) -> Option<usize> {
        let line_start = self.line_start(self.cursor);
        let line_end = self.line_end(self.cursor);
        let is_bracket = |ch| matches!(ch, '(' | ')' | '[' | ']' | '{' | '}');
        let start = (self.cursor..line_end).find(|&i| is_bracket(self.text[i]))?;
        let prefix = &self.text[line_start..start];
        let mut quote = None;
        for (i, ch) in prefix.iter().copied().enumerate() {
            if ch == '\\' {
                return None;
            }
            if let Some(delimiter) = quote {
                if ch == delimiter {
                    quote = None;
                } else if is_bracket(ch) {
                    return None;
                }
            } else if matches!(ch, '\'' | '"') {
                quote = Some(ch);
            } else if ch == '#'
                || (ch == '/'
                    && prefix
                        .get(i + 1)
                        .is_some_and(|next| matches!(next, '/' | '*')))
                || (ch == '*' && prefix.get(i + 1) == Some(&'/'))
            {
                return None;
            }
        }
        if quote.is_some()
            || self.text[..line_start]
                .windows(2)
                .any(|pair| pair == ['/', '*'])
        {
            return None;
        }
        let (open, close, forward) = match self.text[start] {
            '(' => ('(', ')', true),
            ')' => ('(', ')', false),
            '[' => ('[', ']', true),
            ']' => ('[', ']', false),
            '{' => ('{', '}', true),
            '}' => ('{', '}', false),
            _ => return None,
        };
        let mut index = start;
        let mut depth = 0usize;
        loop {
            let ch = self.text[index];
            if matches!(ch, '\'' | '"') {
                loop {
                    index = if forward {
                        index.checked_add(1).filter(|i| *i < self.text.len())?
                    } else {
                        index.checked_sub(1)?
                    };
                    let inside = self.text[index];
                    if inside == ch {
                        break;
                    }
                    if inside == '\\' || inside == '\n' || is_bracket(inside) {
                        return None;
                    }
                }
            } else if matches!(ch, '\\' | '#')
                || (ch == '/'
                    && self
                        .text
                        .get(index + 1)
                        .is_some_and(|next| matches!(next, '/' | '*')))
                || (ch == '*' && self.text.get(index + 1) == Some(&'/'))
            {
                return None;
            } else if ch == if forward { open } else { close } {
                depth += 1;
            } else if ch == if forward { close } else { open } {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            index = if forward {
                index.checked_add(1).filter(|i| *i < self.text.len())?
            } else {
                index.checked_sub(1)?
            };
        }
    }
    fn percentage_line(&self, percent: usize) -> Option<usize> {
        if !(1..=100).contains(&percent) {
            return None;
        }
        let lines = self.source_line_at(self.text.len().saturating_sub(1)) + 1;
        Some(lines.saturating_mul(percent).saturating_add(99) / 100)
    }
    fn operate(&mut self, op: char, mut range: Range<usize>, line: bool) {
        let visual = self.selection().is_some();
        let column = self.cursor - self.line_start(self.cursor);
        if matches!(op, 'u' | 'U' | '~') {
            let cursor = if line {
                let column = self.cursor - self.line_start(self.cursor);
                let indent = self.text[range.start..self.line_end(range.start)]
                    .iter()
                    .take_while(|c| c.is_whitespace())
                    .count();
                range.start + column.min(indent)
            } else {
                range.start
            };
            self.case_range(range, op, cursor);
            return;
        }
        self.register = self.text[range.clone()].iter().collect();
        if line && !self.register.ends_with('\n') {
            self.register.push('\n');
        }
        self.line_register = line;
        self.capture_register(op);
        self.mode = Mode::Normal;
        if op != 'y' {
            self.checkpoint();
            let change_start = range.start;
            if op == 'c' && line {
                // Keep the first line's indentation, just like autoindent cc.
                range.start += self.text[range.start..self.line_end(range.start)]
                    .iter()
                    .take_while(|c| matches!(c, ' ' | '\t'))
                    .count();
            }
            if op == 'c'
                && line
                && range.end > range.start
                && self.text.get(range.end - 1) == Some(&'\n')
            {
                range.end -= 1;
            }
            if op == 'd'
                && line
                && range.start > 0
                && range.end == self.text.len()
                && self.text.last() != Some(&'\n')
            {
                range.start -= 1;
            }
            let removed = self.text[range.clone()]
                .iter()
                .filter(|ch| **ch == '\n')
                .count();
            let first = self.source_line_at(range.start);
            let last = self.source_line_at(range.end.saturating_sub(1));
            let unfolded = self.closed_fold_rows(first).is_none()
                && self.closed_fold_rows(last).is_none()
                && last >= first
                && self
                    .visible_row(last)
                    .saturating_sub(self.visible_row(first))
                    == last - first;
            let simple_lines = line && !visual && unfolded && range.end < self.text.len();
            if removed == 0
                || (simple_lines
                    && (op == 'c' || (op == 'd' && range.start == self.line_start(range.start))))
            {
                self.prepare_jump_change(JumpImpact::Lines {
                    start: first + usize::from(removed > 0 && op == 'c'),
                    removed,
                    added: 0,
                });
            }
            self.track_visual_edit(range.clone(), 0, false);
            self.text.drain(range.clone());
            self.cursor = range.start.min(self.text.len());
            if op == 'c' {
                self.mode = Mode::Insert;
            }
            self.changed();
            if op == 'd' && line && !visual {
                let start = self.line_start(self.cursor);
                self.cursor = start + column.min(self.line_end(start) - start);
                self.clamp();
            }
            if op == 'c' {
                self.mode = Mode::Insert;
                if line && self.cursor > change_start {
                    self.automatic_indent = Some((self.version, change_start..self.cursor));
                }
            }
        } else {
            if !line || visual {
                self.cursor = range.start.min(self.text.len());
            }
            self.clamp();
        }
    }
    /// Apply a linewise motion to all source lines represented by closed folds.
    pub(super) fn operate_line_motion(&mut self, op: char, target: usize) {
        let original_column = self.tab_column(self.cursor);
        let current_row = self.source_line_at(self.cursor);
        let target_row = self.source_line_at(target);
        let first = self.folded_first_row(current_row.min(target_row));
        let last = self.folded_last_row(current_row.max(target_row));
        let start = self.line_starts[first];
        let end = self
            .line_end(self.line_starts[last])
            .saturating_add(1)
            .min(self.text.len());
        if target_row < current_row {
            let column = self.tab_column(self.cursor);
            self.cursor = if op == 'y'
                && self
                    .folded_line_count(self.line_starts[target_row])
                    .is_some()
            {
                self.line_starts[target_row]
            } else {
                self.offset_at_tab_column(self.line_starts[target_row], column)
            };
            self.clamp();
        }
        self.operate(op, start..end, true);
        if op == 'd' && self.cursor == self.text.len() && self.text.last() == Some(&'\n') {
            let last = self.line_start(self.cursor.saturating_sub(1));
            self.cursor = self.offset_at_tab_column(last, original_column);
            self.clamp();
        }
    }
    fn key_base(&mut self, key: &str) -> Effect {
        if let Some(effect) = self.block_key(key) {
            return effect;
        }
        if let Some(effect) = self.prompt_register_key(key) {
            return effect;
        }
        if key == "<Esc>" || (key == "<C-c>" && matches!(self.mode, Mode::Command | Mode::Search)) {
            if matches!(self.mode, Mode::Command | Mode::Search) {
                if key == "<Esc>" {
                    self.registers
                        .record_prompt_history(self.mode == Mode::Search, &self.command);
                }
                self.prompt_history = None;
            }
            self.dismiss_search_preview(true);
            if self.mode == Mode::Insert {
                self.clear_automatic_indent();
                self.cursor = self
                    .grapheme_move(self.cursor, 1, false, true)
                    .unwrap_or(self.cursor);
            }
            self.mode = Mode::Normal;
            self.pending.clear();
            self.count = 0;
            self.command.clear();
            self.prompt_cursor = 0;
            self.prompt_register_pending = false;
            self.prompt_clipboard_pending = None;
            self.clamp();
            return Effect::None;
        }
        if let Some(effect) = self.search_expansion_key(key) {
            return effect;
        }
        if let Some(effect) = self.prompt_history_key(key) {
            return effect;
        }
        if let Some(effect) = self.prompt_edit_key(key) {
            return effect;
        }
        if matches!(self.mode, Mode::Command | Mode::Search) {
            if self.mode == Mode::Search && matches!(key, "<C-g>" | "<C-t>") {
                return self.refresh_search_preview(Some(key == "<C-t>"));
            } else if matches!(key, "<Enter>" | "<C-j>" | "<C-m>") {
                self.registers
                    .record_prompt_history(self.mode == Mode::Search, &self.command);
                self.prompt_history = None;
                let command = std::mem::take(&mut self.command);
                self.prompt_cursor = 0;
                let search = self.mode == Mode::Search;
                self.mode = Mode::Normal;
                if search {
                    return self.submit_search(command);
                } else {
                    return match command.trim() {
                        c if !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit()) => {
                            let number = c.parse::<usize>().unwrap_or(usize::MAX);
                            self.cursor = self.line_number(number.max(1));
                            self.clamp();
                            self.desired_column = None;
                            Effect::None
                        }
                        "bn" | "bnext" => Effect::Buffer("buffers.next".into()),
                        "bf" | "bfi" | "bfir" | "bfirs" | "bfirst" | "br" | "bre" | "brew"
                        | "brewi" | "brewin" | "brewind" => Effect::Buffer("buffers.first".into()),
                        "bl" | "bla" | "blas" | "blast" => Effect::Buffer("buffers.last".into()),
                        "clearj" | "clearju" | "clearjum" | "clearjump" | "clearjumps" => {
                            Effect::ClearJumps
                        }
                        "bp" | "bprevious" | "bprev" => Effect::Buffer("buffers.prev".into()),
                        "ls" | "buffers" | "files" => Effect::BufferList(false),
                        "ls!" | "buffers!" | "files!" => Effect::BufferList(true),
                        "bd" | "bdel" | "bdele" | "bdelet" | "bdelete" => {
                            Effect::VimBufferDelete(false)
                        }
                        "bd!" | "bdel!" | "bdele!" | "bdelet!" | "bdelete!" => {
                            Effect::VimBufferDelete(true)
                        }
                        c if targeted_buffer_delete(c).is_some() => {
                            let (force, target) = targeted_buffer_delete(c).unwrap();
                            Effect::VimBufferDeleteTarget(force, target)
                        }
                        c if numbered_buffer_command(c).is_some() => Effect::Buffer(format!(
                            "buffers.number.{}",
                            numbered_buffer_command(c).unwrap()
                        )),
                        c if c.strip_suffix('#').is_some_and(|prefix| {
                            matches!(
                                prefix.trim(),
                                "b" | "bu" | "buf" | "buff" | "buffe" | "buffer"
                            )
                        }) =>
                        {
                            Effect::Buffer("buffers.alternate".into())
                        }
                        "enew" => Effect::NewFile,
                        "terminal" | "term" => Effect::NewTerminal,
                        "sp" | "split" => Effect::Split(false),
                        "vs" | "vsplit" => Effect::Split(true),
                        "new" => Effect::SplitNew(false),
                        "vne" | "vnew" => Effect::SplitNew(true),
                        "on" | "only" => Effect::Only,
                        "clo" | "close" => Effect::CloseWindow,
                        c if counted_split_command(c).is_some() => {
                            match counted_split_command(c).unwrap() {
                                Ok((key, count, Some(path))) => {
                                    Effect::SplitPathSized(matches!(key, 'v' | 'N'), count, path)
                                }
                                Ok((key @ ('n' | 'N'), count, None)) => {
                                    Effect::SplitNewSized(key == 'N', count)
                                }
                                Ok((key, count, None)) => Effect::WindowCommand(key, count),
                                Err(message) => {
                                    self.message = message.into();
                                    Effect::None
                                }
                            }
                        }
                        c if window_command(c).is_some() => match window_command(c).unwrap() {
                            Ok((key, count)) => Effect::WindowCommand(key, count),
                            Err(message) => {
                                self.message = message.into();
                                Effect::None
                            }
                        },
                        c if c.starts_with("sp ") => Effect::SplitPath(false, c[3..].trim().into()),
                        c if c.starts_with("split ") => {
                            Effect::SplitPath(false, c[6..].trim().into())
                        }
                        c if c.starts_with("vs ") => Effect::SplitPath(true, c[3..].trim().into()),
                        c if c.starts_with("vsplit ") => {
                            Effect::SplitPath(true, c[7..].trim().into())
                        }
                        c if c.starts_with("new ") => {
                            Effect::SplitPath(false, c[4..].trim().into())
                        }
                        c if c.starts_with("vnew ") => {
                            Effect::SplitPath(true, c[5..].trim().into())
                        }
                        c if c
                            .strip_prefix("e ")
                            .or_else(|| c.strip_prefix("edit "))
                            .is_some_and(|argument| argument.trim() == "#") =>
                        {
                            Effect::Buffer("buffers.alternate.edit".into())
                        }
                        c if c.starts_with("e ") => Effect::OpenPath(c[2..].trim().into()),
                        c if c.starts_with("edit ") => Effect::OpenPath(c[5..].trim().into()),
                        c if c.starts_with("w ") => Effect::WriteCopy(c[2..].trim().into()),
                        c if c.starts_with("write ") => Effect::WriteCopy(c[6..].trim().into()),
                        c if c.starts_with("wq ") => Effect::Write(c[3..].trim().into(), true),
                        c if c.starts_with("saveas ") => Effect::SaveAs(c[7..].trim().into()),
                        "w" | "write" => Effect::Save,
                        "wq" | "x" => Effect::SaveClose,
                        "q" | "quit" => Effect::Close(false),
                        "q!" | "quit!" => Effect::Close(true),
                        "noh" | "nohl" | "nohlsearch" => {
                            self.registers.set_search_highlight(false);
                            Effect::None
                        }
                        "format" => Effect::Format,
                        "TexPrepare" => Effect::TexPrepare,
                        "OpenExternal" => Effect::OpenExternal,
                        "LspStart" => Effect::Lsp("start".into()),
                        "LspStop" => Effect::Lsp("stop".into()),
                        "MarkdownPreview" => Effect::Markdown("start".into()),
                        "MarkdownPreviewStop" => Effect::Markdown("stop".into()),
                        "MarkdownPreviewToggle" => Effect::Markdown("toggle".into()),
                        c if c.starts_with("PRView ") => {
                            Effect::Git(format!("pr {}", c[7..].trim()))
                        }
                        "PRVideo" => Effect::Video(String::new()),
                        c if c.starts_with("PRVideo ") => Effect::Video(c[8..].trim().into()),
                        "GitStatus" => Effect::Git("status".into()),
                        "GitLog" => Effect::Git("log".into()),
                        "GitProjectDiff" => Effect::Git("project_diff".into()),
                        "GitDiff" => Effect::Git("diff".into()),
                        "GitBlame" => Effect::Git("blame".into()),
                        "TmuxReconnect" => Effect::ReconnectTerminal,
                        "TerminalRecover" => Effect::RecoverTerminals,
                        "Diagnostics" => Effect::Diagnostics(false),
                        "BufferDiagnostics" => Effect::Diagnostics(true),
                        "Metrics" => Effect::Metrics(true),
                        "MetricsStop" => Effect::Metrics(false),
                        "MetricsWrite" => Effect::MetricsExport,
                        "SyntaxToggle" => Effect::SyntaxToggle,
                        "set wrap" | "setlocal wrap" => Effect::Wrap(Some(true)),
                        "set nowrap" | "setlocal nowrap" => Effect::Wrap(Some(false)),
                        "set wrap!" | "set invwrap" | "setlocal wrap!" | "setlocal invwrap" => {
                            Effect::Wrap(None)
                        }
                        "Font" => Effect::Typography(String::new()),
                        c if c.starts_with("Font ") => Effect::Typography(c[5..].trim().into()),
                        c if c.starts_with("FontSize ") => Effect::Typography(c[9..].trim().into()),
                        "colorscheme" => Effect::Theme(String::new()),
                        c if c.starts_with("colorscheme ") => Effect::Theme(c[12..].trim().into()),
                        c if c == "AgentsLocal"
                            || c.starts_with("AgentsRemote ")
                            || c.starts_with("AgentsDisconnect ") =>
                        {
                            Effect::Agents(c.into())
                        }
                        _ => {
                            self.message = format!("Comando pendiente: {command}");
                            Effect::None
                        }
                    };
                }
            } else if !key.starts_with('<') {
                self.insert(key);
            }
            return Effect::None;
        }
        if self.mode == Mode::Insert {
            match key {
                "<Enter>" | "<C-j>" | "<C-m>" => self.indented_newline(),
                "<Tab>" => self.indent_tab(),
                "<C-t>" | "<C-d>" => self.shift_insert_indent(key == "<C-t>"),
                "<C-w>" => self.delete_insert_word(),
                "<BS>" | "<C-h>" => {
                    if self.indent_backspace() {
                        return Effect::None;
                    } else if self.cursor > 0 {
                        let start = if self.cursor == self.line_start(self.cursor) {
                            self.cursor - 1
                        } else if let Some(start) = self.grapheme_move(self.cursor, 1, false, true)
                        {
                            start
                        } else {
                            self.message =
                                "Borrado Unicode fuera del límite; texto conservado".into();
                            return Effect::None;
                        };
                        let left = self.text[self.cursor - 1];
                        let right = self.text.get(self.cursor).copied();
                        if !self.text[start..self.cursor].contains(&'\n') {
                            self.prepare_jump_change(JumpImpact::Lines {
                                start: self.source_line_at(start),
                                removed: 0,
                                added: 0,
                            });
                        }
                        if matches!(
                            (left, right),
                            ('(', Some(')'))
                                | ('[', Some(']'))
                                | ('{', Some('}'))
                                | ('\'', Some('\''))
                                | ('"', Some('"'))
                                | ('`', Some('`'))
                        ) {
                            self.track_visual_edit(self.cursor..self.cursor + 1, 0, false);
                            self.text.remove(self.cursor);
                        }
                        self.track_visual_edit(start..self.cursor, 0, false);
                        self.text.drain(start..self.cursor);
                        self.cursor = start;
                        self.changed();
                    }
                }
                "<Delete>" => {
                    if self.cursor < self.text.len() {
                        let end = if self.text[self.cursor] == '\n' {
                            self.cursor + 1
                        } else if let Some(end) = self.grapheme_move(self.cursor, 1, true, true) {
                            end
                        } else {
                            self.message =
                                "Borrado Unicode fuera del límite; texto conservado".into();
                            return Effect::None;
                        };
                        if !self.text[self.cursor..end].contains(&'\n') {
                            self.prepare_jump_change(JumpImpact::Lines {
                                start: self.source_line_at(self.cursor),
                                removed: 0,
                                added: 0,
                            });
                        }
                        self.track_visual_edit(self.cursor..end, 0, false);
                        self.text.drain(self.cursor..end);
                        self.changed();
                    }
                }
                "<Left>" | "<Right>" | "<Up>" | "<Down>" => {
                    if let Some(p) = self.motion(key, 1) {
                        self.cursor = p;
                    }
                }
                _ => {}
            }
            return Effect::None;
        }
        if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() && (key != "0" || self.count > 0) {
            self.count = (self.count * 10 + key.parse::<usize>().unwrap()).min(9999);
            return Effect::None;
        }
        let explicit_count = self.count > 0;
        let n = std::mem::take(&mut self.count).max(1);
        if let Some(effect) = self.scroll_command(key, n, explicit_count) {
            return effect;
        }
        if key == "%" && explicit_count && self.pending.is_empty() {
            if let Some(line) = self.percentage_line(n) {
                self.record_history_jump();
                self.goto_line_column(line);
            }
            return Effect::None;
        }
        if key == "G" && self.pending.is_empty() {
            self.record_history_jump();
            self.goto_line_column(if explicit_count { n } else { usize::MAX });
            return Effect::None;
        }
        if let Some(selection) = self.selection()
            && matches!(key, "d" | "c" | "y")
        {
            self.operate(
                key.chars().next().unwrap(),
                selection,
                self.mode == Mode::VisualLine,
            );
            return Effect::None;
        }
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            let op = pending.chars().next().unwrap();
            if op == 'g' && key == "v" {
                self.reselect_visual();
                return Effect::None;
            }
            if op == 'g' && matches!(key, "*" | "#") {
                let total = pending[1..]
                    .parse::<usize>()
                    .unwrap_or(1)
                    .saturating_mul(n)
                    .min(9999);
                return self.search_word(key == "#", total, false);
            }
            if matches!(op, '>' | '<') {
                let total = pending[1..]
                    .parse::<usize>()
                    .unwrap_or(1)
                    .saturating_mul(n)
                    .min(9999);
                if key == "g" {
                    self.awaiting = Some(Await::ShiftG(op, total));
                    return Effect::None;
                }
                if key == "G" {
                    let target = if self.operator_count_explicit || explicit_count {
                        self.line_number(total)
                    } else {
                        self.line_start(self.text.len().saturating_sub(1))
                    };
                    self.shift_to_position(op, target);
                    return Effect::None;
                }
                let start = self.cursor;
                let end = if key == op.to_string() {
                    self.motion("j", total.saturating_sub(1)).unwrap_or(start)
                } else if let Some(p) = self.motion(key, total) {
                    p
                } else {
                    self.message = "Movimiento de indentación pendiente".into();
                    return Effect::None;
                };
                if (key == op.to_string() && total > 1
                    || matches!(key, "j" | "k" | "<Down>" | "<Up>"))
                    && self.source_line_at(start) == self.source_line_at(end)
                {
                    return Effect::None;
                }
                self.shift_to_position(op, end);
                return Effect::None;
            }
            if op == 'g' && key == "g" {
                self.record_history_jump();
                self.goto_line_column(pending[1..].parse().unwrap_or(1));
                return Effect::None;
            }
            if op == 'g' && matches!(key, "u" | "U" | "~") {
                let total = pending[1..]
                    .parse::<usize>()
                    .unwrap_or(1)
                    .saturating_mul(n)
                    .min(9999);
                self.pending = format!("{key}{total}");
                return Effect::None;
            }
            if matches!(op, 'd' | 'c' | 'y' | 'u' | 'U' | '~') {
                let total = pending[1..]
                    .parse::<usize>()
                    .unwrap_or(1)
                    .saturating_mul(n)
                    .min(9999);
                if key == op.to_string() && matches!(op, 'u' | 'U' | '~') {
                    self.case_current_lines(op, total);
                } else if key == op.to_string() {
                    let row = self.source_line_at(self.cursor);
                    let start = self.line_starts[self.folded_first_row(row)];
                    let mut end = start;
                    for _ in 0..total {
                        let row = self
                            .line_starts
                            .partition_point(|start| *start <= end)
                            .saturating_sub(1);
                        let last = self.folded_last_row(row);
                        end = (self.line_end(self.line_starts[last]) + 1).min(self.text.len());
                    }
                    self.operate(op, start..end, true);
                } else if matches!(op, 'u' | 'U' | '~') && key == "g" {
                    self.awaiting = Some(Await::CaseG(op, total));
                } else if matches!(op, 'u' | 'U' | '~') && key == "G" {
                    let target = if self.operator_count_explicit || explicit_count {
                        self.line_number(total)
                    } else {
                        self.line_start(self.text.len())
                    };
                    self.case_to_line(op, target);
                } else if matches!(op, 'd' | 'c' | 'y') && matches!(key, "{" | "}") {
                    self.operate_paragraph(op, key == "}", total);
                } else if matches!(key, "i" | "a") {
                    self.awaiting = Some(Await::Object(op, total, key == "a"));
                } else if matches!(key, "f" | "F" | "t" | "T") {
                    self.awaiting = Some(Await::Find(key.chars().next().unwrap(), total, Some(op)));
                } else if matches!(key, ";" | ",") {
                    if let Some(target) = self.repeat_find_position(key, total) {
                        let start = self.cursor.min(target);
                        let end = if target >= self.cursor {
                            let Some(end) = self.grapheme_move(target, 1, true, true) else {
                                self.chosen_register = None;
                                return Effect::None;
                            };
                            end
                        } else {
                            self.cursor
                        };
                        self.operate(op, start..end, false);
                    } else {
                        self.chosen_register = None;
                    }
                } else if matches!(op, 'd' | 'c' | 'y') && key == "G" {
                    self.record_history_jump();
                    let target = if self.operator_count_explicit || explicit_count {
                        self.line_number(total)
                            .min(self.line_start(self.text.len().saturating_sub(1)))
                    } else {
                        self.line_start(self.text.len().saturating_sub(1))
                    };
                    self.operate_line_motion(op, target);
                } else if matches!(op, 'd' | 'c' | 'y')
                    && key == "%"
                    && (self.operator_count_explicit || explicit_count)
                {
                    if let Some(line) = self.percentage_line(total) {
                        self.record_history_jump();
                        self.operate_line_motion(op, self.line_number(line));
                    } else {
                        self.chosen_register = None;
                    }
                } else if matches!(op, 'd' | 'c' | 'y')
                    && matches!(key, "j" | "k" | "<Down>" | "<Up>")
                {
                    if let Some(target) = self.motion(key, total)
                        && self.source_line_at(target) != self.source_line_at(self.cursor)
                    {
                        self.operate_line_motion(op, target);
                    } else {
                        self.chosen_register = None;
                    }
                } else if matches!(op, 'd' | 'y') && matches!(key, "w" | "W") {
                    self.operate_forward_word(op, key, total);
                } else if op == 'c' && matches!(key, "w" | "W") {
                    self.operate_change_word(key, total);
                } else if let Some(mut end) = self.motion(key, total) {
                    if key == "%" {
                        self.record_history_jump();
                    }
                    if matches!(op, 'u' | 'U' | '~')
                        && matches!(key, "j" | "k" | "<Up>" | "<Down>" | "G")
                    {
                        self.case_to_line(op, end);
                        return Effect::None;
                    }
                    if matches!(op, 'd' | 'c' | 'y') && key == "$" {
                        let origin_row = self.source_line_at(self.cursor);
                        let origin_start = self.line_starts[origin_row];
                        let origin_fold = self.folded_line_count(origin_start).is_some();
                        let target_row = self.source_line_at(end);
                        if origin_fold {
                            self.cursor = origin_start;
                        }
                        if op == 'd' && origin_fold && target_row == origin_row {
                            self.operate_line_motion(op, end);
                            return Effect::None;
                        }
                        if self
                            .folded_line_count(self.line_starts[target_row])
                            .is_some()
                        {
                            let last = self.folded_last_row(target_row);
                            let start = self.line_starts[last];
                            let line_end = self.line_end(start);
                            end = if line_end > start {
                                let Some(last) = self.grapheme_move(line_end, 1, false, true)
                                else {
                                    self.chosen_register = None;
                                    self.message =
                                        "Operación Unicode fuera del límite; texto conservado"
                                            .into();
                                    return Effect::None;
                                };
                                last
                            } else {
                                line_end
                            };
                        }
                    }
                    if op == 'd'
                        && key == "$"
                        && self.cursor == self.line_start(self.cursor)
                        && self.source_line_at(end) > self.source_line_at(self.cursor)
                    {
                        self.operate_line_motion(op, end);
                        return Effect::None;
                    }
                    let inclusive = matches!(key, "$" | "e" | "E" | "%")
                        && !(key == "$"
                            && end == self.line_start(end)
                            && end == self.line_end(end));
                    let start = self.cursor.min(end);
                    let last = self.cursor.max(end);
                    let end = if inclusive && last < self.line_end(last) {
                        let Some(end) = self.grapheme_move(last, 1, true, true) else {
                            self.chosen_register = None;
                            self.message =
                                "Operación Unicode fuera del límite; texto conservado".into();
                            return Effect::None;
                        };
                        end
                    } else {
                        (last + usize::from(inclusive)).min(self.text.len())
                    };
                    self.operate(op, start..end, false);
                } else {
                    self.chosen_register = None;
                    if key != "$" {
                        self.message = if key == "%" {
                            "%: sin pareja simple o contexto no compatible"
                        } else {
                            "Operador/textobject pendiente"
                        }
                        .into();
                    }
                }
                return Effect::None;
            }
        }
        if matches!(key, "{" | "}") {
            if let Some(motion) = self.paragraph_motion(key == "}", n) {
                self.record_history_jump();
                self.cursor = motion.offset;
                self.clamp();
            }
            return Effect::None;
        }
        if let Some(p) = self.motion(key, n) {
            if key == "%" {
                self.record_history_jump();
            }
            self.cursor = p;
            self.clamp();
            return Effect::None;
        }
        match key {
            "$" => {}
            "%" => self.message = "%: sin pareja simple o contexto no compatible".into(),
            "*" | "#" | "£" => return self.search_word(key != "*", n, true),
            "~" => {
                let end = (self.cursor + n).min(self.line_end(self.cursor));
                self.case_range(self.cursor..end, '~', end);
            }
            "i" | "a" | "I" | "A" => {
                if key == "i" && self.grapheme_move(self.cursor, 0, true, true).is_none() {
                    self.message = "Cursor Unicode fuera del límite; texto conservado".into();
                    return Effect::None;
                }
                let after = if key == "a" {
                    let Some(after) = self.grapheme_move(self.cursor, 1, true, true) else {
                        self.message =
                            "Movimiento Unicode fuera del límite; texto conservado".into();
                        return Effect::None;
                    };
                    Some(after)
                } else {
                    None
                };
                self.checkpoint();
                if let Some(after) = after {
                    self.cursor = after;
                }
                if key == "I" {
                    self.cursor = self.motion("^", 1).unwrap();
                }
                if key == "A" {
                    self.cursor = self.line_end(self.cursor);
                }
                self.mode = Mode::Insert;
            }
            "o" | "O" => {
                self.open_indented_line(key == "o");
            }
            ">" | "<" if self.selection().is_some() => {
                let selection = self.selection().unwrap();
                self.shift_lines(
                    self.line_start(selection.start),
                    self.line_start(selection.end.saturating_sub(1)),
                    key == ">",
                    n,
                    true,
                );
            }
            ">" | "<" => {
                self.operator_count_explicit = explicit_count;
                self.pending = format!("{key}{n}");
            }
            "d" | "c" | "y" => {
                self.operator_count_explicit = explicit_count;
                self.pending = format!("{key}{n}");
            }
            "g" => {
                self.operator_count_explicit = explicit_count;
                self.pending = format!("g{n}");
            }
            "x" if self
                .folded_line_count(self.line_starts[self.folded_first_row(self.position().0)])
                .is_some() =>
            {
                let first = self.folded_first_row(self.position().0);
                let last = self.folded_last_row(first);
                let start = self.line_starts[first];
                let end = (self.line_end(self.line_starts[last]) + 1).min(self.text.len());
                self.operate('d', start..end, true);
            }
            "x" => {
                if let Some(end) = self.grapheme_move(self.cursor, n, true, true) {
                    if end > self.cursor {
                        self.operate('d', self.cursor..end, false);
                    } else {
                        self.chosen_register = None;
                    }
                } else {
                    self.chosen_register = None;
                    self.message = "Borrado Unicode fuera del límite; texto conservado".into();
                }
            }
            "X" => {
                let Some(start) = self.grapheme_move(self.cursor, n, false, true) else {
                    self.chosen_register = None;
                    self.message = "Borrado Unicode fuera del límite o cursor dentro de una secuencia; texto conservado".into();
                    return Effect::None;
                };
                if start < self.cursor {
                    self.operate('d', start..self.cursor, false);
                } else {
                    self.chosen_register = None;
                }
            }
            "D" | "C" | "Y" => {
                // Neovim's default Normal-mode Y mapping is y$; D and C are
                // d$/c$. Reuse the counted operator path, including folds and
                // registers, without recording an extra synthetic keystroke.
                let op = match key {
                    "D" => 'd',
                    "C" => 'c',
                    _ => 'y',
                };
                self.operator_count_explicit = explicit_count;
                self.pending = format!("{op}{n}");
                return self.key_base("$");
            }
            "v" | "V" => {
                let entering = self.mode == Mode::Normal;
                let mode = if key == "v" {
                    Mode::Visual
                } else {
                    Mode::VisualLine
                };
                if self.mode == mode {
                    self.mode = Mode::Normal;
                } else {
                    if self.selection().is_none() {
                        self.anchor = self.cursor;
                    }
                    self.mode = mode;
                    if entering {
                        self.expand_folded_visual_entry();
                    }
                }
            }
            "p" | "P" => return self.put_register(key, n),
            "u" | "<C-r>" => {
                for _ in 0..n {
                    let revision = if key == "u" {
                        self.undo.pop()
                    } else {
                        self.redo.pop()
                    };
                    let Some(r) = revision else { break };
                    let visual = self.last_visual;
                    if self.visual_generation == r.visual_generation {
                        self.last_visual = r.visual;
                    } else {
                        self.track_visual_replacement(&r.text);
                    }
                    let previous = Revision {
                        text: std::mem::replace(&mut self.text, r.text),
                        cursor: if key == "u" {
                            r.redo_cursor_line
                                .map(|row| self.line_starts[row.min(self.line_starts.len() - 1)])
                                .unwrap_or(self.cursor)
                        } else {
                            self.cursor
                        },
                        redo_cursor_line: r.redo_cursor_line,
                        visual,
                        visual_generation: self.visual_generation,
                    };
                    self.cursor = r.cursor;
                    if key == "u" {
                        self.redo.push(previous);
                    } else {
                        self.undo.push(previous);
                    }
                    self.changed();
                }
            }
            ":" | "/" | "?" => {
                self.command.clear();
                self.prompt_cursor = 0;
                self.prompt_history = None;
                self.prompt_register_pending = false;
                self.search_reverse = key == "?";
                self.search_count = n;
                self.mode = if key == ":" {
                    Mode::Command
                } else {
                    Mode::Search
                };
                if self.mode == Mode::Search {
                    self.begin_search_preview();
                }
            }
            "n" | "N" => return self.repeat_search(key == "N", n),
            "<C-s>" => return Effect::Save,
            _ => self.message = format!("Tecla pendiente en editor: {key}"),
        }
        Effect::None
    }
}

/// Existing regular UTF-8 files only. Refuse stale writes and preserve mode.
pub fn save_file(root: &Path, path: &Path, expected: &str, text: &str) -> Result<(), String> {
    use std::{
        fs,
        io::Write,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let canonical = path.canonicalize().map_err(|e| e.to_string())?;
    let before = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if canonical != path
        || !canonical.starts_with(&root)
        || !before.is_file()
        || before.nlink() != 1
    {
        return Err(
            "Guardar requiere un archivo regular sin symlink/hardlink dentro del proyecto".into(),
        );
    }
    if before.len() > 2 * 1024 * 1024
        || fs::read_to_string(path).map_err(|e| e.to_string())? != expected
    {
        return Err("El archivo cambió en disco. El buffer se conserva; no se sobrescribió".into());
    }
    let temp = path.with_file_name(format!(
        ".workspace-save-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    ));
    let result = (|| {
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        out.set_permissions(before.permissions())
            .map_err(|e| e.to_string())?;
        out.sync_all().map_err(|e| e.to_string())?;
        let now = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if now.ino() != before.ino()
            || now.mtime() != before.mtime()
            || now.mtime_nsec() != before.mtime_nsec()
            || fs::read_to_string(path).map_err(|e| e.to_string())? != expected
        {
            return Err("Cambio concurrente detectado; se conserva el buffer".into());
        }
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_line_yank_uses_shared_linewise_and_numbered_registers() {
        let bank = RegisterBank::default();
        bank.capture('a', 'y', "keep".into(), false).unwrap();
        bank.record_terminal_lines("alpha   \nbeta\n".into(), false)
            .unwrap();
        let mut editor = Editor::with_registers("base".into(), bank.clone());
        editor.key("p");
        assert_eq!(editor.contents(), "base\nalpha   \nbeta");
        assert_eq!(bank.get('0'), Some(("alpha   \nbeta\n".into(), true)));
        assert_eq!(bank.get('a'), Some(("keep".into(), false)));
        assert!(
            bank.record_terminal_lines("bad\trow\n".into(), false)
                .is_err()
        );
        assert!(
            bank.record_terminal_lines("no newline".into(), false)
                .is_err()
        );
        assert_eq!(bank.get('0'), Some(("alpha   \nbeta\n".into(), true)));
        bank.record_terminal_lines("clipboard\n".into(), true)
            .unwrap();
        assert_eq!(bank.get('+'), Some(("clipboard\n".into(), true)));
        assert_eq!(bank.get('0'), Some(("clipboard\n".into(), true)));
    }

    #[test]
    fn empty_command_host_parses_edit_without_mutating_a_buffer() {
        let mut editor = Editor::new(String::new());
        assert_eq!(editor.key(":"), Effect::None);
        assert_eq!(editor.mode, Mode::Command);
        editor.insert("edit /tmp/synthetic file.txt");
        assert_eq!(
            editor.key("<Enter>"),
            Effect::OpenPath("/tmp/synthetic file.txt".into())
        );
        assert_eq!(editor.mode, Mode::Normal);
        assert_eq!(editor.contents(), "");
        editor.key(":");
        editor.insert("e another");
        editor.key("<Esc>");
        assert_eq!(editor.mode, Mode::Normal);
        assert_eq!(editor.contents(), "");
    }
    #[test]
    fn terminal_recovery_command_produces_a_distinct_effect() {
        let mut editor = Editor::new(String::new());
        editor.key(":");
        editor.insert("TerminalRecover");
        assert_eq!(editor.key("<Enter>"), Effect::RecoverTerminals);
        assert_eq!(editor.mode, Mode::Normal);
    }
    #[test]
    fn ex_line_and_buffer_navigation() {
        let mut e = Editor::new("alpha\n  beta\ngamma".into());
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-clearjumps.json"
        ))
        .unwrap();
        for command in ["clearj", "clearju", "clearjum", "clearjump", "clearjumps"] {
            assert_eq!(reference["aliases"][command], "clearjumps");
            e.key(":");
            e.insert(command);
            assert_eq!(e.key("<Enter>"), Effect::ClearJumps, ":{command}");
        }
        e.key(":");
        e.insert("clearjumps!");
        assert_eq!(reference["aliases"]["clearjumps!"], false);
        assert_eq!(e.key("<Enter>"), Effect::None);
        e.cursor = 3;
        for (command, position) in [("2", (1, 0)), ("99", (2, 0)), ("0", (0, 0))] {
            e.key(":");
            e.insert(command);
            assert_eq!(e.key("<Enter>"), Effect::None);
            assert_eq!(e.position(), position, ":{command}");
        }
        for (command, effect) in [
            ("bn", "buffers.next"),
            ("bnext", "buffers.next"),
            ("bp", "buffers.prev"),
            ("bprevious", "buffers.prev"),
            ("bprev", "buffers.prev"),
            ("e #", "buffers.alternate.edit"),
            ("edit  #", "buffers.alternate.edit"),
            ("b #", "buffers.alternate"),
            ("bu #", "buffers.alternate"),
            ("buf #", "buffers.alternate"),
            ("buff #", "buffers.alternate"),
            ("buffe #", "buffers.alternate"),
            ("buffer #", "buffers.alternate"),
            ("b#", "buffers.alternate"),
            ("buffer#", "buffers.alternate"),
            ("b 2", "buffers.number.2"),
            ("buffer2", "buffers.number.2"),
            ("buf 0", "buffers.number.0"),
        ] {
            e.key(":");
            e.insert(command);
            assert_eq!(e.key("<Enter>"), Effect::Buffer(effect.into()));
        }
        for command in ["b #extra", "buffer##", "bextra#", "b", "b 2extra"] {
            e.key(":");
            e.insert(command);
            assert_eq!(e.key("<Enter>"), Effect::None, ":{command}");
        }
        for (command, all) in [
            ("ls", false),
            ("buffers", false),
            ("files", false),
            ("ls!", true),
            ("buffers!", true),
            ("files!", true),
        ] {
            e.key(":");
            e.insert(command);
            assert_eq!(e.key("<Enter>"), Effect::BufferList(all), ":{command}");
        }
        for (command, force) in [
            ("bd", false),
            ("bdelete", false),
            ("bd!", true),
            ("bdelete!", true),
        ] {
            e.key(":");
            e.insert(command);
            assert_eq!(
                e.key("<Enter>"),
                Effect::VimBufferDelete(force),
                ":{command}"
            );
        }
        for (command, force, target) in [
            ("bd2", false, VimBufferTarget::Number(2)),
            ("bdelete 2", false, VimBufferTarget::Number(2)),
            ("bd!2", true, VimBufferTarget::Number(2)),
            ("bd! 2", true, VimBufferTarget::Number(2)),
            ("bd #", false, VimBufferTarget::Alternate),
            ("bd!#", true, VimBufferTarget::Alternate),
            ("bdelete 0", false, VimBufferTarget::Number(0)),
        ] {
            e.key(":");
            e.insert(command);
            assert_eq!(
                e.key("<Enter>"),
                Effect::VimBufferDeleteTarget(force, target),
                ":{command}"
            );
        }
    }
    #[test]
    fn mode_transitions_keep_editor_keys_and_cancel_without_losing_text() {
        let mut e = Editor::new("abc\nnext".into());
        e.key("i");
        assert_eq!(e.mode, Mode::Insert);
        e.insert("X");
        e.key("<C-h>");
        assert_eq!(e.contents(), "abc\nnext");
        e.key("<C-j>");
        assert_eq!(e.contents(), "\nabc\nnext");
        e.key("<Esc>");
        assert_eq!(e.mode, Mode::Normal);
        for (key, mode) in [
            ("v", Mode::Visual),
            ("V", Mode::VisualLine),
            ("<C-v>", Mode::VisualBlock),
            (":", Mode::Command),
            ("/", Mode::Search),
            ("?", Mode::Search),
        ] {
            let text = e.contents();
            e.key(key);
            assert_eq!(e.mode, mode, "{key}");
            e.key("<Esc>");
            assert_eq!(e.mode, Mode::Normal);
            assert_eq!(e.contents(), text);
        }
        e.key(":");
        e.insert("enew");
        assert_eq!(e.key("<C-j>"), Effect::NewFile);
        assert_eq!(e.mode, Mode::Normal);
    }
    #[test]
    fn enew_and_first_write_are_effects_without_mutating_source_buffer() {
        let mut e = Editor::new("preserved".into());
        for (command, effect) in [
            ("enew", Effect::NewFile),
            ("set wrap", Effect::Wrap(Some(true))),
            ("setlocal wrap", Effect::Wrap(Some(true))),
            ("set nowrap", Effect::Wrap(Some(false))),
            ("setlocal nowrap", Effect::Wrap(Some(false))),
            ("set wrap!", Effect::Wrap(None)),
            ("set invwrap", Effect::Wrap(None)),
            ("sp", Effect::Split(false)),
            ("split", Effect::Split(false)),
            ("vs", Effect::Split(true)),
            ("vsplit", Effect::Split(true)),
            ("new", Effect::SplitNew(false)),
            ("vnew", Effect::SplitNew(true)),
            ("on", Effect::Only),
            ("only", Effect::Only),
            ("clo", Effect::CloseWindow),
            ("close", Effect::CloseWindow),
            (
                "sp nested/file name.txt",
                Effect::SplitPath(false, "nested/file name.txt".into()),
            ),
            (
                "vsplit /tmp/a.txt",
                Effect::SplitPath(true, "/tmp/a.txt".into()),
            ),
            (
                "e /synthetic/path with spaces.txt",
                Effect::OpenPath("/synthetic/path with spaces.txt".into()),
            ),
            (
                "edit nested/file.txt",
                Effect::OpenPath("nested/file.txt".into()),
            ),
            ("e \\#", Effect::OpenPath("\\#".into())),
            ("w new.txt", Effect::WriteCopy("new.txt".into())),
            ("quit", Effect::Close(false)),
            ("quit!", Effect::Close(true)),
            ("write", Effect::Save),
            ("write new.txt", Effect::WriteCopy("new.txt".into())),
            ("wq ñ.txt", Effect::Write("ñ.txt".into(), true)),
            (
                "saveas /tmp/new name.txt",
                Effect::SaveAs("/tmp/new name.txt".into()),
            ),
        ] {
            e.key(":");
            e.insert(command);
            assert_eq!(e.key("<Enter>"), effect);
            assert_eq!(e.mode, Mode::Normal);
            assert_eq!(e.contents(), "preserved");
        }
        e.key("i");
        e.insert(" ");
        e.key("<Esc>");
        assert_eq!(e.mode, Mode::Normal);
    }
    #[test]
    fn video_command_preserves_the_explicit_relative_path_and_buffer() {
        let mut editor = Editor::new("unchanged".into());
        editor.key(":");
        editor.insert("PRVideo docs/synthetic-video.mov");
        assert_eq!(
            editor.key("<Enter>"),
            Effect::Video("docs/synthetic-video.mov".into())
        );
        assert_eq!(editor.text.iter().collect::<String>(), "unchanged");
        assert_eq!(editor.mode, Mode::Normal);
        assert!(!editor.dirty());
    }
    #[test]
    fn vertical_motions_keep_tab_columns_through_short_lines() {
        let mut on_tab = Editor::new("\tabc\n0123456".into());
        on_tab.key("j");
        assert_eq!(on_tab.position(), (1, 1));
        // Clean Neovim, tabstop=2: j/j/j/k/k from row 1, byte 4.
        let mut e = Editor::new("abcdefghi\n\tabcdefg\nx\nabcdefghi".into());
        e.goto(0, 4);
        for (key, position) in [
            ("j", (1, 3)),
            ("j", (2, 0)),
            ("j", (3, 4)),
            ("k", (2, 0)),
            ("k", (1, 3)),
        ] {
            e.key(key);
            assert_eq!(e.position(), position);
        }
        e.goto(0, 1);
        e.key("j");
        assert_eq!(e.position(), (1, 0));
        e.key("k");
        assert_eq!(e.position(), (0, 1));
        e.key("$");
        e.key("j");
        assert_eq!(e.position(), (1, 7));
        assert!(!e.dirty());
    }
    #[test]
    fn vertical_motions_restore_desired_column_across_short_lines_and_end_of_line() {
        let mut e = Editor::new("abcdefghij\nx\n0123456789\n\nabcdefghij".into());
        e.goto(0, 8);
        e.key("j");
        assert_eq!(e.position(), (1, 0));
        e.key("j");
        assert_eq!(e.position(), (2, 8));
        e.key("k");
        e.key("k");
        assert_eq!(e.position(), (0, 8));
        e.key("j");
        e.key("l");
        e.key("j");
        assert_eq!(e.position(), (2, 0));
        e.key("$");
        e.key("j");
        assert_eq!(e.position(), (3, 0));
        e.key("j");
        assert_eq!(e.position(), (4, 9));
        e.goto(0, 8);
        e.key("2");
        e.key("j");
        assert_eq!(e.position(), (2, 8));
        e.key("v");
        e.key("j");
        e.key("j");
        assert_eq!(e.position(), (4, 8));
        assert!(e.selection().unwrap().len() > 10);
        e.key("<Esc>");
        e.goto(0, 8);
        e.key("j");
        e.goto(2, 4);
        e.key("k");
        e.key("k");
        assert_eq!(e.position(), (0, 4));
        e.key("i");
        e.insert("ñ");
        e.key("<Esc>");
        e.key("j");
        e.key("j");
        assert_eq!(e.position(), (2, 4));
        e.key("u");
        assert!(!e.dirty());
    }
    #[test]
    fn yanks_named_registers_and_blackhole_work_across_buffers() {
        let bank = RegisterBank::default();
        let mut a = Editor::with_registers("uno\ndos".into(), bank.clone());
        let mut b = Editor::with_registers("base".into(), bank.clone());
        a.key("y");
        a.key("y");
        b.key("p");
        assert_eq!(b.contents(), "base\nuno");
        b.key("u");
        assert_eq!(b.contents(), "base");
        a.key("\"");
        a.key("a");
        a.key("y");
        a.key("e");
        b.key("\"");
        b.key("a");
        b.key("P");
        assert_eq!(b.contents(), "unobase");
        let mut c = Editor::with_registers("discard".into(), bank);
        c.key("\"");
        c.key("_");
        c.key("d");
        c.key("d");
        c.key("p");
        assert_eq!(c.contents(), "uno");
        assert_eq!(a.contents(), "uno\ndos");
    }
    #[test]
    fn yank_character_motions_match_clean_neovim_reference() {
        // Captured from clean Neovim with a synthetic three-line buffer.
        for (keys, expected) in [
            ("yw", "alpha "),
            ("ye", "alpha"),
            ("y$", "alpha beta"),
            ("y2w", "alpha beta"),
            ("vly", "al"),
        ] {
            let mut editor = Editor::new("alpha beta\ngamma delta\nomega".into());
            for key in keys.chars() {
                editor.key(&key.to_string());
            }
            assert_eq!(
                editor.registers.get('"'),
                Some((expected.into(), false)),
                "{keys}"
            );
            assert_eq!(
                editor.contents(),
                "alpha beta\ngamma delta\nomega",
                "{keys}"
            );
        }
    }
    #[test]
    fn completion_is_one_insert_change_and_replays_without_a_server() {
        let mut editor = Editor::new("answer".into());
        editor.key("o");
        editor.insert("ans");
        let range =
            serde_json::json!({"start":{"line":1,"character":0},"end":{"line":1,"character":3}});
        editor
            .apply_lsp_edit(&range, "answer", editor.version)
            .unwrap();
        editor.key("<Esc>");
        assert_eq!(editor.contents(), "answer\nanswer");
        editor.key("u");
        assert_eq!(editor.contents(), "answer");
        editor.key("<C-r>");
        assert_eq!(editor.contents(), "answer\nanswer");
        editor.key(".");
        for _ in 0..20 {
            if !editor.replaying() {
                break;
            }
            editor.advance_replay();
        }
        assert_eq!(editor.contents(), "answer\nanswer\nanswer");
        editor.key("q");
        editor.key("a");
        editor.key("o");
        editor.insert("árTAIL");
        for _ in 0..4 {
            editor.key("<Left>");
        }
        let row = editor.position().0;
        let range = serde_json::json!({"start":{"line":row,"character":0},"end":{"line":row,"character":6}});
        let stale = editor.version;
        editor.apply_lsp_edit(&range, "árbol", stale).unwrap();
        assert!(editor.apply_lsp_edit(&range, "bad", stale).is_err());
        editor.key("<Esc>");
        editor.key("q");
        editor.key("@");
        editor.key("a");
        for _ in 0..20 {
            if !editor.replaying() {
                break;
            }
            editor.advance_replay();
        }
        assert!(!editor.replaying());
        assert!(editor.contents().ends_with("árbol\nárbol"));
        let mut crlf = Editor::new("ans\r\n".into());
        crlf.key("A");
        let range =
            serde_json::json!({"start":{"line":0,"character":0},"end":{"line":0,"character":3}});
        crlf.apply_lsp_edit(&range, "answer\r\nnext", crlf.version)
            .unwrap();
        assert_eq!(crlf.contents(), "answer\r\nnext\r\n");
        crlf.key("<Esc>");
        crlf.key("u");
        assert_eq!(crlf.contents(), "ans\r\n");
    }
    #[test]
    fn cached_utf16_positions_survive_mutation_and_undo() {
        let mut editor = Editor::new("🦀á\n界ñ\n".into());
        for action in 0..3 {
            if action == 1 {
                editor.key("i");
                editor.insert("ñ\n🦀");
                editor.key("<Esc>");
            }
            if action == 2 {
                editor.key("u");
            }
            for offset in 0..=editor.text.len() {
                assert_eq!(
                    editor.lsp_position(offset),
                    crate::lsp::position(&editor.text, offset)
                );
            }
        }
        assert_eq!(
            editor.lsp_position(2),
            serde_json::json!({"line":0,"character":3})
        );
        assert_eq!(
            editor.lsp_position(usize::MAX),
            crate::lsp::position(&editor.text, editor.text.len())
        );
    }
    #[test]
    fn change_word_line_and_counted_goto_follow_modal_semantics() {
        let mut e = Editor::new("uno dos\ntres\ncuatro\n".into());
        e.key("c");
        e.key("w");
        e.insert("nuevo");
        e.key("<Esc>");
        assert_eq!(e.contents(), "nuevo dos\ntres\ncuatro\n");
        e.key("2");
        e.key("G");
        assert_eq!(e.position(), (1, 3));
        e.key("c");
        e.key("c");
        e.insert("linea");
        e.key("<Esc>");
        assert_eq!(e.contents(), "nuevo dos\nlinea\ncuatro\n");
        e.key("3");
        e.key("g");
        e.key("g");
        assert_eq!(e.position(), (2, 4));
    }
    #[test]
    fn counts_operators_unicode_insert_undo_and_search() {
        let mut e = Editor::new("uno dos\ntres\n".into());
        e.key("2");
        e.key("d");
        e.key("w");
        assert_eq!(e.contents(), "\ntres\n");
        e.key("u");
        assert_eq!(e.contents(), "uno dos\ntres\n");
        e.key("i");
        e.insert("á🐱 ");
        e.key("<Esc>");
        assert!(e.contents().starts_with("á🐱 uno"));
        e.key("u");
        assert_eq!(e.contents(), "uno dos\ntres\n");
        e.key("/");
        e.insert("tres");
        e.key("<Enter>");
        e.finish_search_test();
        assert_eq!(e.position(), (1, 0));
    }
    #[test]
    fn line_yank_paste_visual_and_crlf() {
        let mut e = Editor::new("a\r\nb\r\n".into());
        e.key("y");
        e.key("y");
        e.key("p");
        assert_eq!(e.contents(), "a\r\na\r\nb\r\n");
        e.key("V");
        e.key("d");
        assert_eq!(e.contents(), "a\r\nb\r\n");
        e.key(":");
        e.insert("wq");
        assert_eq!(e.key("<Enter>"), Effect::SaveClose);
    }
    #[test]
    fn alternate_buffer_control_key_is_owned_only_by_normal_mode() {
        let mut e = Editor::new("abc".into());
        for key in ["<C-6>", "<C-^>"] {
            assert_eq!(e.key(key), Effect::Buffer("buffers.alternate".into()));
        }
        e.key("2");
        assert_eq!(e.key("<C-6>"), Effect::Buffer("buffers.number.2".into()));
        e.key("i");
        assert_eq!(e.key("<C-6>"), Effect::None);
        assert_eq!(e.mode, Mode::Insert);
        e.key("<Esc>");
        e.key(":");
        assert_eq!(e.key("<C-6>"), Effect::None);
        assert_eq!(e.mode, Mode::Command);
    }
    #[test]
    fn save_ack_keeps_new_edits_undo_and_buffer_navigation_live() {
        let mut e = Editor::new("abc".into());
        e.saving = true;
        assert_eq!(e.key("H"), Effect::Buffer("buffers.prev".into()));
        e.key("A");
        e.paste(" ñ");
        e.key("<Esc>");
        assert_eq!(e.contents(), "abc ñ");
        e.saved("abc".into());
        assert!(e.dirty());
        assert_eq!(e.original(), "abc");
        e.key("u");
        assert_eq!(e.contents(), "abc");
        assert!(!e.dirty());
        e.key("<C-r>");
        assert_eq!(e.contents(), "abc ñ");
        assert!(e.dirty());
        e.saving = true;
        e.key("0");
        e.key("/");
        e.insert("ñ");
        e.key("<Enter>");
        e.finish_search_test();
        assert_eq!(e.cursor, 4);
        e.saved("abc ñ".into());
        assert!(!e.dirty());
    }
    #[test]
    fn save_refuses_external_edit_and_symlink() {
        let root =
            std::env::temp_dir().join(format!("workspace-editor-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("a.txt");
        std::fs::write(&path, "before").unwrap();
        save_file(&root, &path, "before", "after").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after");
        assert!(save_file(&root, &path, "before", "lost").is_err());
        let link = root.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(save_file(&root, &link, "after", "lost").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn wrapped_plain_vertical_keys_preserve_count_and_operator_routing() {
    let mut editor = Editor::new("one\ntwo\nthree".into());
    assert_eq!(editor.key_with_wrap("j"), Effect::WrappedMove(true, 1));
    assert_eq!(editor.cursor, 0);
    assert!(editor.commit_wrapped_motion(4));
    editor.key("v");
    assert_eq!(editor.key_with_wrap("k"), Effect::WrappedMove(false, 1));
    assert!(editor.commit_wrapped_motion(0));
    assert_eq!(editor.selection(), Some(0..5));
    editor.key("<Esc>");
    editor.key_with_wrap("2");
    assert_eq!(editor.key_with_wrap("j"), Effect::None);
    assert_eq!(editor.position().0, 2);
    editor.key_with_wrap("d");
    assert_ne!(editor.key_with_wrap("k"), Effect::WrappedMove(false, 1));
}

#[test]
fn explicit_wrapped_motion_accepts_count_only_before_g() {
    let mut editor = Editor::new("one\ntwo\nthree".into());
    editor.key_with_wrap("3");
    editor.key_with_wrap("g");
    assert_eq!(editor.key_with_wrap("j"), Effect::WrappedMove(true, 3));
    editor.key_with_wrap("g");
    editor.key_with_wrap("2");
    assert_eq!(editor.key_with_wrap("j"), Effect::None);
    assert_eq!(editor.cursor, 0);
    assert!(!editor.command_pending());
}

#[test]
fn wrapped_operator_routes_counts_and_refuses_stale_results() {
    let mut editor = Editor::new("abcdefghijabcdefghijabcdefghij".into());
    for key in ["d", "2", "g"] {
        assert_eq!(editor.key_with_wrap(key), Effect::None);
    }
    assert_eq!(
        editor.key_with_wrap("j"),
        Effect::WrappedOperator('d', true, 2)
    );
    let version = editor.version;
    assert!(!editor.commit_wrapped_operator('d', 1, version, 20));
    assert!(!editor.commit_wrapped_operator('d', 0, version + 1, 20));
    assert_eq!(editor.contents(), "abcdefghijabcdefghijabcdefghij");
    assert!(editor.commit_wrapped_operator('d', 0, version, 20));
    assert_eq!(editor.contents(), "abcdefghij");
    assert_eq!(
        editor.key_with_wrap("."),
        Effect::WrappedOperator('d', true, 2)
    );
}

#[test]
fn normal_x_backwards_matches_clean_neovim() {
    for (sequence, expected, removed, cursor) in [
        ("llX", "acdef\nnext", Some("b"), (0, 1)),
        ("ll2X", "cdef\nnext", Some("ab"), (0, 0)),
        ("ll9X", "cdef\nnext", Some("ab"), (0, 0)),
        ("X", "abcdef\nnext", None, (0, 0)),
        ("lllX.", "adef\nnext", Some("b"), (0, 1)),
    ] {
        let mut editor = Editor::new("abcdef\nnext".into());
        for key in sequence.chars() {
            editor.key(&key.to_string());
        }
        while editor.replaying() {
            editor.advance_replay();
        }
        assert_eq!(editor.contents(), expected, "{sequence}");
        assert_eq!(
            editor.registers.get('"'),
            removed.map(|s| (s.into(), false)),
            "{sequence}"
        );
        assert_eq!(editor.position(), cursor, "{sequence}");
        if removed.is_none() {
            assert!(!editor.dirty());
        }
    }
    let mut editor = Editor::new("abcdef".into());
    for key in ["l", "l", "\"", "a", "X"] {
        editor.key(key);
    }
    assert_eq!(editor.registers.get('a'), Some(("b".into(), false)));
    editor.key("u");
    assert_eq!(editor.contents(), "abcdef");
    let mut editor = Editor::new("ae\u{301}z".into());
    editor.cursor = 2; // Invalid interior cursor: conservatively refuse.
    editor.key("X");
    assert_eq!(editor.contents(), "ae\u{301}z");
    assert!(!editor.dirty());
    editor.goto(0, 3);
    editor.key("X");
    assert_eq!(editor.contents(), "az");
    let mut editor = Editor::new("aéz".into());
    editor.goto(0, 2);
    editor.key("X");
    assert_eq!(editor.contents(), "az");
}

#[test]
fn counted_undo_redo_matches_separate_clean_neovim_edits() {
    let mut editor = Editor::new("base".into());
    for text in ["1", "2", "3"] {
        editor.key("A");
        editor.insert(text);
        editor.key("<Esc>");
    }
    assert_eq!(editor.contents(), "base123");
    assert_eq!(editor.position(), (0, 6));
    editor.key("2");
    editor.key("u");
    assert_eq!(editor.contents(), "base1");
    assert_eq!(editor.position(), (0, 4));
    editor.key("2");
    editor.key("<C-r>");
    assert_eq!(editor.contents(), "base123");
    assert_eq!(editor.position(), (0, 6));
    editor.key("9");
    editor.key("9");
    editor.key("u");
    assert_eq!(editor.contents(), "base");
    assert!(!editor.dirty());
    editor.key("9");
    editor.key("9");
    editor.key("<C-r>");
    assert_eq!(editor.contents(), "base123");
    editor.key("2");
    editor.key("u");
    editor.key("A");
    editor.insert("x");
    editor.key("<Esc>");
    editor.key("<C-r>");
    assert_eq!(editor.contents(), "base1x");
}

#[test]
fn shorthand_yank_delete_change_counts_match_default_neovim() {
    for (sequence, expected, register, cursor) in [
        ("Y", "abcdef\nsecond\nthird", "cdef", (0, 2)),
        ("2Y", "abcdef\nsecond\nthird", "cdef\nsecond", (0, 2)),
        ("D", "ab\nsecond\nthird", "cdef", (0, 1)),
        ("2D", "ab\nthird", "cdef\nsecond", (0, 1)),
        ("3D", "ab", "cdef\nsecond\nthird", (0, 1)),
        ("C", "ab\nsecond\nthird", "cdef", (0, 1)),
        ("2C", "ab\nthird", "cdef\nsecond", (0, 1)),
    ] {
        let mut editor = Editor::new("abcdef\nsecond\nthird".into());
        editor.goto(0, 2);
        for key in sequence.chars() {
            editor.key(&key.to_string());
        }
        if editor.mode == Mode::Insert {
            editor.key("<Esc>");
        }
        assert_eq!(editor.contents(), expected, "{sequence}");
        assert_eq!(
            editor.registers.get('"'),
            Some((register.into(), false)),
            "{sequence}"
        );
        assert_eq!(editor.position(), cursor, "{sequence}");
        if matches!(sequence.chars().last(), Some('D' | 'C')) {
            editor.key("u");
            assert_eq!(
                editor.contents(),
                "abcdef\nsecond\nthird",
                "undo {sequence}"
            );
        }
    }
    let mut editor = Editor::new("abcdef\nsecond".into());
    editor.goto(0, 2);
    for key in ["\"", "a", "Y"] {
        editor.key(key);
    }
    assert_eq!(editor.registers.get('a'), Some(("cdef".into(), false)));
    let mut editor = Editor::new("abcdef\nsecond\nthird".into());
    for key in ["l", "l", "2", "D", "."] {
        editor.key(key);
    }
    while editor.replaying() {
        editor.advance_replay();
    }
    assert_eq!(editor.contents(), "a");
    let mut editor = Editor::new("abcdef\nsecond\nthird".into());
    for key in ["l", "l", "C"] {
        editor.key(key);
    }
    editor.insert("xyz");
    editor.key("<Esc>");
    editor.key(".");
    while editor.replaying() {
        editor.advance_replay();
    }
    assert_eq!(editor.contents(), "abxyxyz\nsecond\nthird");
}

#[test]
fn linewise_operator_motions_match_clean_neovim() {
    #[derive(serde::Deserialize)]
    struct Reference {
        lines: Vec<String>,
        start: [usize; 2],
        keys: String,
        after: Vec<String>,
        register: String,
        #[serde(rename = "type")]
        register_type: String,
        cursor: [usize; 5],
        #[serde(default)]
        eol: bool,
    }
    for raw in [
        include_str!("../../../Resources/Fixtures/nvim-line-operators.jsonl"),
        include_str!("../../../Resources/Fixtures/nvim-end-count.jsonl"),
    ]
    .into_iter()
    .flat_map(str::lines)
    {
        let reference: Reference = serde_json::from_str(raw).unwrap();
        let suffix = if reference.eol { "\n" } else { "" };
        let mut editor = Editor::new(format!("{}{suffix}", reference.lines.join("\n")));
        editor.goto(reference.start[0] - 1, reference.start[1] - 1);
        for key in reference.keys.chars() {
            if editor.mode == Mode::Insert {
                editor.insert(&key.to_string());
            } else {
                editor.key(&key.to_string());
            }
        }
        if editor.mode == Mode::Insert {
            editor.key("<Esc>");
        }
        let after_suffix = if reference.after.len() == 1 && reference.after[0].is_empty() {
            ""
        } else {
            suffix
        };
        assert_eq!(
            editor.contents(),
            format!("{}{after_suffix}", reference.after.join("\n")),
            "{} at {:?}",
            reference.keys,
            reference.start
        );
        let (text, linewise) = editor.registers.get('"').unwrap_or_default();
        assert_eq!(text, reference.register, "{}", reference.keys);
        assert_eq!(
            linewise,
            reference.register_type == "V",
            "{} at {:?} in {:?}",
            reference.keys,
            reference.start,
            reference.lines
        );
        assert_eq!(
            editor.position(),
            (reference.cursor[1] - 1, reference.cursor[2] - 1),
            "{} at {:?} in {:?}",
            reference.keys,
            reference.start,
            reference.lines
        );
    }
    let mut editor = Editor::new("abc\ndef\nghi\n".into());
    for key in ["d", "2", "$"] {
        editor.key(key);
    }
    assert_eq!(editor.contents(), "ghi\n");
    editor.key("u");
    assert_eq!(editor.contents(), "abc\ndef\nghi\n");
}

#[test]
fn linewise_operators_include_closed_fold_like_neovim() {
    for raw in include_str!("../../../Resources/Fixtures/nvim-line-fold-operators.jsonl").lines() {
        let reference: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(reference["closed_before"], "2");
        let lines: Vec<_> = reference["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect();
        let text = lines.join("\n");
        let mut editor = Editor::new(text.clone()).for_path(Path::new("synthetic.rs"));
        let mut engine = crate::syntax_folds::Engine::default();
        let folds = engine
            .compute(
                crate::syntax::Language::Rust,
                &text,
                &std::sync::atomic::AtomicUsize::new(0),
            )
            .unwrap();
        assert!(editor.apply_parser_folds(editor.version, Ok(folds)));
        editor.key("z");
        editor.key("M");
        assert_eq!(editor.folded_line_count(editor.line_starts[1]), Some(3));
        editor.goto(
            reference["start"][0].as_u64().unwrap() as usize - 1,
            reference["start"][1].as_u64().unwrap() as usize - 1,
        );
        let keys = reference["keys"].as_str().unwrap();
        for key in keys.chars() {
            if editor.mode == Mode::Insert {
                editor.insert(&key.to_string());
            } else {
                editor.key(&key.to_string());
            }
        }
        if editor.mode == Mode::Insert {
            editor.key("<Esc>");
        }
        let after: Vec<_> = reference["after"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect();
        assert_eq!(editor.contents(), after.join("\n"), "{keys}");
        let (text, linewise) = editor.registers.get('"').unwrap_or_default();
        assert_eq!(text, reference["register"].as_str().unwrap(), "{keys}");
        assert_eq!(linewise, reference["type"] == "V");
        assert_eq!(
            editor.position(),
            (
                reference["cursor"][1].as_u64().unwrap() as usize - 1,
                reference["cursor"][2].as_u64().unwrap() as usize - 1
            ),
            "{keys}"
        );
    }
}

#[test]
fn percent_motion_matches_clean_neovim_for_plain_brackets_and_counts() {
    for raw in include_str!("../../../Resources/Fixtures/nvim-percent.jsonl").lines() {
        let reference: serde_json::Value = serde_json::from_str(raw).unwrap();
        let lines: Vec<_> = reference["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect();
        let mut editor = Editor::new(lines.join("\n"));
        editor.goto(
            reference["start"][0].as_u64().unwrap() as usize - 1,
            reference["start"][1].as_u64().unwrap() as usize - 1,
        );
        let keys = reference["keys"].as_str().unwrap();
        for key in keys.chars() {
            editor.key(&key.to_string());
        }
        let after: Vec<_> = reference["after"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect();
        assert_eq!(editor.contents(), after.join("\n"), "{keys}");
        let (text, linewise) = editor.registers.get('"').unwrap_or_default();
        assert_eq!(text, reference["register"].as_str().unwrap(), "{keys}");
        assert_eq!(linewise, reference["type"] == "V", "{keys}");
        assert_eq!(
            editor.position(),
            (
                reference["cursor"][1].as_u64().unwrap() as usize - 1,
                reference["cursor"][2].as_u64().unwrap() as usize - 1
            ),
            "{keys}"
        );
    }
    for text in ["(\"(\")", "( /* ) */ )", "( # )"] {
        let mut editor = Editor::new(text.into());
        editor.key("d");
        editor.key("%");
        assert_eq!(
            editor.contents(),
            text,
            "ambiguous input must remain intact"
        );
    }
    let mut editor = Editor::new("(\"(\")".into()).with_clipboard();
    for key in ["\"", "_", "d", "%", "y", "y"] {
        editor.key(key);
    }
    assert_eq!(
        editor.take_clipboard_write(),
        Some(("(\"(\")\n".into(), true))
    );
}
