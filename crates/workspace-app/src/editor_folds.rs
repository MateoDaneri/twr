//! Bounded indentation and versioned parser folds, with explicit closure commands.
//! Initial state is open, matching the pinned foldlevel=99.
//! Edits retain unaffected closed ranges. Four parser languages and Visual ranges
//! are supported; arbitrary expression/manual definitions remain separate work.
use super::*;

const MAX_FOLDS: usize = 4096;
const MAX_DEPTH: u8 = 20;
#[derive(Clone)]
struct Fold {
    rows: Range<usize>,
    closed: bool,
    level: usize,
}
#[derive(Default)]
pub(super) struct Folds {
    definitions: Vec<Fold>,
    valid: bool,
    baseline: Vec<char>,
    // Nonoverlapping hidden rows. The summary row immediately precedes each.
    hidden: Vec<Range<usize>>,
}

impl Editor {
    pub(super) fn manual_shift_fold_snapshot(&self) -> Option<Vec<(Range<usize>, usize, bool)>> {
        if self.fold_language.is_some() {
            return None;
        }
        let folds = self.folds.as_ref().filter(|folds| folds.valid)?;
        folds
            .definitions
            .iter()
            .any(|fold| fold.closed != (fold.level > self.fold_level))
            .then(|| {
                folds
                    .definitions
                    .iter()
                    .map(|fold| (fold.rows.clone(), fold.level, fold.closed))
                    .collect()
            })
    }

    pub(super) fn restore_manual_shift(
        &mut self,
        previous: Vec<(Range<usize>, usize, bool)>,
        shifted: Range<usize>,
    ) {
        let row = self.source_line_at(self.cursor);
        let retained = self.folds.take();
        if self.define_folds().is_err() {
            self.folds = retained;
            return;
        }
        if let Some(folds) = &mut self.folds {
            for index in 0..folds.definitions.len() {
                let (earlier, remaining) = folds.definitions.split_at_mut(index);
                let fold = &mut remaining[0];
                let inherited = earlier
                    .iter()
                    .filter(|parent| {
                        parent.level < fold.level && parent.rows.contains(&fold.rows.start)
                    })
                    .max_by_key(|parent| parent.level)
                    .map(|parent| parent.closed)
                    .unwrap_or(fold.level > self.fold_level);
                fold.closed = previous
                    .iter()
                    .find(|(rows, level, _)| {
                        *level == fold.level && rows.contains(&fold.rows.start)
                    })
                    .map(|(_, _, closed)| *closed)
                    .unwrap_or(inherited);
                if shifted.len() > 1
                    && !previous.iter().any(|(rows, level, _)| {
                        *level == fold.level && rows.contains(&fold.rows.start)
                    })
                    && previous.iter().any(|(rows, level, closed)| {
                        *closed
                            && *level == fold.level
                            && rows.start > fold.rows.start
                            && rows.end <= fold.rows.end
                            && rows.start >= shifted.start
                            && rows.end <= shifted.end
                    })
                {
                    fold.closed = true;
                }
                if shifted.len() == 1 && fold.rows.start == row {
                    fold.closed = false;
                }
            }
        }
        self.refresh_fold_rows();
    }

    pub(super) fn shift_fold_snapshot(&self) -> Option<Vec<(Range<usize>, usize, bool)>> {
        // Parser definitions are asynchronous and must remain versioned.
        if self.fold_language.is_some() {
            return None;
        }
        self.folds
            .as_ref()
            .filter(|folds| {
                folds.valid
                    && folds
                        .definitions
                        .iter()
                        .all(|fold| fold.closed == (fold.level > self.fold_level))
            })
            .map(|folds| {
                folds
                    .definitions
                    .iter()
                    .map(|fold| (fold.rows.clone(), fold.level, fold.closed))
                    .collect()
            })
    }

    pub(super) fn restore_shift_folds(
        &mut self,
        previous: Option<Vec<(Range<usize>, usize, bool)>>,
    ) {
        let Some(previous) = previous else { return };
        let previous: std::collections::HashMap<_, _> = previous
            .into_iter()
            .map(|(rows, level, closed)| ((rows, level), closed))
            .collect();
        let retained = self.folds.take();
        if self.define_folds().is_err() {
            self.folds = retained;
            return;
        }
        if let Some(folds) = &mut self.folds {
            for fold in &mut folds.definitions {
                fold.closed = previous
                    .get(&(fold.rows.clone(), fold.level))
                    .copied()
                    .unwrap_or(fold.level > self.fold_level);
            }
        }
        self.refresh_fold_rows();
    }

    /// A worker result belongs only to this exact supplied buffer version.
    pub fn apply_parser_folds(
        &mut self,
        version: u64,
        mut result: Result<Vec<crate::syntax_folds::Definition>, String>,
    ) -> bool {
        if self.fold_language.is_none() || version != self.version {
            return false;
        }
        if let Ok(definitions) = &result
            && (definitions.len() > MAX_FOLDS
                || definitions.iter().any(|f| {
                    f.rows.end > self.line_starts.len()
                        || f.rows.start.saturating_add(1) >= f.rows.end
                        || f.level == 0
                        || f.level > MAX_DEPTH as usize
                }))
        {
            result = Err("Pliegues sintácticos: resultado fuera del límite".into());
        }
        if result.is_ok() && self.message == "Pliegues sintácticos pendientes; edición disponible"
        {
            self.message.clear();
        }
        self.parser_fold_result = Some((version, result));
        true
    }

