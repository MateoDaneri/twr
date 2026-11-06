//! Search snapshots are evaluated off the UI thread; shared patterns stay in RAM.
use super::{Editor, Effect, Mode};
use std::ops::Range;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};
const SEARCH_BUDGET_ABORTED: &str = "Búsqueda cancelada o sobre el presupuesto";
pub(super) struct Pattern {
    query: String,
    compiled: OnceLock<Result<regex::Regex, String>>,
}
impl Pattern {
    fn new(query: &str) -> Result<Self, String> {
        if query.len() > 4096 {
            return Err("Búsqueda limitada a 4 KiB".into());
        }
        Ok(Self {
            query: query.into(),
            compiled: OnceLock::new(),
        })
    }
    fn matches(&self, text: &str, cancel: &AtomicBool) -> Result<Vec<Range<usize>>, String> {
        if text.len() > 4 * 1024 * 1024 {
            return Err("Búsqueda limitada a 4 MiB de texto".into());
        }
        if cancel.load(Ordering::Acquire) {
            return Err("Búsqueda cancelada".into());
        }
        // Neovim's buffer patterns can consume the last line's EOL even when
        // the serialized file has no final LF. Never expose the extra line.
        let terminated;
        let text = if text.ends_with('\n') {
            text
        } else {
            terminated = format!("{text}\n");
            &terminated
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        let regex = self
            .compiled
            .get_or_init(|| crate::vim_pattern::compile(&self.query))
            .as_ref()
            .map_err(Clone::clone)?;
        let (mut byte, mut character, mut line_start) = (0, 0, 0);
        let mut positions = Vec::new();
        let mut scan = 0;
        while let Some(found) = regex.find_at(text, scan) {
            if cancel.load(Ordering::Acquire) || std::time::Instant::now() > deadline {
                return Err(SEARCH_BUDGET_ABORTED.into());
            }
            // A final LF terminates the last line; it does not create another line.
            if found.start() == text.len() && text.ends_with('\n') {
                break;
            }
            let skipped = &text[byte..found.start()];
            if let Some(newline) = skipped.rfind('\n') {
                line_start = byte + newline + 1;
            }
            character += skipped.chars().count();
            byte = found.start();
            let position = character.saturating_sub(usize::from(
                byte > line_start && (byte == text.len() || text.as_bytes()[byte] == b'\n'),
            ));
            if positions
                .last()
                .is_none_or(|p: &Range<usize>| p.start != position)
            {
                let end = character + text[found.start()..found.end()].chars().count();
                positions.push(position..end.max(position));
            }
            if positions.len() > 100000 {
                return Err("Búsqueda limitada a 100000 coincidencias".into());
            }
            // cpo-c skips the previous match, at most to the next line's start.
            let next = text[found.start()..found.end()]
                .find('\n')
                .map_or(found.end(), |n| found.start() + n + 1);
            if next > found.start() {
                scan = next;
            } else if let Some(ch) = text[found.start()..].chars().next() {
                scan = found.start() + ch.len_utf8();
            } else {
                break;
            }
        }
        if cancel.load(Ordering::Acquire) || std::time::Instant::now() > deadline {
            return Err(SEARCH_BUDGET_ABORTED.into());
        }
        Ok(positions)
    }
    #[cfg(test)]
    fn positions(&self, text: &str, cancel: &AtomicBool) -> Result<Vec<usize>, String> {
        Ok(self
            .matches(text, cancel)?
            .into_iter()
            .map(|r| r.start)
            .collect())
    }
    #[cfg(test)]
    fn find(
        &self,
        text: &str,
        cursor: usize,
        reverse: bool,
        count: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<Range<usize>>, String> {
        let positions = self.matches(text, cancel)?;
        Ok(select_match(&positions, cursor, reverse, count))
    }
}
fn select_match(
    positions: &[Range<usize>],
    cursor: usize,
    reverse: bool,
    count: usize,
) -> Option<Range<usize>> {
    if positions.is_empty() {
        return None;
    }
    let total = positions.len();
    let step = count.max(1).saturating_sub(1) % total;
    let index = if reverse {
        let before = positions.partition_point(|position| position.start < cursor);
        (before.checked_sub(1).unwrap_or(total - 1) + total - step) % total
    } else {
        (positions.partition_point(|position| position.start <= cursor) + step) % total
    };
    Some(positions[index].clone())
}
pub(super) struct Ticket {
    generation: u64,
    version: u64,
    cursor: usize,
    mode: Mode,
    anchor: usize,
    cancel: Arc<AtomicBool>,
    preview: bool,
    highlights_only: bool,
    query: String,
}
pub(super) struct Preview {
    fold_closures: Option<Vec<bool>>,
    view: (usize, usize),
    range: Option<Range<usize>>,
    matches: Vec<Range<usize>>,
    query: String,
    step_delta: i16,
    version: u64,
    cursor: usize,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
pub struct SearchJob {
    generation: u64,
    pattern: Arc<Pattern>,
    text: String,
    cursor: usize,
    reverse: bool,
    count: usize,
    pub(crate) cancel: Arc<AtomicBool>,
}
pub(super) struct Highlights {
    version: u64,
    query: String,
    ranges: Vec<Range<usize>>,
}
struct Found {
    selected: Option<Range<usize>>,
    ranges: Vec<Range<usize>>,
}
pub struct SearchResult {
    generation: u64,
    cancel: Arc<AtomicBool>,
    result: Result<Found, String>,
}
impl SearchJob {
    pub fn run(self) -> SearchResult {
        SearchResult {
            generation: self.generation,
            result: self
                .pattern
                .matches(&self.text, &self.cancel)
                .map(|positions| {
                    let selected = select_match(&positions, self.cursor, self.reverse, self.count);
                    // Highlight the union, including overlapping multiline matches.
                    let mut ranges: Vec<Range<usize>> = Vec::new();
                    for range in positions {
                        if let Some(last) = ranges.last_mut()
                            && range.start <= last.end
                        {
                            last.end = last.end.max(range.end);
                        } else {
                            ranges.push(range);
                        }
                    }
                    Found { selected, ranges }
                }),
            cancel: self.cancel,
        }
    }
}
impl Editor {
    pub(super) fn begin_search_preview(&mut self) {
        self.search_restore_view = None;
        self.search_preview = Some(Preview {
            fold_closures: self.fold_closures(),
            view: (self.jump_view.0, self.jump_view.1),
            range: None,
            matches: Vec::new(),
            query: String::new(),
            step_delta: 0,
            version: self.version,
            cursor: self.cursor,
        });
    }
    pub(super) fn dismiss_search_preview(&mut self, restore: bool) {
        if let Some(preview) = self.search_preview.take()
            && restore
        {
            if preview.version == self.version {
                self.restore_fold_closures(preview.fold_closures.as_deref());
            }
            self.search_restore_view = Some(preview.view);
        }
    }
    pub fn cancel_search_prompt(&mut self) {
        self.cancel_search();
        self.dismiss_search_preview(true);
        if self.mode == Mode::Search {
            self.mode = Mode::Normal;
            self.command.clear();
            self.prompt_cursor = 0;
        }
    }
    pub fn search_preview_active(&self) -> bool {
        self.search_preview.is_some()
    }
    pub fn search_queued(&self) -> bool {
        self.search_job.is_some()
    }
    pub fn search_match(&self) -> Option<Range<usize>> {
        self.search_preview
            .as_ref()
            .filter(|p| p.version == self.version && p.cursor == self.cursor)?
            .range
            .clone()
    }
    pub fn search_highlights(&self) -> &[Range<usize>] {
        if let Some(preview) = &self.search_preview {
            return if preview.version == self.version && preview.query == self.command {
                &preview.matches
            } else {
                &[]
            };
        }
        if self.registers.search_highlight()
            && let Some(cache) = &self.search_highlights
            && cache.version == self.version
            && self
                .registers
                .search()
                .is_some_and(|(pattern, _)| pattern.query == cache.query)
        {
            return &cache.ranges;
        }
        &[]
    }
    pub fn queue_search_highlights(&mut self) {
        if self.search_busy()
            || self.search_queued()
            || self.mode == Mode::Search
            || !self.registers.search_highlight()
        {
            return;
        }
        let Some((pattern, reverse)) = self.registers.search() else {
            return;
        };
        if self
            .search_highlights
            .as_ref()
            .is_some_and(|cache| cache.version == self.version && cache.query == pattern.query)
        {
            return;
        }
        self.start_search(pattern, reverse, 1, self.cursor, false, true);
    }
    pub fn display_cursor(&self) -> usize {
        if let Some((row, column)) = self.block_display_position() {
            let end = self.line_end(self.line_starts[row]);
            if self.cursor == end {
                return end + column.saturating_sub(self.cell_column(end));
            }
        }
        self.search_match().map_or(self.cursor, |r| r.start)
    }
    /// Virtual columns can share a numeric offset with a later physical row.
    /// Return a caret only to its owning row, not every matching global offset.
    pub fn display_cursor_in_line(&self, start: usize) -> Option<usize> {
        let cursor = self.display_cursor();
        if self.mode == Mode::VisualBlock {
            return self.block_cursor_column(start).map(|_| cursor);
        }
        (self.line_start(cursor.min(self.text.len())) == start).then_some(cursor)
    }
    pub fn display_position(&self) -> (usize, usize) {
        if let Some(position) = self.block_display_position() {
            return position;
        }
        let cursor = self.display_cursor();
        let row = self
            .line_starts
            .partition_point(|p| *p <= cursor)
            .saturating_sub(1);
        (row, cursor - self.line_start(cursor))
    }
    pub fn search_origin_view(&self) -> Option<(usize, usize)> {
        self.search_preview
            .as_ref()
            .filter(|p| p.range.is_none())
            .map(|p| p.view)
    }
    pub fn take_search_restore_view(&mut self) -> Option<(usize, usize)> {
        self.search_restore_view.take()
    }
    pub(super) fn refresh_search_preview(&mut self, invert: Option<bool>) -> Effect {
        // Navigation only applies to a displayed match of this query. Keep an
        // unfinished new query alive when Ctrl-G/T arrives before that result.
        if invert.is_some()
            && self
                .search_preview
                .as_ref()
                .is_none_or(|p| p.range.is_none() || p.query != self.command)
        {
            return Effect::None;
        }
        self.cancel_search();
        if self.mode != Mode::Search || self.search_preview.is_none() {
            return Effect::None;
        }
        let saved = self
            .search_preview
            .as_ref()
            .and_then(|p| p.fold_closures.clone());
        self.restore_fold_closures(saved.as_deref());
        let pattern = if self.command.is_empty() {
            None
        } else {
            Pattern::new(&self.command).ok().map(Arc::new)
        };
        let Some(pattern) = pattern else {
            if let Some(preview) = self.search_preview.as_mut() {
                preview.range = None;
                preview.query.clear();
                preview.matches.clear();
            }
            return Effect::None;
        };
        let preview = self.search_preview.as_mut().unwrap();
        let (cursor, reverse, count) = if let Some(reverse) = invert {
            let Some(range) = &preview.range else {
                return Effect::None;
            };
            if preview.query != self.command {
                return Effect::None;
            }
            preview.step_delta += if reverse { -1 } else { 1 };
            if preview.step_delta.unsigned_abs() > 128 {
                preview.step_delta = 0;
                self.message = "Navegación incremental limitada a 128 pasos pendientes".into();
                return Effect::None;
            }
            if preview.step_delta == 0 {
                return Effect::None;
            }
            (
                range.start,
                preview.step_delta < 0,
                preview.step_delta.unsigned_abs() as usize,
            )
        } else {
            preview.step_delta = 0;
            (self.cursor, self.search_reverse, self.search_count)
        };
        self.start_search(pattern, reverse, count, cursor, true, false)
    }
    /// These keys belong to the search prompt, before workspace pane navigation.
    pub(super) fn search_expansion_key(&mut self, key: &str) -> Option<Effect> {
        if self.mode != Mode::Search {
            return None;
        }
        match key {
            "<C-l>" => Some(self.expand_search_object(false)),
            _ => None,
        }
    }
    pub(super) fn expand_search_object(&mut self, word: bool) -> Effect {
        let Some(preview) = self.search_preview.as_ref() else {
            return Effect::None;
        };
        // A result for a previous query/buffer must never supply text to this prompt.
        if preview.version != self.version
            || preview.cursor != self.cursor
            || (!self.command.is_empty() && preview.query != self.command)
        {
            return Effect::None;
        }
        let position = preview
            .range
            .as_ref()
            .map_or(self.cursor, |range| range.end);
        let end = self.line_end(position.min(self.text.len()));
        if position >= end {
            return Effect::None;
        }
        let extension = if word {
            let keyword = |ch: char| ch.is_alphanumeric() || ch == '_';
            let selected = (position..end.min(position.saturating_add(1024)))
                .find(|p| keyword(self.text[*p]))
                .or_else(|| (!self.text[position].is_whitespace()).then_some(position));
            let Some(selected) = selected else {
                return Effect::None;
            };
            let is_keyword = keyword(self.text[selected]);
            let same = |ch: char| !ch.is_whitespace() && keyword(ch) == is_keyword;
            let mut start = selected;
            while start > self.line_start(selected) && same(self.text[start - 1]) {
                if selected - start >= 1024 {
                    self.message = "Objeto de búsqueda supera el límite del patrón".into();
                    return Effect::None;
                }
                start -= 1;
            }
            let mut finish = selected + 1;
            while finish < end && same(self.text[finish]) {
                if finish - start >= 1024 {
                    self.message = "Objeto de búsqueda supera el límite del patrón".into();
                    return Effect::None;
                }
                finish += 1;
            }
            let object: String = self.text[start..finish].iter().collect();
            // Neovim's command-line word expansion omits an already typed prefix.
            // Use ASCII folding for this overlap, preserving the actual suffix case.
            let typed: Vec<char> = self.command.chars().collect();
            let object_chars: Vec<char> = object.chars().collect();
            let overlap = (1..=typed.len().min(object_chars.len()))
                .rev()
                .find(|n| {
                    typed[typed.len() - n..]
                        .iter()
                        .zip(&object_chars[..*n])
                        .all(|(a, b)| a.eq_ignore_ascii_case(b))
                })
                .unwrap_or(0);
            object_chars[overlap..].iter().collect::<String>()
        } else {
            let ch = self.text[position];
            // Match Neovim's incsearch Ctrl-L quoting with the default magic option;
            // inline \v/\V switches do not change this command-line quoting rule.
            let mut extension = String::new();
            if ".^$*[\\".contains(ch) || self.search_prefix().starts_with(ch) {
                extension.push('\\');
            }
            let mut escaped = false;
            let uppercase = self.command.chars().any(|ch| {
                if escaped {
                    escaped = false;
                    return false;
                }
                if ch == '\\' {
                    escaped = true;
                    return false;
                }
                ch.is_uppercase()
            });
            if uppercase {
                extension.push(ch);
            } else {
                extension.extend(ch.to_lowercase());
            }
            extension
        };
        if extension.is_empty() {
            return Effect::None;
        }
        if self.command.chars().count() + extension.chars().count() >= 1024 {
            self.message = "Expansión de búsqueda supera el límite del patrón".into();
            return Effect::None;
        }
        self.insert_prompt_text(&extension);
        self.refresh_search_preview(None)
    }
    pub fn search_prefix(&self) -> &'static str {
        if self.search_reverse { "?" } else { "/" }
    }
    pub fn cancel_search(&mut self) {
        self.search_followups.clear();
        if self.search_pending.take().is_some() && self.message == "Buscando…" {
            self.message.clear();
        }
        self.search_job.take();
    }
    pub(super) fn search_navigation_busy(&self) -> bool {
        self.search_pending
            .as_ref()
            .is_some_and(|ticket| !ticket.highlights_only)
    }
    pub fn search_busy(&self) -> bool {
        self.search_pending.is_some()
    }
    pub fn take_search_job(&mut self) -> Option<SearchJob> {
        self.search_job.take()
    }
    /// Cancel only the request represented by this result. A newer search
    /// may already be running on the same shared editor.
    pub fn search_result_is_pending(&self, output: &SearchResult) -> bool {
        self.search_pending.as_ref().is_some_and(|ticket| {
            ticket.generation == output.generation && Arc::ptr_eq(&ticket.cancel, &output.cancel)
        })
    }
    pub fn cancel_search_result(&mut self, output: &SearchResult) -> bool {
        if self.search_result_is_pending(output) {
            self.cancel_search_prompt();
            self.cancel_replay();
            true
        } else {
            false
        }
    }
    pub fn apply_search(&mut self, output: SearchResult) -> bool {
        let Some(ticket) = self.search_pending.as_ref() else {
            return false;
        };
        if ticket.generation != output.generation || !Arc::ptr_eq(&ticket.cancel, &output.cancel) {
            return false;
        }
        let preview = ticket.preview;
        let highlights_only = ticket.highlights_only;
        let query = ticket.query.clone();
        let valid = !ticket.cancel.load(Ordering::Acquire)
            && ticket.version == self.version
            && (highlights_only
                || (ticket.cursor == self.cursor
                    && ticket.mode == self.mode
                    && ticket.anchor == self.anchor))
            && (!highlights_only
                || (self.registers.search_highlight()
                    && self
                        .registers
                        .search()
                        .is_some_and(|(p, _)| p.query == ticket.query)))
            && (!preview || (ticket.query == self.command && self.search_preview.is_some()));
        self.search_pending.take();
        if !valid {
            if preview && let Some(preview) = self.search_preview.as_mut() {
                preview.range = None;
                preview.matches.clear();
            }
            self.search_followups.clear();
            self.cancel_replay();
            return false;
        }
        if preview {
            let aborted = output
                .result
                .as_ref()
                .err()
                .is_some_and(|error| error == SEARCH_BUDGET_ABORTED);
            if let Some(preview) = self.search_preview.as_mut() {
                let found = output.result.ok();
                preview.range = found.as_ref().and_then(|f| f.selected.clone());
                preview.matches = found.map_or_else(Vec::new, |f| f.ranges);
                // A timeout is not a valid no-match result. Clearing the
                // preview query prevents Ctrl-L from using the origin cursor.
                preview.query = if aborted { String::new() } else { query };
                preview.step_delta = 0;
            }
            let saved = self
                .search_preview
                .as_ref()
                .and_then(|p| p.fold_closures.clone());
            self.restore_fold_closures(saved.as_deref());
            if let Some(found) = self.search_match() {
                self.open_folds_at(found.start);
            }
            if aborted {
                self.message = SEARCH_BUDGET_ABORTED.into();
            } else {
                self.message.clear();
            }
            return true;
        }
        let result = output.result.map(|found| {
            self.search_highlights = Some(Highlights {
                version: self.version,
                query: query.clone(),
                ranges: found.ranges,
            });
            found.selected
        });
        if highlights_only {
            if result.is_err() {
                // Do not continuously retry a pattern over the worker's budget.
                self.search_highlights = Some(Highlights {
                    version: self.version,
                    query,
                    ranges: Vec::new(),
                });
            }
            return true;
        }
        match result {
            Ok(Some(position)) => {
                self.record_history_jump();
                self.cursor = position.start;
                self.open_cursor_folds();
                self.desired_column = None;
                self.message.clear();
                if let Some(invert) = self.search_followups.pop_front() {
                    let following = std::mem::take(&mut self.search_followups);
                    self.repeat_search(invert, 1);
                    self.search_followups = following;
                }
            }
            Ok(None) => {
                self.search_followups.clear();
                self.message = "Sin coincidencias".into();
                self.cancel_replay();
            }
            Err(error) => {
                self.search_followups.clear();
                self.message = error;
                self.cancel_replay();
            }
        }
        true
    }
    pub(super) fn submit_search(&mut self, command: String) -> Effect {
        let accepted = self
            .search_preview
            .as_ref()
            .filter(|p| p.query == command && p.version == self.version && p.cursor == self.cursor)
            .and_then(|p| p.range.clone());
        if let Some(preview) = &self.search_preview
            && preview.query == command
            && preview.version == self.version
            && preview.cursor == self.cursor
        {
            self.search_highlights = Some(Highlights {
                version: self.version,
                query: command.clone(),
                ranges: preview.matches.clone(),
            });
        }
        self.dismiss_search_preview(false);
        let pattern = if command.is_empty() {
            self.registers.search().map(|(p, _)| p)
        } else {
            match Pattern::new(&command) {
                Ok(p) => Some(Arc::new(p)),
                Err(error) => {
                    self.message = error;
                    return Effect::None;
                }
            }
        };
        let Some(pattern) = pattern else {
            self.message = "Todavía no hay búsqueda".into();
            return Effect::None;
        };
        self.registers.set_search(pattern, self.search_reverse);
        if let Some(ticket) = self.search_pending.as_mut()
            && ticket.preview
        {
            ticket.preview = false;
            ticket.mode = Mode::Normal;
            return if self.search_queued() {
                Effect::Search
            } else {
                Effect::None
            };
        }
        if let Some(accepted) = accepted {
            self.record_history_jump();
            self.cursor = accepted.start;
            self.open_cursor_folds();
            self.desired_column = None;
            self.message.clear();
            return Effect::None;
        }
        self.repeat_search(false, self.search_count)
    }
    pub(super) fn repeat_search(&mut self, invert: bool, count: usize) -> Effect {
        let Some((pattern, reverse)) = self.registers.search() else {
            self.message = "Todavía no hay búsqueda".into();
            return Effect::None;
        };
        self.registers.set_search_highlight(true);
        self.start_search(pattern, reverse ^ invert, count, self.cursor, false, false)
    }
    fn start_search(
        &mut self,
        pattern: Arc<Pattern>,
        reverse: bool,
        count: usize,
        cursor: usize,
        preview: bool,
        highlights_only: bool,
    ) -> Effect {
        if self.text.len() > 4 * 1024 * 1024 {
            self.message = "Búsqueda sobre el presupuesto de texto".into();
            return Effect::None;
        }
        self.cancel_search();
        self.search_generation = self.search_generation.wrapping_add(1);
        let cancel = Arc::new(AtomicBool::new(false));
        self.search_pending = Some(Ticket {
            generation: self.search_generation,
            version: self.version,
            cursor: self.cursor,
            mode: self.mode,
            anchor: self.anchor,
            cancel: cancel.clone(),
            preview,
            highlights_only,
            query: pattern.query.clone(),
        });
        self.search_job = Some(SearchJob {
            generation: self.search_generation,
            pattern,
            text: self.text.iter().collect(),
            cursor,
            reverse,
            count,
            cancel,
        });
        if !highlights_only {
            self.message = "Buscando…".into();
        }
        Effect::Search
    }
    #[cfg(test)]
    pub(super) fn finish_search_test(&mut self) {
        if let Some(job) = self.take_search_job() {
            self.apply_search(job.run());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::RegisterBank;
    fn key(e: &mut Editor, key: &str) {
        e.key(key);
        e.finish_search_test();
    }
    fn search(e: &mut Editor, query: &str) {
        key(e, "/");
        e.insert(query);
        e.finish_search_test();
        key(e, "<Enter>");
    }
    #[test]
    fn successful_searches_record_jump_origin_like_clean_neovim() {
        use crate::editor::JumpImpact;
        use workspace_core::editor_jumplist::View;
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-search.json"
        ))
        .unwrap();
        for case in reference["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let before = &case["before"];
            let mut editor = Editor::new("first\nneedle one\nmiddle\nneedle two\nlast".into());
            editor.goto(before["cursor"][0].as_u64().unwrap() as usize - 1, 0);
            editor
                .registers
                .set_search(Arc::new(Pattern::new("needle").unwrap()), false);
            assert_eq!(editor.enable_jump_changes(), Some(0));
            let view = View {
                top: 3,
                left: 0,
                skip: 0,
                valid: true,
            };
            editor.set_jump_history_view(view);
            match name {
                "forward" | "backward" | "missing" | "cancel" | "wrap" | "count" => {
                    if name == "count" {
                        editor.key("2");
                    }
                    editor.key(if name == "backward" { "?" } else { "/" });
                    editor.insert(if name == "missing" {
                        "missing"
                    } else {
                        "needle"
                    });
                    editor.finish_search_test();
                    assert!(editor.take_jump_changes().is_empty(), "preview {name}");
                    key(
                        &mut editor,
                        if name == "cancel" { "<Esc>" } else { "<Enter>" },
                    );
                }
                "next" => key(&mut editor, "n"),
                "previous" => key(&mut editor, "N"),
                "word" => key(&mut editor, "*"),
                _ => panic!("unknown case {name}"),
            }
            let after = &case["after"];
            assert_eq!(
                editor.position(),
                (
                    after["cursor"][0].as_u64().unwrap() as usize - 1,
                    after["cursor"][1].as_u64().unwrap() as usize,
                ),
                "cursor {name}"
            );
            let events = editor.take_jump_changes();
            let jumps = after["jumps"].as_array().unwrap();
            assert_eq!(events.len(), jumps.len(), "events {name}");
            for (event, jump) in events.iter().zip(jumps) {
                assert_eq!(
                    event.impact,
                    JumpImpact::Jump {
                        row: jump["row"].as_u64().unwrap() as usize - 1,
                        column: jump["col"].as_u64().unwrap() as usize,
                        view,
                    },
                    "origin {name}"
                );
                assert_eq!((event.before, event.after), (0, 0));
            }
        }
    }

    #[test]
    fn search_jump_is_recorded_only_when_the_pending_result_is_accepted() {
        use crate::editor::JumpImpact;
        let mut editor = Editor::new("start\nneedle\nlast".into());
        editor.enable_jump_changes().unwrap();
        editor.key("/");
        editor.insert("needle");
        let pending = editor.take_search_job().unwrap();
        editor.key("<Enter>");
        assert!(editor.take_jump_changes().is_empty());
        assert!(editor.apply_search(pending.run()));
        assert!(matches!(editor.take_jump_changes().as_slice(), [event]
            if matches!(event.impact, JumpImpact::Jump { row: 0, column: 0, .. })));

        editor.goto(0, 0);
        editor.key("/");
        editor.insert("needle");
        let stale = editor.take_search_job().unwrap();
        editor.cancel_search_prompt();
        assert!(!editor.apply_search(stale.run()));
        assert!(editor.take_jump_changes().is_empty());
    }

    #[test]
    fn discarding_old_search_result_does_not_cancel_newer_request() {
        let mut editor = Editor::new("start needle newer".into());
        editor.key("/");
        editor.insert("needle");
        let old = editor.take_search_job().unwrap();
        editor.insert(" newer");
        let current = editor.take_search_job().unwrap();
        assert!(!editor.cancel_search_result(&old.run()));
        assert!(editor.search_busy());
        let current = current.run();
        assert!(editor.cancel_search_result(&current));
        assert!(!editor.search_busy());
        assert_eq!(editor.mode, Mode::Normal);
        assert!(!editor.cancel_search_result(&current));
    }
    #[test]
    fn expansions_match_clean_neovim_command_lines() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-search-expansion.json"
        ))
        .unwrap();
        let mut failures = Vec::new();
        for case in fixture["cases"].as_array().unwrap() {
            let mut e = Editor::new(case["text"].as_str().unwrap().into());
            key(&mut e, case["prefix"].as_str().unwrap_or("/"));
            e.insert(case["query"].as_str().unwrap());
            e.finish_search_test();
            for chord in case["keys"].as_array().unwrap() {
                key(&mut e, chord.as_str().unwrap());
            }
            if e.command != case["expected_command"].as_str().unwrap() {
                failures.push(format!(
                    "{}: actual {:?}, expected {}",
                    case["label"], e.command, case["expected_command"]
                ));
            }
            assert_eq!(e.mode, Mode::Search);
            assert_eq!(e.cursor, 0);
            assert_eq!(e.version, 0);
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
    #[test]
    fn expansion_rejects_stale_preview_cancels_prefix_and_preserves_query_budget() {
        let mut e = Editor::new("start foobar.tail".into());
        key(&mut e, "/");
        e.insert("foo");
        let old = e.take_search_job().unwrap();
        e.insert("bar");
        assert!(!e.apply_search(old.run()));
        key(&mut e, "<C-l>");
        assert_eq!(e.command, "foobar");
        e.finish_search_test();
        key(&mut e, "<C-l>");
        assert_eq!(e.command, "foobar\\.");
        key(&mut e, "<C-r>");
        key(&mut e, "<Esc>");
        assert_eq!(e.mode, Mode::Search);
        key(&mut e, "<Esc>");
        assert_eq!(e.mode, Mode::Normal);
        assert!(e.registers.search().is_none());
        key(&mut e, "l");
        assert_eq!(e.cursor, 1);
        let prefix = "a".repeat(1022);
        let mut e = Editor::new(format!("start {prefix}."));
        key(&mut e, "/");
        e.insert(&prefix);
        // Test the pattern-length boundary with a confirmed match. Compiling
        // a 1022-character regex can exceed the separate wall-clock budget
        // under load, which exercises the abort path rather than this limit.
        let job = e.take_search_job().unwrap();
        let range = 6..(6 + prefix.len());
        assert!(e.apply_search(SearchResult {
            generation: job.generation,
            cancel: job.cancel.clone(),
            result: Ok(Found {
                selected: Some(range.clone()),
                ranges: vec![range],
            }),
        }));
        key(&mut e, "<C-l>");
        assert_eq!(e.command, prefix);
        assert!(e.message.contains("límite"));
    }
    #[test]
    fn search_budget_failure_never_expands_from_an_unverified_cursor() {
        let mut e = Editor::new("start foobar".into());
        key(&mut e, "/");
        e.insert("foo");
        let job = e.take_search_job().unwrap();
        let output = SearchResult {
            generation: job.generation,
            cancel: job.cancel.clone(),
            result: Err(SEARCH_BUDGET_ABORTED.into()),
        };
        assert!(e.apply_search(output));
        key(&mut e, "<C-l>");
        assert_eq!(e.command, "foo");
        assert!(e.message.contains("presupuesto"));
        assert_eq!(e.cursor, 0);
        // Editing the query starts a fresh preview; a successful result may
        // expand normally again and must clear the previous failure.
        e.insert("b");
        e.finish_search_test();
        key(&mut e, "<C-l>");
        assert_eq!(e.command, "fooba");
        assert!(!e.message.contains("presupuesto"));
    }
    #[test]
    fn word_expansion_bounds_large_objects_and_rejects_unsupported_register_text() {
        let mut e = Editor::new(format!("zzz {}", "a".repeat(10000)));
        key(&mut e, "/");
        e.insert("a");
        e.finish_search_test();
        key(&mut e, "<C-r>");
        key(&mut e, "<C-w>");
        assert_eq!(e.command, "a");
        assert!(e.message.contains("límite"));
        assert_eq!(e.version, 0);
        key(&mut e, "<C-r>");
        e.insert("b");
        assert_eq!(e.command, "a");
        assert!(e.message.contains("Registros"));
        key(&mut e, "<Esc>");
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.cursor, 0);
    }
    #[test]
    fn search_expansion_owns_ctrl_l_without_changing_other_workspace_modes() {
        assert!(!Mode::Search.workspace_chord("<C-l>"));
        assert!(!Mode::Search.workspace_chord("<C-r>"));
        assert!(!Mode::Search.workspace_chord("<C-w>"));
        for mode in [Mode::Insert, Mode::Command, Mode::Visual] {
            assert!(!mode.workspace_chord("<C-l>"));
        }
        assert!(Mode::Normal.workspace_chord("<C-l>"));
        assert!(!Mode::Search.workspace_chord("<C-/>"));
    }
    #[test]
    fn all_unicode_matches_survive_enter_and_cancel_restores_confirmed_pattern() {
        let mut e = Editor::new("ñ😀 ñ😀\nñ😀".into());
        e.key("/");
        e.insert("ñ😀");
        e.finish_search_test();
        assert_eq!(e.search_highlights(), &[0..2, 3..5, 6..8]);
        assert_eq!(e.cursor, 0);
        key(&mut e, "<Enter>");
        assert_eq!(e.cursor, 3);
        assert_eq!(e.search_highlights(), &[0..2, 3..5, 6..8]);
        e.key("/");
        e.insert("missing");
        e.finish_search_test();
        assert!(e.search_highlights().is_empty());
        key(&mut e, "<Esc>");
        assert_eq!(e.search_highlights(), &[0..2, 3..5, 6..8]);
        e.key("/");
        e.insert("ñ");
        e.finish_search_test();
        key(&mut e, "<BS>");
        assert!(e.search_highlights().is_empty());
        assert_eq!(e.contents(), "ñ😀 ñ😀\nñ😀");
    }
    #[test]
    fn edits_refresh_highlights_without_moving_cursor_and_discard_old_generation() {
        let mut e = Editor::new("x x".into());
        search(&mut e, "x");
        e.mode = Mode::Insert;
        e.cursor = e.text.len();
        e.insert(" x");
        assert!(e.search_highlights().is_empty());
        e.queue_search_highlights();
        let old = e.take_search_job().unwrap();
        e.insert(" x");
        assert!(!e.apply_search(old.run()));
        e.queue_search_highlights();
        let job = e.take_search_job().unwrap();
        e.cursor = 0; // Highlight-only work does not own navigation.
        assert!(e.apply_search(job.run()));
        assert_eq!(e.cursor, 0);
        assert_eq!(e.mode, Mode::Insert);
        assert_eq!(e.search_highlights(), &[0..1, 2..3, 4..5, 6..7]);
        e.queue_search_highlights();
        assert!(!e.search_busy());
    }
    #[test]
    fn nohl_is_shared_and_next_search_reenables_it_without_buffer_changes() {
        let bank = RegisterBank::default();
        let mut a = Editor::with_registers("x x".into(), bank.clone());
        let mut b = Editor::with_registers("x ñ x".into(), bank);
        search(&mut a, "x");
        b.queue_search_highlights();
        let pending = b.take_search_job().unwrap();
        a.key(":");
        a.insert("nohlsearch");
        key(&mut a, "<Enter>");
        assert!(a.search_highlights().is_empty());
        assert!(!b.apply_search(pending.run()));
        b.queue_search_highlights();
        assert!(!b.search_busy());
        key(&mut b, "n");
        assert_eq!(b.search_highlights(), &[0..1, 4..5]);
        assert_eq!(a.search_highlights(), &[0..1, 2..3]);
        assert_eq!(a.contents(), "x x");
        assert_eq!(b.contents(), "x ñ x");
    }
    #[test]
    fn passive_refresh_does_not_swallow_next_match_command() {
        let mut e = Editor::new("x x x".into());
        search(&mut e, "x");
        e.search_highlights = None;
        e.queue_search_highlights();
        let passive = e.take_search_job().unwrap();
        assert_eq!(e.key("n"), Effect::Search);
        assert!(!e.apply_search(passive.run()));
        e.finish_search_test();
        assert_eq!(e.cursor, 4);
        assert_eq!(e.search_highlights(), &[0..1, 2..3, 4..5]);
    }
    #[test]
    fn overlapping_multiline_highlights_are_unioned_without_changing_navigation() {
        let mut e = Editor::new("a\na\na".into());
        search(&mut e, r"a\_.a");
        assert_eq!(e.cursor, 2);
        assert_eq!(e.search_highlights().len(), 1);
        assert_eq!(e.search_highlights()[0], 0..5);
    }
    #[test]
    fn incremental_match_is_provisional_and_escape_restores_view_without_history() {
        let mut e = Editor::new("origin\nñ😀 one\nñ😀 two".into());
        e.cursor = 1;
        e.jump_viewport(0, 3, 10, 20);
        e.key("/");
        e.insert("ñ😀");
        e.finish_search_test();
        assert_eq!(e.search_match(), Some(7..9));
        assert_eq!(e.display_cursor(), 7);
        assert_eq!(e.display_position(), (1, 0));
        assert_eq!(e.cursor, 1);
        assert_eq!(e.version, 0);
        assert!(!e.dirty());
        assert!(e.registers.search().is_none());
        e.key("<Esc>");
        assert_eq!(e.cursor, 1);
        assert_eq!(e.display_cursor(), 1);
        assert_eq!(e.take_search_restore_view(), Some((0, 3)));
        assert!(e.registers.search().is_none());
        assert_eq!(e.mode, Mode::Normal);
    }
    #[test]
    fn preview_supersedes_pending_results_and_invalid_empty_queries_restore_origin() {
        let mut e = Editor::new("a ñ😀 a ñ😀".into());
        e.key("/");
        e.insert("ñ");
        let old = e.take_search_job().unwrap();
        e.insert("😀");
        let latest = e.take_search_job().unwrap();
        assert!(!e.apply_search(old.run()));
        assert!(e.apply_search(latest.run()));
        assert_eq!(e.search_match(), Some(2..4));
        e.insert("z");
        e.finish_search_test();
        assert_eq!(e.search_match(), None);
        assert_eq!(e.display_cursor(), 0);
        key(&mut e, "<BS>");
        assert_eq!(e.search_match(), Some(2..4));
        e.insert(r"\(");
        e.finish_search_test();
        assert_eq!(e.search_match(), None);
        assert!(e.message.is_empty());
        for _ in 0..4 {
            key(&mut e, "<BS>");
        }
        assert!(e.command.is_empty());
        assert!(!e.search_busy());
        assert_eq!(e.search_match(), None);
        assert_eq!(e.cursor, 0);
    }
    #[test]
    fn preview_count_navigation_and_enter_promote_the_pending_request() {
        let mut e = Editor::new("zero ñ x ñ y ñ".into());
        e.key("2");
        e.key("/");
        e.insert("ñ");
        e.finish_search_test();
        assert_eq!(e.search_match(), Some(9..10));
        key(&mut e, "<C-g>");
        assert_eq!(e.search_match(), Some(13..14));
        e.key("<C-t>");
        let pending = e.take_search_job().unwrap();
        assert_eq!(e.key("<Enter>"), Effect::None);
        assert_eq!(e.mode, Mode::Normal);
        assert!(e.search_match().is_none());
        assert!(e.apply_search(pending.run()));
        assert_eq!(e.cursor, 9);
        assert!(!e.search_busy());
        key(&mut e, "n");
        assert_eq!(e.cursor, 13);
        e.key("?");
        e.insert("ñ");
        e.finish_search_test();
        assert_eq!(e.search_match(), Some(9..10));
        key(&mut e, "<C-g>");
        assert_eq!(e.search_match(), Some(13..14));
        key(&mut e, "<C-t>");
        assert_eq!(e.search_match(), Some(9..10));
        assert_eq!(e.key("<Enter>"), Effect::None);
        assert_eq!(e.cursor, 9);
        assert!(!e.dirty());
    }
    #[test]
    fn provisional_results_cannot_change_a_visited_or_modified_buffer() {
        let mut e = Editor::new("a b a".into());
        e.key("/");
        e.insert("b");
        let late = e.take_search_job().unwrap();
        e.cancel_search_prompt();
        assert!(!e.apply_search(late.run()));
        assert_eq!(e.cursor, 0);
        assert_eq!(e.mode, Mode::Normal);
        e.key("/");
        e.insert("b");
        let late = e.take_search_job().unwrap();
        e.version += 1;
        assert!(!e.apply_search(late.run()));
        assert!(e.search_match().is_none());
    }
    #[test]
    fn rapid_preview_navigation_accumulates_and_enter_accepts_the_last_target() {
        let mut e = Editor::new("zero q x q y q".into());
        e.key("/");
        e.insert("q");
        e.finish_search_test();
        assert_eq!(e.search_match(), Some(5..6));
        e.key("<C-g>");
        let old = e.take_search_job().unwrap();
        e.key("<C-g>");
        e.key("<C-g>");
        e.key("<C-t>");
        assert_eq!(e.key("<Enter>"), Effect::Search);
        assert!(!e.apply_search(old.run()));
        e.finish_search_test();
        assert_eq!(e.cursor, 13);
        assert_eq!(e.mode, Mode::Normal);
        e.key("/");
        e.insert("x");
        e.key("<C-g>");
        let pending = e.take_search_job().unwrap();
        assert_eq!(e.key("<Enter>"), Effect::None);
        assert!(e.apply_search(pending.run()));
        assert_eq!(e.cursor, 7);
    }
    #[test]
    fn directions_counts_empty_query_and_cancel_follow_shared_search_state() {
        let bank = RegisterBank::default();
        let mut a = Editor::with_registers("uno ñ uno ñ uno".into(), bank.clone());
        let mut b = Editor::with_registers("ñ uno ñ uno ñ".into(), bank);
        key(&mut a, "?");
        a.insert("uno");
        key(&mut a, "<Enter>");
        assert_eq!(a.cursor, 12);
        key(&mut a, "n");
        assert_eq!(a.cursor, 6);
        key(&mut b, "n");
        assert_eq!(b.cursor, 8);
        key(&mut b, "N");
        assert_eq!(b.cursor, 2);
        key(&mut b, "2");
        key(&mut b, "n");
        assert_eq!(b.cursor, 2);
        key(&mut b, "/");
        key(&mut b, "<Enter>");
        assert_eq!(b.cursor, 8);
        key(&mut a, "n");
        assert_eq!(a.cursor, 12);
        key(&mut a, "?");
        a.insert("inexistente");
        key(&mut a, "<Esc>");
        key(&mut a, "n");
        assert_eq!(a.cursor, 0);
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.contents(), "uno ñ uno ñ uno");
    }
    #[test]
    fn regex_keeps_unicode_nonoverlap_and_zero_width_line_positions() {
        let cancel = AtomicBool::new(false);
        let p = Pattern::new("ññ").unwrap();
        assert_eq!(p.positions("ñññxññ", &cancel).unwrap(), vec![0, 4]);
        assert_eq!(
            p.find("ñññxññ", 0, false, 9999, &cancel).unwrap(),
            Some(4..6)
        );
        assert_eq!(
            Pattern::new("$")
                .unwrap()
                .positions("ab\nxyz\n", &cancel)
                .unwrap(),
            vec![1, 5]
        );
        assert_eq!(
            Pattern::new("missing")
                .unwrap()
                .find("abc", 0, false, 1, &cancel)
                .unwrap(),
            None
        );
        assert!(Pattern::new(&"a".repeat(4097)).is_err());
        cancel.store(true, Ordering::Release);
        assert!(p.positions("ññ", &cancel).is_err());
    }
    #[test]
    fn documented_subset_matches_clean_neovim_reference_cases() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-pattern-reference.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let query = case["query"].as_str().unwrap();
            let actual = Pattern::new(query)
                .unwrap()
                .positions(case["text"].as_str().unwrap(), &AtomicBool::new(false))
                .unwrap();
            let expected: Vec<usize> = case["expected_positions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_u64().unwrap() as usize)
                .collect();
            assert_eq!(actual, expected, "{}: {query}", case["label"]);
        }
        for query in [
            r"x\@=",
            r"\(x\)\1",
            r"x\zs",
            r"\%V",
            r"\v(?i:abc)",
            r"\va+?",
            r"[a&&b]x\z",
            r"a\{10001}",
        ] {
            assert!(crate::vim_pattern::compile(query).is_err(), "{query}");
        }
    }
    #[test]
    fn results_require_same_request_buffer_version_cursor_and_mode() {
        let mut e = Editor::new("abc ABC abc".into());
        e.key("/");
        e.insert("abc");
        assert_eq!(e.key("<Enter>"), Effect::Search);
        let old = e.take_search_job().unwrap();
        e.key("l");
        assert!(!e.apply_search(old.run()));
        assert_eq!(e.cursor, 1);
        e.key("n");
        let old = e.take_search_job().unwrap();
        e.version += 1;
        assert!(!e.apply_search(old.run()));
        assert_eq!(e.cursor, 1);
        e.key("n");
        let old = e.take_search_job().unwrap();
        e.key("/");
        e.insert("abc");
        e.key("<Enter>");
        let latest = e.take_search_job().unwrap();
        assert!(!e.apply_search(old.run()));
        assert!(e.apply_search(latest.run()));
        assert_eq!(e.cursor, 4);
        assert!(!e.dirty());
    }
    #[test]
    fn rapid_repeats_keep_order_and_escape_cancels_the_bounded_queue() {
        let mut e = Editor::new("a a a".into());
        e.key("/");
        e.insert("a");
        e.key("<Enter>");
        e.key("n");
        e.key("n");
        e.key("N");
        let mut jobs = 0;
        while let Some(job) = e.take_search_job() {
            e.apply_search(job.run());
            jobs += 1;
        }
        assert_eq!(jobs, 4);
        assert_eq!(e.cursor, 4);
        assert!(!e.search_busy());
        e.key("n");
        let old = e.take_search_job().unwrap();
        e.key("n");
        e.key("<Esc>");
        assert!(!e.apply_search(old.run()));
        assert!(e.search_followups.is_empty());
        e.key("n");
        for _ in 0..129 {
            e.key("n");
        }
        assert!(!e.search_busy());
        assert!(e.message.contains("128"));
        assert!(!e.dirty());
    }
    #[test]
    fn macro_waits_for_search_before_modifying_the_match() {
        let mut e = Editor::new("a id12 id3".into());
        e.key("q");
        e.key("a");
        e.key("/");
        e.insert(r"id\d\+");
        e.key("<Enter>");
        e.finish_search_test();
        e.key("x");
        e.key("q");
        e.key("u");
        e.key("0");
        assert_eq!(e.key("@"), Effect::None);
        assert_eq!(e.key("a"), Effect::Search);
        assert!(e.search_busy());
        assert_eq!(e.advance_replay(), Effect::None);
        assert_eq!(e.contents(), "a id12 id3");
        e.finish_search_test();
        while e.replaying() {
            e.advance_replay();
        }
        assert_eq!(e.contents(), "a d12 id3");
    }
}
