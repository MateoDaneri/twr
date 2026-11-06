//! Bounded per-line Unicode cursor boundaries. Offsets count Rust chars.
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) const MAX_UNICODE_LINE: usize = 131_072;

pub(super) enum LineIndex {
    Ascii(usize),
    Segments {
        points: Vec<usize>,
        columns: Vec<usize>,
    },
}

impl LineIndex {
    pub(super) fn new(line: &[char], tabstop: usize) -> Option<Self> {
        let tabstop = tabstop.max(1);
        if line.iter().all(char::is_ascii) {
            return Some(Self::Ascii(line.len()));
        }
        if line.len() > MAX_UNICODE_LINE {
            return None;
        }
        let text: String = line.iter().collect();
        let mut result = vec![0];
        let mut columns = vec![0usize];
        let mut offset = 0;
        for grapheme in text.graphemes(true) {
            offset += grapheme.chars().count();
            result.push(offset);
            let column = *columns.last().unwrap();
            let width = if grapheme == "\t" {
                tabstop - column % tabstop
            } else {
                UnicodeWidthStr::width(grapheme).max(1)
            };
            columns.push(column.saturating_add(width));
        }
        Some(Self::Segments {
            points: result,
            columns,
        })
    }

    #[cfg(test)]
    pub(super) fn points(&self) -> Vec<usize> {
        match self {
            Self::Ascii(len) => (0..=*len).collect(),
            Self::Segments { points, .. } => points.clone(),
        }
    }

    pub(super) fn move_by(
        &self,
        offset: usize,
        count: usize,
        right: bool,
        allow_end: bool,
    ) -> Option<usize> {
        match self {
            Self::Ascii(len) => {
                if offset > *len {
                    return None;
                }
                let max = if allow_end {
                    *len
                } else {
                    len.saturating_sub(1)
                };
                Some(if right {
                    offset.saturating_add(count).min(max)
                } else {
                    offset.saturating_sub(count)
                })
            }
            Self::Segments { points, .. } => {
                let index = points.binary_search(&offset).ok()?;
                let max = if allow_end {
                    points.len() - 1
                } else {
                    points.len().saturating_sub(2)
                };
                let target = if right {
                    index.saturating_add(count).min(max)
                } else {
                    index.saturating_sub(count)
                };
                points.get(target).copied()
            }
        }
    }

    pub(super) fn floor(&self, offset: usize) -> Option<usize> {
        match self {
            Self::Ascii(len) => (offset <= *len).then_some(offset),
            Self::Segments { points, .. } => {
                if offset > *points.last()? {
                    return None;
                }
                points
                    .get(points.partition_point(|p| *p <= offset).saturating_sub(1))
                    .copied()
            }
        }
    }

    pub(super) fn exact_forward(&self, offset: usize, count: usize) -> Option<usize> {
        match self {
            Self::Ascii(len) => offset.checked_add(count).filter(|end| *end <= *len),
            Self::Segments { points, .. } => {
                let index = points.binary_search(&offset).ok()?;
                points.get(index.checked_add(count)?).copied()
            }
        }
    }

    pub(super) fn column(
        &self,
        offset: usize,
        line: &[char],
        tabstop: usize,
        normal: bool,
    ) -> Option<usize> {
        let tabstop = tabstop.max(1);
        match self {
            Self::Ascii(len) => {
                if offset > *len {
                    return None;
                }
                let mut column = 0usize;
                for ch in &line[..offset] {
                    column += if *ch == '\t' {
                        tabstop - column % tabstop
                    } else {
                        1
                    };
                }
                if normal && line.get(offset) == Some(&'\t') {
                    column += tabstop - column % tabstop - 1;
                }
                Some(column)
            }
            Self::Segments { points, columns } => {
                let i = points.binary_search(&offset).ok()?;
                let mut column = *columns.get(i)?;
                if normal && line.get(offset) == Some(&'\t') {
                    column += tabstop - column % tabstop - 1;
                }
                Some(column)
            }
        }
    }

    pub(super) fn offset_at_column(&self, target: usize, line: &[char], tabstop: usize) -> usize {
        let tabstop = tabstop.max(1);
        match self {
            Self::Ascii(_) => {
                let mut column = 0usize;
                for (offset, ch) in line.iter().enumerate() {
                    let width = if *ch == '\t' {
                        tabstop - column % tabstop
                    } else {
                        1
                    };
                    if column.saturating_add(width) > target {
                        return offset;
                    }
                    column += width;
                }
                line.len()
            }
            Self::Segments { points, columns } => {
                let index = columns
                    .partition_point(|column| *column <= target)
                    .saturating_sub(1);
                points[index]
            }
        }
    }
}

pub(super) struct Cache {
    pub(super) version: u64,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) tabstop: usize,
    pub(super) index: Option<LineIndex>,
}