    fn define_folds(&mut self) -> Result<(), String> {
        if self.folds.as_ref().is_some_and(|folds| folds.valid) {
            return Ok(());
        }
        if self.text.len() > 2_000_000 {
            return Err("Pliegues limitados a dos millones de caracteres".into());
        }
        let mut definitions = if self.fold_language.is_some() && self.text.is_empty() {
            Vec::new()
        } else if self.fold_language.is_some() {
            if self.text.len() > crate::syntax::MAX_CHARS {
                return Err("Pliegues sintácticos limitados a 131.072 caracteres".into());
            }
            let (_, result) = self
                .parser_fold_result
                .as_ref()
                .filter(|(version, _)| *version == self.version)
                .ok_or("Pliegues sintácticos pendientes; edición disponible")?;
            result
                .as_ref()
                .map_err(Clone::clone)?
                .iter()
                .map(|definition| Fold {
                    rows: definition.rows.clone(),
                    level: definition.level,
                    closed: false,
                })
                .collect()
        } else {
            let mut levels = Vec::with_capacity(self.line_starts.len());
            let mut previous = 0;
            let mut pending = 0;
            for &start in &self.line_starts {
                let end = self.line_end(start);
                let mut column = 0usize;
                let mut significant = None;
                for &ch in &self.text[start..end] {
                    match ch {
                        ' ' => column += 1,
                        '\t' => column += self.tab_padding(column),
                        _ => {
                            significant = Some(ch);
                            break;
                        }
                    }
                }
                if significant.is_none() || significant == Some('#') {
                    levels.push(0u8);
                    pending += 1;
                    continue;
                }
                let level = (column / self.indent_style.width).min(MAX_DEPTH as usize) as u8;
                let begin = levels.len().saturating_sub(pending);
                for ignored in &mut levels[begin..] {
                    *ignored = previous.min(level);
                }
                pending = 0;
                levels.push(level);
                previous = level;
            }
            // Ignored trailing lines meet an implicit level zero.
            let mut definitions = Vec::new();
            let mut opened = Vec::<usize>::new();
            for row in 0..=levels.len() {
                let level = levels.get(row).copied().unwrap_or(0) as usize;
                while opened.len() > level {
                    let start = opened.pop().unwrap();
                    // Default foldminlines=1: one physical line stays displayed.
                    if row.saturating_sub(start) > 1 {
                        if definitions.len() == MAX_FOLDS {
                            return Err(
                                "Pliegues: máximo de 4.096 definiciones; texto conservado".into()
                            );
                        }
                        definitions.push(Fold {
                            rows: start..row,
                            closed: false,
                            level: opened.len() + 1,
                        });
                    }
                }
                while opened.len() < level {
                    opened.push(row);
                }
            }
            definitions
        };
        definitions.sort_by_key(|f| (f.rows.start, std::cmp::Reverse(f.rows.end)));
        if let Some(previous) = &self.folds {
            for definition in &mut definitions {
                definition.closed = previous.definitions.iter().any(|old| {
                    old.closed && old.rows == definition.rows && old.level == definition.level
                });
            }
        }
        let baseline = self.text.clone();
        if baseline.capacity() > 2_000_000 {
            return Err(
                "Caché de pliegues fuera del límite de caracteres; texto conservado".into(),
            );
        }
        self.folds = Some(Folds {
            valid: true,
            baseline,
            definitions,
            hidden: Vec::new(),
        });
        self.refresh_fold_rows();
        Ok(())
    }

