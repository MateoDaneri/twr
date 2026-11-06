//! Bounded exact/smartcase label jump, using Flash's pinned label order.
//! This initial adapter covers one rendered editor, not remote or tree ranges.
use super::*;

#[derive(Clone)]
pub(super) struct Target {
    start: usize,
    end: usize,
    label: Option<char>,
}
pub(super) struct Jump {
    query: String,
    origin: usize,
    operator: Option<char>,
    ranges: Vec<Range<usize>>,
    targets: Vec<Target>,
    labels: std::collections::BTreeMap<usize, char>,
}
impl Editor {
    pub fn jump_viewport(&mut self, top: usize, left: usize, rows: usize, columns: usize) {
        self.jump_view = (top, left, rows.min(100), columns.min(200));
    }
    pub fn jump_label(&self, position: usize) -> Option<char> {
        self.jump.as_ref()?.labels.get(&position).copied()
    }
    pub fn jump_prompt(&self) -> Option<String> {
        self.jump
            .as_ref()
            .map(|s| format!("Salto /{} · etiqueta o Enter · Esc cancelar", s.query))
    }
    pub(super) fn begin_jump(&mut self) {
        let (top, left, rows, width) = self.jump_view;
        let ranges = self
            .visible_lines(top, rows)
            .into_iter()
            .filter(|r| self.folded_line_count(r.start).is_none())
            .filter_map(|r| {
                let pieces = self.display_cells(&self.text[r.clone()], left, width, 0);
                let mut full = pieces.iter().filter(|piece| !piece.clipped);
                let first = full.next()?;
                let start = r.start + first.source.start;
                let end = r.start + full.next_back().unwrap_or(first).source.end;
                (start < end).then_some(start..end)
            })
            .collect();
        self.jump = Some(Jump {
            query: String::new(),
            origin: self.cursor,
            operator: self.pending.chars().next(),
            ranges,
            targets: Vec::new(),
            labels: Default::default(),
        });
        self.pending.clear();
        self.count = 0;
    }
    pub(super) fn jump_key(&mut self, key: &str) {
        let Some(mut state) = self.jump.take() else {
            return;
        };
        if key == "<Esc>" {
            self.message = "Salto cancelado".into();
            return;
        }
        let ch = key.chars().next().filter(|_| key.chars().count() == 1);
        let target = if key == "<Enter>" {
            state
                .targets
                .iter()
                .find(|t| t.start >= state.origin)
                .or(state.targets.first())
        } else {
            state
                .targets
                .iter()
                .find(|t| t.label.is_some() && t.label == ch)
        }
        .map(|t| t.start);
        if let Some(target) = target {
            if let Some(op) = state.operator {
                if target != state.origin {
                    self.operate(
                        op,
                        target.min(state.origin)..target.max(state.origin),
                        false,
                    );
                }
            } else {
                self.cursor = target;
                self.clamp();
            }
            self.message.clear();
            return;
        }
        if key == "<BS>" {
            state.query.pop();
        } else if key == "<Space>" {
            state.query.push(' ');
        } else if let Some(ch) = ch.filter(|c| !c.is_control()) {
            if state.query.chars().count() < 64 {
                state.query.push(ch);
            }
        } else {
            self.jump = Some(state);
            return;
        }
        let pattern: Vec<char> = state.query.chars().collect();
        let sensitive = pattern.iter().any(|c| c.is_uppercase());
        let previous = std::mem::take(&mut state.targets);
        state.labels.clear();
        if !pattern.is_empty() {
            for range in &state.ranges {
                for start in range.clone() {
                    let end = start + pattern.len();
                    if end <= range.end
                        && self.text[start..end].iter().zip(&pattern).all(|(a, b)| {
                            a == b || (!sensitive && a.to_lowercase().eq(b.to_lowercase()))
                        })
                    {
                        state.targets.push(Target {
                            start,
                            end,
                            label: None,
                        });
                    }
                }
            }
            // Don't assign a label that could extend the query at any match.
            let next: std::collections::BTreeSet<char> = state
                .targets
                .iter()
                .filter_map(|t| self.text.get(t.end).copied())
                .collect();
            let mut labels: Vec<_> = "asdfghjklqwertyuiopzxcvbnmASDFGHJKLQWERTYUIOPZXCVBNM"
                .chars()
                .filter(|c| !next.contains(c))
                .collect();
            let mut order: Vec<_> = (0..state.targets.len()).collect();
            order.sort_by_key(|i| state.targets[*i].start.abs_diff(state.origin));
            for i in &order {
                let t = &mut state.targets[*i];
                if let Some(label) = previous
                    .iter()
                    .find(|p| p.start == t.start)
                    .and_then(|p| p.label)
                    && let Some(index) = labels.iter().position(|l| *l == label)
                {
                    t.label = Some(labels.remove(index));
                }
            }
            for i in order {
                if state.targets[i].label.is_none() && !labels.is_empty() {
                    state.targets[i].label = Some(labels.remove(0));
                }
            }
            state.labels = state
                .targets
                .iter()
                .filter_map(|t| t.label.map(|l| (t.end, l)))
                .collect();
        }
        self.jump = Some(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jump_searches_visible_cells_after_mixed_width_prefix() {
        let line = format!(
            "{}\t{}XYZ\t{}",
            "界".repeat(80),
            "e\u{301}".repeat(40),
            "a".repeat(100)
        );
        let mut editor = Editor::new(line.clone());
        editor.jump_viewport(0, 162, 1, 80);
        editor.key("s");
        editor.key("X");
        assert_eq!(editor.jump.as_ref().unwrap().targets.len(), 1);
        assert_eq!(editor.jump.as_ref().unwrap().targets[0].start, 161);
        editor.key("<Enter>");
        assert_eq!(editor.position(), (0, 161));
        assert_eq!(editor.display_cell_column(), 202);
        assert_eq!(editor.contents(), line);
        assert!(!editor.dirty());

        let mut editor = Editor::new("界abc".into());
        editor.jump_viewport(0, 1, 1, 3);
        editor.key("s");
        editor.key("界");
        assert!(editor.jump.as_ref().unwrap().targets.is_empty());
    }
    #[test]
    fn labeled_jump_cancels_preserves_unicode_and_composes_with_operators() {
        let mut e = Editor::new("árbol answer\nnext answer\nOUTSIDE answer".into());
        e.jump_viewport(0, 0, 2, 200);
        e.key("s");
        e.key("a");
        e.key("n");
        let targets = &e.jump.as_ref().unwrap().targets;
        assert_eq!(targets.len(), 2);
        let target = targets[1].clone();
        e.key(&target.label.unwrap().to_string());
        assert_eq!(e.cursor, target.start);
        assert!(!e.dirty());
        e.key("s");
        e.key("á");
        e.key("<Esc>");
        assert_eq!(e.cursor, target.start);
        e.goto(0, 0);
        e.key("d");
        e.key("s");
        e.key("a");
        e.key("n");
        e.key("<Enter>");
        assert!(e.contents().starts_with("answer"));
        e.key("u");
        assert!(e.contents().starts_with("árbol"));
        e.key("s");
        e.key("A");
        assert!(e.jump.as_ref().unwrap().targets.is_empty());
        e.key("<Esc>");
        e.goto(0, 0);
        e.key("v");
        e.key("s");
        e.key("a");
        e.key("n");
        e.key("<Enter>");
        assert_eq!(e.mode, Mode::Visual);
        assert!(e.selection().is_some());
    }
}
