//! Half-page geometry for native lists. All indices count visible, unfolded rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub top: usize,
    pub cursor: usize,
}
/// Snacks scrolls the viewport and only pulls its cursor into the scrolloff area.
/// At either edge it also applies the requested delta to the cursor.
pub fn snacks(current: Page, count: usize, rows: usize, margin: usize, delta: isize) -> Page {
    if count == 0 {
        return Page { top: 0, cursor: 0 };
    }
    let rows = rows.max(1);
    let max_top = count.saturating_sub(rows);
    let top = current.top.saturating_add_signed(delta).min(max_top);
    let cursor = if top == 0 || top == max_top {
        current.cursor.saturating_add_signed(delta)
    } else {
        current.cursor
    };
    let configured_margin = margin.min((rows - 1) / 2);
    let margin_for = |cursor: usize| {
        if cursor
            .saturating_add(1)
            .min(count.saturating_sub(cursor.saturating_add(1)))
            > configured_margin
        {
            configured_margin
        } else {
            0
        }
    };
    let margin = margin_for(cursor);
    let cursor = cursor.clamp(
        top + margin,
        (top + rows - 1).saturating_sub(margin).min(count - 1),
    );
    // list:render() runs move(0) after scrolling. That second scrolloff check
    // can shift the viewport again when the cursor leaves an edge area.
    let margin = margin_for(cursor);
    let top = top.clamp(
        cursor.saturating_sub(rows - 1 - margin),
        cursor.saturating_sub(margin),
    );
    Page { top, cursor }
}
pub fn normal(current: Page, count: usize, rows: usize, delta: isize) -> Page {
    if count == 0 {
        return Page { top: 0, cursor: 0 };
    }
    let top = current
        .top
        .saturating_add_signed(delta)
        .min(count.saturating_sub(rows.max(1)));
    Page {
        top,
        cursor: current
            .cursor
            .saturating_add_signed(delta)
            .min(count - 1)
            .clamp(top, top.saturating_add(rows.max(1) - 1).min(count - 1)),
    }
}