    /// Compare the bounded previous text before replacing line_starts.
    /// Only closed folds wholly outside the changed interval survive.
    pub(super) fn retain_folds_after_change(&mut self) {
        let Some(mut folds) = self.folds.take() else {
            return;
        };
        if self.text.len() > 2_000_000 || !folds.definitions.iter().any(|fold| fold.closed) {
            return;
        }
        let prefix = folds
            .baseline
            .iter()
            .zip(&self.text)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = folds.baseline[prefix..]
            .iter()
            .rev()
            .zip(self.text[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let old_end = folds.baseline.len() - suffix;
        let new_end = self.text.len() - suffix;
        let removed = folds.baseline[prefix..old_end]
            .iter()
            .filter(|ch| **ch == '\n')
            .count();
        let added = self.text[prefix..new_end]
            .iter()
            .filter(|ch| **ch == '\n')
            .count();
        let clean_boundary = new_end == 0 || self.text.get(new_end - 1) == Some(&'\n');
        folds.definitions.retain_mut(|fold| {
            if !fold.closed {
                return false;
            }
            let start = self.line_starts[fold.rows.start];
            let end = self
                .line_starts
                .get(fold.rows.end)
                .copied()
                .unwrap_or(folds.baseline.len());
            if old_end < start || (old_end == start && clean_boundary) {
                fold.rows = fold
                    .rows
                    .start
                    .saturating_sub(removed)
                    .saturating_add(added)
                    ..fold.rows.end.saturating_sub(removed).saturating_add(added);
                true
            } else {
                prefix >= end
            }
        });
        if folds.definitions.is_empty() {
            return;
        }
        folds.valid = false;
        if self.text.len() > folds.baseline.capacity() {
            // clone_from may geometrically grow beyond the retained cache cap.
            folds.baseline = self.text.clone();
        } else {
            folds.baseline.clone_from(&self.text);
        }
        if folds.baseline.capacity() > 2_000_000 {
            return;
        }
        self.folds = Some(folds);
        self.refresh_fold_rows();
    }

    pub(super) fn fold_closures(&self) -> Option<Vec<bool>> {
        self.folds
            .as_ref()
            .map(|folds| folds.definitions.iter().map(|fold| fold.closed).collect())
    }

    pub(super) fn restore_fold_closures(&mut self, saved: Option<&[bool]>) {
        if let (Some(folds), Some(saved)) = (&mut self.folds, saved)
            && folds.definitions.len() == saved.len()
        {
            for (fold, closed) in folds.definitions.iter_mut().zip(saved) {
                fold.closed = *closed;
            }
            self.refresh_fold_rows();
        }
    }

    fn refresh_fold_rows(&mut self) {
        let Some(folds) = &mut self.folds else { return };
        folds.hidden.clear();
        for fold in &folds.definitions {
            if !fold.closed {
                continue;
            }
            let hidden = fold.rows.start + 1..fold.rows.end;
            if folds
                .hidden
                .last()
                .is_some_and(|outer| hidden.start >= outer.start && hidden.end <= outer.end)
            {
                continue;
            }
            folds.hidden.push(hidden);
        }
    }

    pub fn source_line_at(&self, offset: usize) -> usize {
        self.line_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1)
    }

    pub fn visible_row(&self, row: usize) -> usize {
        let Some(folds) = &self.folds else { return row };
        let mut removed = 0;
        for hidden in &folds.hidden {
            if row < hidden.start {
                break;
            }
            removed += row.saturating_add(1).min(hidden.end) - hidden.start;
        }
        row.saturating_sub(removed)
    }

    pub fn source_row(&self, visual: usize) -> usize {
        let hidden_rows = self
            .folds
            .as_ref()
            .map_or(0, |folds| folds.hidden.iter().map(Range::len).sum());
        // Clamp in visible space first: the last physical row may itself be hidden.
        let mut row = visual.min(
            self.line_starts
                .len()
                .saturating_sub(hidden_rows)
                .saturating_sub(1),
        );
        if let Some(folds) = &self.folds {
            for hidden in &folds.hidden {
                if row < hidden.start {
                    break;
                }
                row = row.saturating_add(hidden.len());
            }
        }
        row.min(self.line_starts.len().saturating_sub(1))
    }

    pub fn folded_line_count(&self, start: usize) -> Option<usize> {
        let row = self
            .line_starts
            .partition_point(|s| *s <= start)
            .saturating_sub(1);
        self.folds
            .as_ref()?
            .hidden
            .iter()
            .find(|hidden| hidden.start == row + 1)
            .map(|hidden| hidden.len() + 1)
    }

    /// A jump can leave the logical cursor inside a closed fold. Neovim
    /// displays that cursor at the start of the folded summary instead.
    pub fn cursor_on_fold_summary(&self, start: usize) -> bool {
        self.folded_line_count(start).is_some()
            && self.visible_row(self.display_position().0)
                == self.visible_row(self.source_line_at(start))
    }

    pub(super) fn open_cursor_folds(&mut self) {
        self.open_folds_at(self.cursor);
    }

    pub(super) fn open_folds_at(&mut self, offset: usize) {
        let row = self.source_line_at(offset);
        if let Some(folds) = &mut self.folds {
            for fold in &mut folds.definitions {
                if fold.rows.contains(&row) {
                    fold.closed = false;
                }
            }
        }
        self.refresh_fold_rows();
    }

    pub(super) fn folded_first_row(&self, row: usize) -> usize {
        self.source_row(self.visible_row(row))
    }

    pub(super) fn folded_last_row(&self, row: usize) -> usize {
        self.folds
            .as_ref()
            .and_then(|folds| {
                folds
                    .hidden
                    .iter()
                    .find(|hidden| row + 1 >= hidden.start && row < hidden.end)
            })
            .map_or(row, |hidden| hidden.end - 1)
    }

    pub(super) fn closed_fold_rows(&self, row: usize) -> Option<Range<usize>> {
        self.folds
            .as_ref()?
            .hidden
            .iter()
            .find(|hidden| row >= hidden.start.saturating_sub(1) && row < hidden.end)
            .map(|hidden| hidden.start - 1..hidden.end)
    }

    pub(super) fn expand_folded_selection(&self, mut range: Range<usize>) -> Range<usize> {
        if range.is_empty() {
            return range;
        }
        if let Some(rows) = self.closed_fold_rows(self.source_line_at(range.start)) {
            range.start = self.line_starts[rows.start];
        }
        if let Some(rows) = self.closed_fold_rows(self.source_line_at(range.end - 1)) {
            range.end = (self.line_end(self.line_starts[rows.end - 1]) + 1).min(self.text.len());
        }
        range
    }

    pub(super) fn expand_folded_visual_entry(&mut self) {
        if let Some(rows) = self.closed_fold_rows(self.position().0) {
            let column = self.tab_column(self.cursor);
            self.anchor = self.line_starts[rows.start];
            self.cursor = self.line_end(self.line_starts[rows.end - 1]);
            self.desired_column = Some((self.cursor, column));
        }
    }

    pub(super) fn folded_visual_eol(&self) -> bool {
        matches!(self.mode, Mode::Visual | Mode::VisualLine)
            && self
                .closed_fold_rows(self.position().0)
                .is_some_and(|rows| rows.end - 1 == self.position().0)
    }

    pub fn visual_rows(&self) -> Option<Range<usize>> {
        if self.mode == Mode::VisualBlock {
            return self.block_selected_rows();
        }
        matches!(self.mode, Mode::Visual | Mode::VisualLine).then(|| {
            let a = self.source_line_at(self.anchor);
            let b = self.source_line_at(self.cursor);
            a.min(b)..a.max(b).saturating_add(1)
        })
    }

    fn fold_visual_range(&mut self, key: &str) {
        let Some(rows) = self.visual_rows() else {
            return;
        };
        let start = self.folded_first_row(rows.start);
        let end = self
            .folded_last_row(rows.end.saturating_sub(1))
            .saturating_add(1);
        let opening = matches!(key, "o" | "O");
        let recursive = matches!(key, "O" | "C");
        let definitions = &mut self.folds.as_mut().unwrap().definitions;
        if recursive {
            for fold in definitions.iter_mut() {
                if fold.rows.start < end && fold.rows.end > start {
                    fold.closed = !opening;
                }
            }
        } else {
            // Visit only fold boundaries. Each iteration advances, and at most
            // 4096 definitions plus their gaps are inspected, never all rows.
            let mut row = start;
            while row < end {
                let closed = definitions
                    .iter()
                    .filter(|fold| fold.closed && fold.rows.contains(&row))
                    .max_by_key(|fold| (fold.rows.len(), std::cmp::Reverse(fold.level)))
                    .map(|fold| (fold.rows.clone(), fold.level));
                let next = if opening {
                    definitions
                        .iter()
                        .enumerate()
                        .filter(|(_, fold)| fold.closed && fold.rows.contains(&row))
                        .max_by_key(|(_, fold)| (fold.rows.len(), std::cmp::Reverse(fold.level)))
                        .map(|(index, _)| index)
                } else {
                    definitions
                        .iter()
                        .enumerate()
                        .filter(|(_, fold)| {
                            !fold.closed
                                && fold.rows.contains(&row)
                                && closed.as_ref().is_none_or(|(closed, level)| {
                                    fold.rows.start <= closed.start
                                        && fold.rows.end >= closed.end
                                        && fold.level < *level
                                })
                        })
                        .min_by_key(|(_, fold)| (fold.rows.len(), std::cmp::Reverse(fold.level)))
                        .map(|(index, _)| index)
                };
                if let Some(index) = next {
                    definitions[index].closed = !opening;
                    row = definitions[index].rows.end;
                } else {
                    row = closed
                        .map(|(range, _)| range.end)
                        .or_else(|| {
                            definitions
                                .iter()
                                .filter(|fold| fold.rows.start > row)
                                .map(|fold| fold.rows.start)
                                .min()
                        })
                        .unwrap_or(end);
                }
            }
        }
        self.mode = Mode::Normal;
        self.block = None;
        self.cursor = self.line_starts[start.min(self.line_starts.len() - 1)];
        self.desired_column = None;
        self.refresh_fold_rows();
    }

    pub(super) fn fold_command(&mut self, key: &str, count: usize) -> bool {
        if !matches!(
            key,
            "o" | "O" | "c" | "C" | "a" | "A" | "v" | "m" | "r" | "M" | "R" | "x" | "X"
        ) {
            return false;
        }
        let visual = matches!(
            self.mode,
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        );
        if self.mode != Mode::Normal && !visual {
            return true;
        }
        if let Err(error) = self.define_folds() {
            self.message = error;
            return true;
        }
        if visual && matches!(key, "o" | "O" | "c" | "C") {
            self.fold_visual_range(key);
            return true;
        }
        let row = self.display_position().0;
        if matches!(key, "R" | "M" | "m" | "r" | "x" | "X") {
            self.fold_level = match key {
                "M" => 0,
                "R" => self
                    .folds
                    .as_ref()
                    .unwrap()
                    .definitions
                    .iter()
                    .map(|fold| fold.level)
                    .max()
                    .unwrap_or(0),
                "m" => self.fold_level.saturating_sub(count),
                "r" => self.fold_level.saturating_add(count),
                _ => self.fold_level,
            };
            for fold in &mut self.folds.as_mut().unwrap().definitions {
                fold.closed = fold.level > self.fold_level;
            }
            if key == "x" {
                self.open_folds_at(self.line_starts[row]);
            }
        } else if key == "v" {
            self.open_cursor_folds();
        } else {
            let definitions = &mut self.folds.as_mut().unwrap().definitions;
            let closed = definitions
                .iter()
                .any(|f| f.closed && f.rows.contains(&row));
            let opening = matches!(key, "o" | "O") || (matches!(key, "a" | "A") && closed);
            let recursive = matches!(key, "O" | "C" | "A");
            let mut changed = false;
            if recursive && opening {
                // Open the entire outermost closed fold under the cursor,
                // including sibling descendants that do not contain its row.
                if let Some(rows) = definitions
                    .iter()
                    .filter(|fold| fold.closed && fold.rows.contains(&row))
                    .max_by_key(|fold| fold.rows.len())
                    .map(|fold| fold.rows.clone())
                {
                    for fold in definitions.iter_mut() {
                        if fold.rows.start >= rows.start && fold.rows.end <= rows.end {
                            changed |= fold.closed;
                            fold.closed = false;
                        }
                    }
                }
            } else {
                for _ in 0..if recursive { MAX_DEPTH as usize } else { count } {
                    let indices = definitions
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| f.rows.contains(&row) && f.closed == opening);
                    let next = if opening {
                        indices.min_by_key(|(_, f)| {
                            (std::cmp::Reverse(f.rows.len()), f.level, f.rows.start)
                        })
                    } else {
                        // Already closed children do not prevent closing a parent.
                        // Equal ranges represent distinct indentation levels.
                        indices.min_by_key(|(_, f)| {
                            (f.rows.len(), std::cmp::Reverse(f.level), f.rows.start)
                        })
                    }
                    .map(|(i, _)| i);
                    let Some(index) = next else { break };
                    definitions[index].closed = !opening;
                    changed = true;
                }
            }
            if !changed {
                self.message = "No hay pliegue en el cursor".into();
            }
        }
        self.refresh_fold_rows();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEXT: &str =
        "top\n  alpha\n    deepA\n    deepB\n  omega\n\ntail\n  secondA\n  secondB\nend";
    fn keys(e: &mut Editor, sequence: &str) {
        for ch in sequence.chars() {
            e.key(&ch.to_string());
        }
    }
    #[test]
    fn manual_fold_shift_reference_states() {
        let references: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-fold-indent-shift-reference.json"
        ))
        .unwrap();
        let mut failures = Vec::new();
        for (index, scenario) in references.as_array().unwrap().iter().enumerate() {
            let mut editor = Editor::new(TEXT.into());
            for state in scenario.as_array().unwrap() {
                let key = state["keys"].as_str().unwrap();
                keys(&mut editor, key);
                while editor.replaying() {
                    editor.advance_replay();
                }
                let text = state["text"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|line| line.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join("\n");
                let row = state["line"].as_u64().unwrap() as usize - 1;
                let closed: Vec<_> = (0..10)
                    .map(|i| editor.closed_fold_rows(i).map(|r| (r.start + 1, r.end)))
                    .collect();
                let reference: Vec<_> = state["folds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| {
                        let start = f[1].as_i64().unwrap();
                        (start > 0).then(|| (start as usize, f[2].as_u64().unwrap() as usize))
                    })
                    .collect();
                if editor.contents() != text || editor.position().0 != row || closed != reference {
                    failures.push(format!(
                        "scenario {} after {key}: row {}/{} folds {:?}/{:?} text {}",
                        index + 1,
                        editor.position().0 + 1,
                        row + 1,
                        closed,
                        reference,
                        editor.contents() == text
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
    #[test]
    fn word_operators_on_closed_folds_match_clean_neovim() {
        for (sequence, expected, register, linewise) in [
            (
                "zMjdw",
                "top\nalpha\n    deepA\n    deepB\n  omega\n\ntail\n  secondA\n  secondB\nend",
                "  ",
                false,
            ),
            ("zMjyw", TEXT, "  ", false),
            (
                "zMjd2w",
                "top\n\n    deepA\n    deepB\n  omega\n\ntail\n  secondA\n  secondB\nend",
                "  alpha",
                false,
            ),
            (
                "zMjldw",
                "top\n alpha\n    deepA\n    deepB\n  omega\n\ntail\n  secondA\n  secondB\nend",
                " ",
                false,
            ),
            (
                "zMdw",
                "\n  alpha\n    deepA\n    deepB\n  omega\n\ntail\n  secondA\n  secondB\nend",
                "top",
                false,
            ),
            (
                "zM2dw",
                "    deepA\n    deepB\n  omega\n\ntail\n  secondA\n  secondB\nend",
                "top\n  alpha\n",
                true,
            ),
        ] {
            let mut editor = Editor::new(TEXT.into());
            keys(&mut editor, sequence);
            assert_eq!(editor.contents(), expected, "{sequence}");
            assert_eq!(
                editor.registers.get('"'),
                Some((register.into(), linewise)),
                "{sequence}"
            );
        }
    }
    #[test]
    fn shift_operators_expand_closed_folds_like_clean_neovim() {
        let shifted = "top\n    alpha\n      deepA\n      deepB\n    omega\n\ntail\n  secondA\n  secondB\nend";
        let backward = "  top\n    alpha\n      deepA\n      deepB\n    omega\n\ntail\n  secondA\n  secondB\nend";
        let left = "top\nalpha\n  deepA\n  deepB\nomega\n\ntail\n  secondA\n  secondB\nend";
        for (sequence, expected, row) in [
            ("zMj>>", shifted, 1),
            ("zMj2>>", shifted, 1),
            ("zMj>j", shifted, 1),
            ("zMj>k", backward, 0),
            ("zMj<<", left, 1),
        ] {
            let mut e = Editor::new(TEXT.into());
            keys(&mut e, sequence);
            assert_eq!(e.contents(), expected, "{sequence}");
            assert_eq!(e.position().0, row, "{sequence}");
            e.key("u");
            assert_eq!(e.contents(), TEXT, "undo {sequence}");
        }
    }
    #[test]
    fn counted_fold_shift_dot_replays_on_visible_lines() {
        let mut e = Editor::new(TEXT.into());
        keys(&mut e, "zMj2>>j.");
        while e.replaying() {
            e.advance_replay();
        }
        assert_eq!(
            e.contents(),
            "top\n    alpha\n      deepA\n      deepB\n    omega\n\n  tail\n  secondA\n  secondB\nend"
        );
        assert_eq!(e.position().0, 5);
    }
    #[test]
    fn shift_after_manual_fold_open_matches_clean_neovim() {
        for (sequence, expected, row) in [
            (
                "zMjzo>>j.",
                "top\n    alpha\n      deepA\n    deepB\n  omega\n\ntail\n  secondA\n  secondB\nend",
                2,
            ),
            (
                "zMjzoj>>j.",
                "top\n  alpha\n      deepA\n        deepB\n  omega\n\ntail\n  secondA\n  secondB\nend",
                3,
            ),
            (
                "zMjzo2>>j.",
                "top\n    alpha\n      deepA\n      deepB\n    omega\n\ntail\n  secondA\n  secondB\nend",
                4,
            ),
        ] {
            let mut e = Editor::new(TEXT.into());
            keys(&mut e, sequence);
            while e.replaying() {
                e.advance_replay();
            }
            assert_eq!(e.contents(), expected, "{sequence}");
            assert_eq!(e.position().0, row, "{sequence}");
        }
    }
    #[test]
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "Expected elements are hidden row intervals, not row numbers"
    )]
    fn indent_folds_follow_clean_neovim_nested_open_close_and_vertical_reference() {
        for (sequence, row, hidden) in [
            ("zc", 2, vec![3..4]),
            ("zczo", 2, vec![]),
            ("zczc", 2, vec![2..5]),
            ("2zc", 2, vec![2..5]),
            ("zM2zo", 2, vec![8..9]),
            ("zczczo", 2, vec![3..4]),
            ("zczczO", 2, vec![]),
            ("za", 2, vec![3..4]),
            ("zaza", 2, vec![]),
            ("zM", 2, vec![2..5, 8..9]),
            ("zMzR", 2, vec![]),
            ("zcj", 4, vec![3..4]),
            ("zck", 1, vec![3..4]),
        ] {
            let mut e = Editor::new(TEXT.into());
            e.goto(2, 0);
            keys(&mut e, sequence);
            assert_eq!(e.position().0, row, "{sequence}");
            assert_eq!(e.folds.as_ref().unwrap().hidden, hidden, "{sequence}");
            assert!(!e.dirty());
        }
        let mut e = Editor::new(TEXT.into());
        keys(&mut e, "zMjj");
        assert_eq!(e.position().0, 5);
        assert_eq!(
            e.visible_lines(0, 5)
                .iter()
                .map(|r| e.text[r.clone()].iter().collect::<String>())
                .collect::<Vec<_>>(),
            ["top", "  alpha", "", "tail", "  secondA"]
        );
    }
    #[test]
    fn fold_line_yank_delete_insert_and_invalidation_preserve_content_and_undo() {
        for sequence in ["zczcdd", "zMx", "zM2x"] {
            let mut hidden_cursor = Editor::new(TEXT.into());
            hidden_cursor.goto(2, 0);
            keys(&mut hidden_cursor, sequence);
            assert_eq!(
                hidden_cursor.contents(),
                "top\n\ntail\n  secondA\n  secondB\nend",
                "{sequence}"
            );
            hidden_cursor.key("u");
            assert_eq!(hidden_cursor.contents(), TEXT);
        }
        let mut e = Editor::new(TEXT.into());
        e.goto(1, 0);
        keys(&mut e, "zMyy");
        assert_eq!(
            e.registers.get('"').unwrap().0,
            "  alpha\n    deepA\n    deepB\n  omega\n"
        );
        assert!(!e.dirty());
        keys(&mut e, "dd");
        assert_eq!(e.contents(), "top\n\ntail\n  secondA\n  secondB\nend");
        e.key("u");
        assert_eq!(e.contents(), TEXT);
        assert!(!e.dirty());
        e.goto(2, 0);
        keys(&mut e, "zMi");
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![8..9]);
        e.insert("Q");
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![8..9]);
        e.key("<Esc>");
        e.key("u");
        assert_eq!(e.contents(), TEXT);
    }
    #[test]
    fn folded_pages_and_accepted_search_use_visible_rows_without_losing_other_folds() {
        let mut e = Editor::new(TEXT.into());
        keys(&mut e, "zM");
        e.jump_viewport(0, 0, 3, 80);
        assert_eq!(e.key("<C-f>"), Effect::Viewport(1));
        assert_eq!(e.position().0, 5);
        e.jump_viewport(1, 0, 3, 80);
        assert_eq!(e.key("<C-b>"), Effect::Viewport(0));
        assert_eq!(e.position().0, 1);
        e.key("/");
        e.insert("deepB");
        e.key("<Enter>");
        e.finish_search_test();
        assert_eq!(e.position().0, 3);
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![8..9]);
        assert!(!e.dirty());
    }

    #[test]
    fn jump_inside_a_closed_fold_places_caret_on_summary_without_moving_logical_cursor() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-fold-ui.json"
        ))
        .unwrap();
        let mut lines = vec![
            "synthetic-1".to_owned(),
            "synthetic-2".to_owned(),
            "  parent".to_owned(),
        ];
        lines.extend((4..=10).map(|i| format!("    child-{i}")));
        lines.extend((11..=30).map(|i| format!("synthetic-{i}")));
        let mut editor = Editor::new(lines.join("\n"));
        keys(&mut editor, "zM");
        let logical = reference["cursor"][0].as_u64().unwrap() as usize - 1;
        let fold_start = reference["fold_start"].as_u64().unwrap() as usize - 1;
        editor.goto(logical, 0);
        assert_eq!(editor.position().0, logical);
        assert_eq!(
            editor.visible_row(logical) + 1,
            reference["screen"]["row"].as_u64().unwrap() as usize
        );
        assert!(editor.cursor_on_fold_summary(editor.line_starts[fold_start]));
        assert!(!editor.cursor_on_fold_summary(editor.line_starts[0]));
        let offset = editor.cursor;
        assert_eq!(editor.wrapped_cursor_at(offset, 60, false).unwrap(), None);
        let projected = editor
            .wrapped_display_cursor_at(offset, 60, false)
            .unwrap()
            .unwrap();
        assert_eq!(projected.line, fold_start);
        assert_eq!(projected.scalar, 0);
        let top = super::wrap::WrappedTop {
            source_line: 0,
            visual_row: 0,
        };
        let top = editor
            .follow_wrapped_cursor_with_margin(top, projected, 15, 60, 0)
            .unwrap();
        let viewport = editor.wrapped_viewport(top, 15, 60).unwrap();
        let cell = editor.wrapped_cursor_cell(&viewport, projected).unwrap();
        assert_eq!(
            cell.row + 1,
            reference["screen"]["row"].as_u64().unwrap() as usize
        );
        let pane = crate::editor_panes::PaneWrap {
            enabled: true,
            ..Default::default()
        };
        assert_eq!(
            crate::editor_panes::wrapped_navigation_cell(&editor, &pane, 15, 60),
            Some((cell.row, 0))
        );
        assert_eq!(editor.position().0, logical);
        editor.goto(10, 0);
        assert!(!editor.cursor_on_fold_summary(editor.line_starts[fold_start]));
    }

    #[test]
    fn edits_rebase_unaffected_closed_folds_and_rebuild_definitions_on_demand() {
        let mut e = Editor::new(TEXT.into());
        e.goto(1, 0);
        keys(&mut e, "zMdd");
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![4..5]);
        e.key("u");
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![8..9]);
        keys(&mut e, "zc");
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![2..5, 8..9]);
        e.goto(0, 0);
        e.registers.capture('a', 'y', "new\n".into(), true).unwrap();
        keys(&mut e, "\"ap");
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![3..6, 9..10]);
        e.key("u");
        assert_eq!(e.contents(), TEXT);
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![2..5, 8..9]);
        e.goto(2, 0);
        e.key("i");
        e.insert("Q\n");
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![9..10]);
        e.key("<Esc>");
        e.key("u");
        assert_eq!(e.contents(), TEXT);
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![8..9]);
        assert!(!e.dirty());
    }

    #[test]
    fn growing_large_buffer_keeps_fold_baseline_capacity_within_retained_bound() {
        let mut e = Editor::new(format!("{}\n  a\n  b\nend", "x".repeat(1_100_000)));
        keys(&mut e, "zM");
        e.key("i");
        e.insert("Q");
        let folds = e.folds.as_ref().unwrap();
        assert!(folds.baseline.capacity() <= 2_000_000);
        assert_eq!(folds.hidden, vec![2..3]);
        assert_eq!(folds.baseline, e.text);
    }

    #[test]
    fn provisional_search_reveals_match_restores_on_escape_and_keeps_acceptance() {
        let mut e = Editor::new(TEXT.into());
        keys(&mut e, "zM");
        for query in ["deepB", "secondB"] {
            e.key("/");
            e.insert(query);
            e.finish_search_test();
            let expected =
                std::iter::once(if query == "deepB" { 8..9 } else { 2..5 }).collect::<Vec<_>>();
            assert_eq!(e.folds.as_ref().unwrap().hidden, expected);
            assert_eq!(e.position().0, 0);
            e.key("<Esc>");
            assert_eq!(e.folds.as_ref().unwrap().hidden, vec![2..5, 8..9]);
        }
        e.key("/");
        e.insert("deepB");
        e.finish_search_test();
        e.key("<Enter>");
        assert_eq!(e.position().0, 3);
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![8..9]);
        assert!(!e.dirty());
    }

    #[test]
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "Expected elements are hidden row intervals"
    )]
    fn fold_levels_and_reapplication_match_clean_neovim_reference() {
        for (sequence, level, hidden) in [
            ("zm", 98, vec![]),
            ("zRzm", 1, vec![3..4]),
            ("zMzr", 1, vec![3..4]),
            ("zMzrzc", 1, vec![2..5]),
            ("zMzozx", 0, vec![8..9]),
            ("zMzozX", 0, vec![2..5, 8..9]),
            ("zMzrzm", 0, vec![2..5, 8..9]),
            ("zM2zr", 2, vec![]),
            ("zR2zm", 0, vec![2..5, 8..9]),
        ] {
            let mut e = Editor::new(TEXT.into());
            e.goto(2, 0);
            keys(&mut e, sequence);
            assert_eq!(e.fold_level, level, "{sequence}");
            assert_eq!(e.folds.as_ref().unwrap().hidden, hidden, "{sequence}");
            assert!(!e.dirty());
        }
    }

    #[test]
    fn fold_level_survives_cache_invalidation_and_reapplies_to_restored_text() {
        let mut e = Editor::new(TEXT.into());
        e.goto(1, 0);
        keys(&mut e, "zMdd");
        e.key("u");
        assert_eq!(e.fold_level, 0);
        keys(&mut e, "zX");
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![2..5, 8..9]);
        keys(&mut e, "zR");
        e.key("i");
        e.insert("Q");
        assert!(e.folds.is_none());
        assert_eq!(e.fold_level, 2);
        e.key("<Esc>");
        e.key("u");
        keys(&mut e, "zm");
        assert_eq!(e.fold_level, 1);
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![3..4]);
        assert!(!e.dirty());
    }

    #[test]
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "Expected elements are hidden row intervals"
    )]
    fn visual_fold_ranges_and_mode_transitions_match_clean_neovim() {
        for (sequence, start, mode, row, hidden) in [
            ("Vjzc", 2, Mode::Normal, 2, vec![3..4]),
            ("Vjjzc", 1, Mode::Normal, 1, vec![2..5]),
            ("Vjjzczo", 1, Mode::Normal, 1, vec![]),
            ("Vjjzczo", 2, Mode::Normal, 2, vec![3..4]),
            ("VjjzCzo", 1, Mode::Normal, 1, vec![3..4]),
            ("VGzc", 1, Mode::Normal, 1, vec![2..5, 8..9]),
            ("zMVGzo", 1, Mode::Normal, 1, vec![3..4]),
            ("zMVGzO", 1, Mode::Normal, 1, vec![]),
            ("Vjza", 2, Mode::VisualLine, 3, vec![3..4]),
            ("VjzA", 2, Mode::VisualLine, 3, vec![2..5]),
            ("VGzM", 1, Mode::VisualLine, 9, vec![2..5, 8..9]),
            ("zMVGzr", 1, Mode::VisualLine, 9, vec![3..4]),
            ("Vjzx", 2, Mode::VisualLine, 3, vec![]),
            ("VjzX", 2, Mode::VisualLine, 3, vec![]),
            ("vjzc", 2, Mode::Normal, 2, vec![3..4]),
            ("<C-V>jzc", 2, Mode::Normal, 2, vec![3..4]),
        ] {
            let mut e = Editor::new(TEXT.into());
            e.goto(start, 0);
            for (index, part) in sequence.split("<C-V>").enumerate() {
                if index > 0 {
                    e.key("<C-v>");
                }
                keys(&mut e, part);
            }
            assert_eq!((e.mode, e.position().0), (mode, row), "{sequence}");
            assert_eq!(e.folds.as_ref().unwrap().hidden, hidden, "{sequence}");
            assert_eq!(e.contents(), TEXT);
            assert!(!e.dirty());
        }
    }

    #[test]
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "Expected elements are hidden row intervals"
    )]
    fn visual_range_counts_follow_reference_and_toggles_keep_the_count() {
        for (sequence, start, mode, row, hidden) in [
            ("Vj2zc", 2, Mode::Normal, 2, vec![3..4]),
            ("Vjj2zczo", 1, Mode::Normal, 1, vec![]),
            ("zMVG2zo", 1, Mode::Normal, 1, vec![3..4]),
            ("zMVG2zo", 2, Mode::Normal, 1, vec![3..4]),
            ("zMVG2zO", 1, Mode::Normal, 1, vec![]),
            ("Vj2za", 2, Mode::VisualLine, 3, vec![2..5]),
            ("Vj2zC", 2, Mode::Normal, 2, vec![2..5]),
            ("V2kzczo", 3, Mode::Normal, 1, vec![]),
        ] {
            let mut e = Editor::new(TEXT.into());
            e.goto(start, 0);
            keys(&mut e, sequence);
            assert_eq!((e.mode, e.position().0), (mode, row), "{sequence}");
            assert_eq!(e.folds.as_ref().unwrap().hidden, hidden, "{sequence}");
            assert_eq!(e.contents(), TEXT);
        }
    }

    #[test]
    fn folded_visual_entries_include_all_rows_and_keep_character_line_block_register_kind() {
        let entire = "  alpha\n    deepA\n    deepB\n  omega\n";
        for (sequence, column, expected, kind) in [
            ("zMVy", 0, entire, registers::Kind::Line),
            ("zMvy", 0, entire, registers::Kind::Character),
            ("zMVy", 3, entire, registers::Kind::Line),
            ("zMvy", 3, entire, registers::Kind::Character),
            (
                "zMVjy",
                0,
                "  alpha\n    deepA\n    deepB\n  omega\n\n",
                registers::Kind::Line,
            ),
            (
                "zMvjy",
                0,
                "  alpha\n    deepA\n    deepB\n  omega\n\n",
                registers::Kind::Character,
            ),
            ("zM<C-V>jy", 0, " \n \n \n \n ", registers::Kind::Block(1)),
            (
                "zM<C-V>y",
                3,
                "  alpha \n    deep\n    deep\n  omega ",
                registers::Kind::Block(8),
            ),
        ] {
            let mut e = Editor::new(TEXT.into());
            e.goto(2, column);
            for (index, part) in sequence.split("<C-V>").enumerate() {
                if index > 0 {
                    e.key("<C-v>");
                }
                keys(&mut e, part);
            }
            assert_eq!(
                e.registers.get_kind('"').unwrap(),
                (expected.into(), kind),
                "{sequence}"
            );
            assert_eq!(e.contents(), TEXT);
            assert!(!e.dirty());
        }
        for sequence in ["zMVd", "zMvd"] {
            let mut e = Editor::new(TEXT.into());
            e.goto(2, 0);
            keys(&mut e, sequence);
            assert_eq!(
                e.contents(),
                "top\n\ntail\n  secondA\n  secondB\nend",
                "{sequence}"
            );
            e.key("u");
            assert_eq!(e.contents(), TEXT);
        }
        let mut e = Editor::new(TEXT.into());
        e.goto(2, 0);
        keys(&mut e, "zMvyzRgv");
        assert_eq!((e.mode, e.position()), (Mode::Visual, (4, 7)));
        assert_eq!(e.selection(), Some(e.line_starts[1]..e.line_starts[5]));
    }

    #[test]
    fn folded_visual_entry_keeps_the_original_vertical_column() {
        let text = TEXT.replace("tail", "tail-long-line");
        for (sequence, mode) in [("zMvjj", Mode::Visual), ("zMVjj", Mode::VisualLine)] {
            let mut e = Editor::new(text.clone());
            e.goto(2, 3);
            keys(&mut e, sequence);
            assert_eq!((e.mode, e.position()), (mode, (6, 3)), "{sequence}");
            e.key("y");
            let expected = if mode == Mode::Visual {
                "  alpha\n    deepA\n    deepB\n  omega\n\ntail"
            } else {
                "  alpha\n    deepA\n    deepB\n  omega\n\ntail-long-line\n"
            };
            assert_eq!(e.registers.get_kind('"').unwrap().0, expected);
            assert_eq!(e.contents(), text);
        }
    }

    #[test]
    fn parser_results_are_versioned_and_keep_closed_selection_semantics() {
        let text = "fn first() {\nif true {\nprintln!(\"ñ\");\n}\n}\nfn second() {\nlet x = 1;\n}";
        let mut e = Editor::new(text.into()).for_path(Path::new("fixture.rs"));
        keys(&mut e, "zM");
        assert!(e.message.contains("pendientes"));
        assert!(e.folds.is_none());
        let mut engine = crate::syntax_folds::Engine::default();
        let definitions = engine
            .compute(
                crate::syntax::Language::Rust,
                text,
                &std::sync::atomic::AtomicUsize::new(0),
            )
            .unwrap();
        assert!(!e.apply_parser_folds(e.version + 1, Ok(definitions.clone())));
        assert!(e.apply_parser_folds(e.version, Ok(definitions)));
        keys(&mut e, "zM");
        assert_eq!(e.folded_line_count(0), Some(5));
        assert_eq!(e.folded_line_count(e.line_starts[5]), Some(3));
        keys(&mut e, "Vy");
        assert_eq!(
            e.registers.get_kind('"').unwrap(),
            (
                "fn first() {\nif true {\nprintln!(\"ñ\");\n}\n}\n".into(),
                registers::Kind::Line
            )
        );
        assert_eq!(e.contents(), text);
        assert!(!e.dirty());
        let old_version = e.version;
        e.replace_formatted(format!("{text}\n"));
        assert!(!e.apply_parser_folds(old_version, Ok(Vec::new())));
        keys(&mut e, "zM");
        assert!(e.message.contains("pendientes"));
        let updated = engine
            .compute(
                crate::syntax::Language::Rust,
                &e.contents(),
                &std::sync::atomic::AtomicUsize::new(0),
            )
            .unwrap();
        assert!(e.apply_parser_folds(e.version, Ok(updated)));
        assert!(!e.message.contains("pendientes"));
        keys(&mut e, "zM");
        assert_eq!(e.folded_line_count(0), Some(5));
    }

    #[test]
    fn parser_fold_size_and_empty_buffers_do_not_wait_for_unrequested_workers() {
        let mut empty = Editor::new(String::new()).for_path(Path::new("empty.rs"));
        keys(&mut empty, "zM");
        assert!(empty.folds.as_ref().unwrap().definitions.is_empty());
        assert!(!empty.message.contains("pendientes"));
        let mut large =
            Editor::new("x".repeat(crate::syntax::MAX_CHARS + 1)).for_path(Path::new("large.rs"));
        keys(&mut large, "zM");
        assert!(large.message.contains("131.072"));
        assert!(large.folds.is_none());
        assert!(!large.dirty());
    }

    #[test]
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "Expected elements are hidden row intervals"
    )]
    fn recursive_open_includes_sibling_children_but_closing_preserves_other_branches() {
        let text =
            "fn first() {\nif true {\na();\n}\nif false {\nb();\n}\n}\nfn second() {\nc();\n}";
        for (sequence, row, hidden) in [
            ("zMzO", 0, vec![9..11]),
            ("zMzozO", 0, vec![2..4, 5..7, 9..11]),
            ("zRzCzo", 0, vec![]),
            ("zRzAzo", 0, vec![]),
            ("zRzCzo", 1, vec![2..4]),
            ("zRzAzo", 1, vec![2..4]),
            ("zMzA", 0, vec![9..11]),
        ] {
            let mut e = Editor::new(text.into()).for_path(Path::new("branches.rs"));
            let mut engine = crate::syntax_folds::Engine::default();
            let definitions = engine
                .compute(
                    crate::syntax::Language::Rust,
                    text,
                    &std::sync::atomic::AtomicUsize::new(0),
                )
                .unwrap();
            e.apply_parser_folds(e.version, Ok(definitions));
            e.goto(row, 0);
            keys(&mut e, sequence);
            assert_eq!(
                e.folds.as_ref().unwrap().hidden,
                hidden,
                "{sequence} row{row}"
            );
            assert_eq!(e.contents(), text);
            assert!(!e.dirty());
        }
    }

    #[test]
    fn folded_eof_never_reappears_beyond_the_last_visible_summary() {
        let mut e = Editor::new("top\n  a\n  b".into());
        keys(&mut e, "zM");
        assert_eq!(e.source_row(999), 1);
        let visible: Vec<String> = e
            .visible_lines(0, 100)
            .into_iter()
            .map(|range| e.text[range].iter().collect())
            .collect();
        assert_eq!(visible, ["top", "  a"]);
        keys(&mut e, "j");
        let cursor = e.cursor;
        keys(&mut e, "jj");
        assert_eq!(e.cursor, cursor);
        let mut trailing = Editor::new("top\n  a\n  b\n".into());
        keys(&mut trailing, "zM");
        assert_eq!(trailing.source_row(999), 3);
        assert_eq!(trailing.visible_lines(0, 100).len(), 3);
    }

    #[test]
    fn ignored_lines_tabs_limits_and_display_rows_are_bounded() {
        let mut e = Editor::new("top\n  a\n\n  #ignored\n\tb\nend".into());
        keys(&mut e, "zM");
        assert_eq!(e.folds.as_ref().unwrap().hidden, vec![2..5]);
        for (source, visual) in [(0, 0), (1, 1), (2, 1), (3, 1), (4, 1), (5, 2)] {
            assert_eq!(e.visible_row(source), visual);
        }
        assert_eq!(e.source_row(2), 5);
        assert_eq!(e.folded_line_count(e.line_starts[1]), Some(4));
        let mut large = Editor::new("  x\n  y\nend\n".repeat(MAX_FOLDS + 1));
        keys(&mut large, "zM");
        assert!(large.folds.is_none());
        assert!(large.message.contains("4.096"));
        assert!(!large.dirty());
    }
}
