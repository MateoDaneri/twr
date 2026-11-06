#[cfg(test)]
mod half_page_view_research {
    use super::*;

    // Approximate the split-cell adjustment after nv_screengo's coladvance.
    fn screen_coladvance(
        editor: &Editor,
        layouts: &mut std::collections::HashMap<usize, Vec<WrapPosition>>,
        line: usize,
        wanted: usize,
        width: usize,
        one_more: bool,
    ) -> Result<WrapCursor, &'static str> {
        let mut cursor =
            editor.native_wheel_coladvance_cached(layouts, line, wanted, width, one_more)?;
        if cursor.scalar > 0 {
            let virtcol = editor.native_wheel_vcol_cached(layouts, cursor, width, false)?;
            let remainder = if wanted < width {
                wanted
            } else {
                (wanted - width) % width
            };
            if virtcol > wanted && remainder > width / 2 {
                let offset = editor.line_starts[line] + cursor.scalar - 1;
                cursor = editor
                    .wrapped_cursor_at(offset, width, false)?
                    .ok_or("bad split cell")?;
            }
        }
        Ok(cursor)
    }

    // Experiment with adjust_skipcol after nv_screengo. Width1 == width2 and
    // there are no sign/number columns or showbreak in these references.
    fn adjust_skip_candidate(
        editor: &Editor,
        mut top: WrappedTop,
        cursor: WrapCursor,
        rows: usize,
        width: usize,
        scrolloff: usize,
        virtual_add: Option<usize>,
        heights: &mut std::collections::HashMap<(usize, bool), usize>,
        layouts: &mut std::collections::HashMap<usize, Vec<WrapPosition>>,
    ) -> Result<WrappedTop, &'static str> {
        if cursor.line != top.source_line {
            return Ok(top);
        }
        let last =
            editor.native_wheel_final_cached(heights, layouts, cursor.line, width, cursor)?;
        // Neovim resets skipcol here only when the line exactly fills the
        // viewport; a shorter line has a smaller w_cline_height.
        if last + 1 == rows {
            top.visual_row = 0;
            return Ok(top);
        }
        let vcol = if let Some(add) = virtual_add {
            editor.native_block_wheel_vcol(layouts, cursor, add, width)?
        } else {
            editor.native_wheel_vcol_cached(layouts, cursor, width, false)?
        };
        let initial_skip = top.visual_row;
        while top.visual_row > 0 && vcol < top.visual_row * width + 3 + scrolloff * width {
            top.visual_row -= 1;
        }
        if top.visual_row != initial_skip {
            return Ok(top);
        }
        let positions = editor.native_wheel_positions_cached(layouts, cursor.line, width)?;
        let size = positions.last().map_or(0, |p| p.row * width + p.end);
        let rounded = size.div_ceil(width) * width;
        let mut col = vcol.saturating_add(scrolloff * width);
        if scrolloff > 0 {
            while col > rounded && col >= width {
                col -= width;
            }
        }
        let mut col = col as isize - (top.visual_row * width) as isize;
        let mut row = 0usize;
        if col >= width as isize {
            col -= width as isize;
            row += 1;
        }
        if col > width as isize {
            row += col as usize / width;
        }
        if row >= rows {
            if top.visual_row == 0 {
                top.visual_row = 1;
                row -= 1;
            }
            if row >= rows {
                top.visual_row += row - rows;
            }
        }
        Ok(top)
    }

    // Approximation of the later curs_columns smoothscroll correction for the
    // equal-width, no-conceal cases in these traces. Research only.
    fn cursor_columns_candidate(
        editor: &Editor,
        mut top: WrappedTop,
        cursor: WrapCursor,
        rows: usize,
        width: usize,
        scrolloff: usize,
        virtual_add: Option<usize>,
        heights: &mut std::collections::HashMap<(usize, bool), usize>,
        layouts: &mut std::collections::HashMap<usize, Vec<WrapPosition>>,
    ) -> Result<WrappedTop, &'static str> {
        if cursor.line != top.source_line {
            return Ok(top);
        }
        let last =
            editor.native_wheel_final_cached(heights, layouts, cursor.line, width, cursor)?;
        let vcol = if let Some(add) = virtual_add {
            editor.native_block_wheel_vcol(layouts, cursor, add, width)?
        } else {
            editor.native_wheel_vcol_cached(layouts, cursor, width, false)?
        };
        let wrow = (vcol / width) as isize - top.visual_row as isize;
        let needs = wrow >= rows as isize
            || ((top.visual_row > 0 || wrow + scrolloff as isize >= rows as isize) && last >= rows);
        if !needs {
            return Ok(top);
        }
        let mut extra = usize::from(top.visual_row * width + scrolloff * width > vcol);
        let n = last.min(wrow.saturating_add(scrolloff as isize).max(0) as usize);
        if n as isize >= rows as isize + top.visual_row as isize - scrolloff as isize {
            extra += 2;
        }
        if extra == 3 || rows <= scrolloff.saturating_mul(2) {
            let centered = (vcol / width).saturating_sub(rows / 2);
            top.visual_row = centered.min((last + 1).saturating_sub(rows));
        } else if extra == 1 {
            let deficit = (top.visual_row * width + scrolloff * width).saturating_sub(vcol);
            top.visual_row = top.visual_row.saturating_sub(deficit.div_ceil(width));
        } else if extra == 2 {
            let mut end = (n as isize - rows as isize + 1) * width as isize;
            while end > vcol as isize {
                end -= width as isize;
            }
            top.visual_row = top.visual_row.max(end.max(0) as usize / width);
        }
        Ok(top)
    }

    // Approximate cursor_correct using the reference viewport as an oracle.
    // This is diagnostic only and omits conceal, folds and incremental redraw.
    fn correct_cursor(
        editor: &Editor,
        top: WrappedTop,
        cursor: WrapCursor,
        rows: usize,
        width: usize,
        scrolloff: usize,
        _one_more: bool,
        _virtual_block: bool,
        heights: &mut std::collections::HashMap<(usize, bool), usize>,
        layouts: &mut std::collections::HashMap<usize, Vec<WrapPosition>>,
    ) -> Result<WrapCursor, &'static str> {
        let viewport = editor.wrapped_viewport_for_cursor(top, rows, width, cursor)?;
        let last = viewport.last().ok_or("empty viewport")?;
        let last_line = editor.source_line_at(last.source.start);
        let last_top = WrappedTop {
            source_line: last_line,
            visual_row: last.visual_row,
        };
        let at_end = editor
            .native_wheel_step_cached(heights, layouts, last_top, true, width, cursor)?
            == last_top;
        let mut above_wanted = scrolloff;
        let mut below_wanted = scrolloff;
        if top.source_line == 0 {
            above_wanted = 0;
            below_wanted = below_wanted.min(rows / 2);
        }
        if at_end {
            below_wanted = 0;
            above_wanted = above_wanted.min(rows.saturating_sub(1) / 2);
        }
        let mut low = top.source_line;
        let mut high = last_line;
        if last.visual_row
            < editor.native_wheel_final_cached(heights, layouts, last_line, width, cursor)?
            && high > low
        {
            high = editor.source_row(editor.visible_row(high).saturating_sub(1));
        }
        let (mut above, mut below) = (0usize, 0usize);
        while (above < above_wanted || below < below_wanted) && low < high {
            if below < below_wanted && (below <= above || above >= above_wanted) {
                below +=
                    editor.native_wheel_final_cached(heights, layouts, high, width, cursor)? + 1;
                high = editor.source_row(editor.visible_row(high).saturating_sub(1));
            }
            if above < above_wanted && (above < below || below >= below_wanted) && low < high {
                above +=
                    editor.native_wheel_final_cached(heights, layouts, low, width, cursor)? + 1;
                low = editor.source_row(editor.visible_row(low).saturating_add(1));
            }
        }
        let mut target = cursor.line;
        if low == high {
            target = low;
        } else {
            if cursor.line < low && top.source_line > 0 {
                target = low;
            }
            if cursor.line > high && !at_end {
                target = high;
            }
        }
        if target == cursor.line {
            return Ok(cursor);
        }
        let mut retained =
            editor.native_wheel_retain_byte_cached(layouts, cursor, target, width)?;
        let line_start = editor.line_starts[target];
        let len = editor.line_end(line_start) - line_start;
        let origin_start = editor.line_starts[cursor.line];
        let origin_byte: usize = editor.text[origin_start..origin_start + cursor.scalar]
            .iter()
            .map(|ch| ch.len_utf8())
            .sum();
        let target_bytes: usize = editor.text[line_start..line_start + len]
            .iter()
            .map(|ch| ch.len_utf8())
            .sum();
        if len > 0 && retained.scalar == len && origin_byte != target_bytes {
            retained = editor
                .wrapped_cursor_at(line_start + len - 1, width, false)?
                .ok_or("bad cursor")?;
        }
        // mark_mb_adjustpos in Neovim cannot leave the cursor on the
        // combining scalar inside a grapheme after retaining a byte column.
        let positions = editor.native_wheel_positions_cached(layouts, target, width)?;
        let mut scalar = retained.scalar;
        while scalar > 0 && scalar < positions.len() && positions[scalar] == positions[scalar - 1] {
            scalar -= 1;
        }
        if scalar != retained.scalar {
            retained = editor
                .wrapped_cursor_at(line_start + scalar, width, true)?
                .ok_or("bad grapheme")?;
        }
        Ok(retained)
    }

    // This experiment carries viewport, cursor and scroll option; the desired
    // column may be carried or taken from the trace. It is not a parity gate.
    #[test]
    fn diagnostic_sequential_half_page() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-half-page.json"
        ))
        .unwrap();
        let (mut total, mut bad_top, mut bad_cursor, mut bad_coladd) =
            (0usize, 0usize, 0usize, 0usize);
        let unseeded = std::env::var("TWR_HALF_PAGE_UNSEEDED").as_deref() == Ok("1");
        let wheel_phase = std::env::var("TWR_HALF_PAGE_WHEEL_PHASE").as_deref() == Ok("1");
        let follow_phase = std::env::var("TWR_HALF_PAGE_FOLLOW_PHASE").as_deref() == Ok("1");
        let trace_case = std::env::var("TWR_HALF_PAGE_TRACE_CASE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok());
        let mut examples = Vec::new();
        for (case_id, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
            let lines: Vec<&str> = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let mut editor = Editor::new(lines.join("\n"));
            editor.indent_style.tabstop = 8;
            let cols = case["width"].as_u64().unwrap() as usize;
            let rows = case["rows"].as_u64().unwrap() as usize;
            let scrolloff = case["scrolloff"].as_u64().unwrap() as usize;
            let virtual_block = case["steps"][0]["mode"].as_str() == Some("\u{16}");
            let one_more = matches!(case["steps"][0]["mode"].as_str(), Some("v" | "V"));
            let mut trace_previous = case["initial_cursor"].clone();
            let mut model_desired = trace_previous[4].as_u64().unwrap() as usize - 1;
            let mut scroll = (rows / 2).max(1);
            let line = trace_previous[1].as_u64().unwrap() as usize - 1;
            let byte = trace_previous[2].as_u64().unwrap() as usize - 1;
            let scalar = lines[line]
                .char_indices()
                .take_while(|(at, _)| *at < byte)
                .count();
            let mut coladd = trace_previous[3].as_u64().unwrap() as usize;
            let initial_offset = editor.line_starts[line] + scalar;
            let original_version = editor.version;
            let linear_visual_mode = match case["steps"][0]["mode"].as_str() {
                Some("v") => Some(("v", Mode::Visual)),
                Some("V") => Some(("V", Mode::VisualLine)),
                _ => None,
            };
            if let Some((key, mode)) = linear_visual_mode {
                editor.cursor = initial_offset;
                editor.key(key);
                assert_eq!(editor.mode, mode);
            }
            let linear_anchor = editor.anchor;
            let anchor = if virtual_block {
                if let Some(initial_anchor) = case.get("initial_anchor") {
                    let anchor_line = initial_anchor[1].as_u64().unwrap() as usize - 1;
                    let anchor_byte = initial_anchor[2].as_u64().unwrap() as usize - 1;
                    let anchor_scalar = lines[anchor_line]
                        .char_indices()
                        .take_while(|(at, _)| *at < anchor_byte)
                        .count();
                    editor.cursor = editor.line_starts[anchor_line] + anchor_scalar;
                } else {
                    editor.cursor = initial_offset;
                }
                editor.key("<C-v>");
                assert_eq!(editor.mode, Mode::VisualBlock);
                if case.get("initial_anchor").is_some() {
                    editor.key("$");
                    assert!(editor.block_wants_eol());
                }
                Some(editor.block_anchor_for_test())
            } else {
                None
            };
            let mut cursor = if virtual_block {
                editor
                    .wrapped_virtual_cursor_at(initial_offset, coladd, cols)
                    .unwrap()
                    .unwrap()
                    .cursor
            } else {
                editor
                    .wrapped_cursor_at(initial_offset, cols, true)
                    .unwrap()
                    .unwrap()
            };
            if virtual_block && case.get("initial_anchor").is_some() {
                let actual = editor.wrapped_block_cursor(cols).unwrap().unwrap();
                assert_eq!(
                    (actual.cursor.line, actual.cursor.scalar, actual.coladd),
                    (cursor.line, cursor.scalar, coladd),
                    "initial block $ cursor case {case_id}"
                );
            }
            let mut top = WrappedTop {
                source_line: case["initial_topline"].as_u64().unwrap() as usize - 1,
                visual_row: case["initial_skipcol"].as_u64().unwrap() as usize / cols,
            };
            for (step_id, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                let start_cursor = cursor;
                let initial_top = top;
                let down = step["key"].as_str().unwrap().contains("C-d");
                let prefix = step["key"].as_str().unwrap().split('<').next().unwrap();
                if !prefix.is_empty() {
                    scroll = prefix.parse::<usize>().unwrap().min(rows);
                }
                let count = scroll.min(rows);
                assert_eq!(
                    count,
                    step["scroll"].as_u64().unwrap() as usize,
                    "scroll option case {case_id} step {step_id}"
                );
                let mut heights = std::collections::HashMap::new();
                let mut layouts = std::collections::HashMap::new();
                let mut available = 0usize;
                if down {
                    let mut probe = top;
                    for _ in 0..rows.saturating_add(count) {
                        let next = editor
                            .native_wheel_step_cached(
                                &mut heights,
                                &mut layouts,
                                probe,
                                true,
                                cols,
                                cursor,
                            )
                            .unwrap();
                        if next == probe {
                            break;
                        }
                        probe = next;
                        available += 1;
                    }
                }
                let amount = if down {
                    count.min(available.saturating_add(1).saturating_sub(rows))
                } else {
                    count
                };
                if wheel_phase && amount > 0 {
                    assert_eq!(scrolloff, 4);
                    let mode = if virtual_block {
                        Mode::VisualBlock
                    } else if let Some((_, mode)) = linear_visual_mode {
                        mode
                    } else {
                        Mode::Normal
                    };
                    let goal = if unseeded {
                        model_desired
                    } else {
                        trace_previous[4].as_u64().unwrap() as usize - 1
                    };
                    let result = editor
                        .calculate_wrapped_wheel(WrappedWheelRequest {
                            top,
                            cursor,
                            goal: Some(goal),
                            rows,
                            columns: cols,
                            delta: amount as isize * if down { 1 } else { -1 },
                            mode,
                            coladd,
                        })
                        .unwrap();
                    top = result.0;
                } else {
                    for _ in 0..amount {
                        let next = editor
                            .native_wheel_step_cached(
                                &mut heights,
                                &mut layouts,
                                top,
                                down,
                                cols,
                                cursor,
                            )
                            .unwrap();
                        if next == top {
                            break;
                        }
                        top = next;
                    }
                }
                if case_id == 7 && step_id == 9 {
                    let intermediate = correct_cursor(&editor, top, cursor, rows, cols, scrolloff, one_more, virtual_block, &mut heights, &mut layouts).unwrap();
                    let vcol = editor.native_wheel_vcol_cached(&mut layouts, intermediate, cols, false).unwrap();
                    println!("case7 intermediate cursor={intermediate:?} vcol={vcol}");
                }
                let mut target = WrappedTop {
                    source_line: cursor.line,
                    visual_row: cursor.visual_row,
                };
                let desired = if unseeded {
                    model_desired
                } else {
                    trace_previous[4].as_u64().unwrap() as usize - 1
                };
                let max_goal = (editor
                    .native_wheel_final_cached(
                        &mut heights,
                        &mut layouts,
                        cursor.line,
                        cols,
                        cursor,
                    )
                    .unwrap()
                    + 1)
                    * cols
                    - 1;
                let end_wanted = virtual_block && desired >= i32::MAX as usize - 1;
                let desired = if end_wanted {
                    cols - 1
                } else {
                    desired.min(max_goal)
                };
                for _ in 0..count {
                    let next = editor
                        .native_wheel_step_cached(
                            &mut heights,
                            &mut layouts,
                            target,
                            down,
                            cols,
                            cursor,
                        )
                        .unwrap();
                    if next == target {
                        break;
                    }
                    target = next;
                }
                model_desired = if end_wanted {
                    i32::MAX as usize - 1
                } else {
                    target.visual_row * cols + desired % cols
                };
                if virtual_block {
                    (cursor, coladd) = editor
                        .native_block_wheel_coladvance(
                            &mut layouts,
                            target.source_line,
                            model_desired,
                            cols,
                        )
                        .unwrap();
                    // nv_screengo temporarily tracks the end of the target
                    // screen row even when coladvance itself uses MAXCOL.
                    // Its split-cell correction compares against that temporary
                    // goal before restoring MAXCOL.
                    if end_wanted {
                        let start = editor.line_starts[cursor.line];
                        let temporary_goal = target.visual_row * cols + cols - 1;
                        let virtcol = editor
                            .native_block_wheel_vcol(&mut layouts, cursor, coladd, cols)
                            .unwrap();
                        if cursor.scalar > 0
                            && virtcol > temporary_goal
                            && temporary_goal % cols > cols / 2
                        {
                            cursor = editor
                                .wrapped_virtual_cursor_at(start + cursor.scalar - 1, coladd, cols)
                                .unwrap()
                                .unwrap()
                                .cursor;
                        }
                    }
                } else {
                    cursor = screen_coladvance(
                        &editor,
                        &mut layouts,
                        target.source_line,
                        model_desired,
                        cols,
                        one_more,
                    )
                    .unwrap();
                }
                let motion_cursor = cursor;
                // Research-only minimal reveal after nv_screengo; this is not
                // a full model of Neovim update_topline or scrolloff.
                if follow_phase {
                    for _ in 0..rows.saturating_add(count) {
                        let visible = editor
                            .wrapped_viewport_for_cursor(top, rows, cols, cursor)
                            .unwrap();
                        if editor.wrapped_cursor_cell(&visible, cursor).is_some() {
                            break;
                        }
                        let down_to_cursor = (cursor.line, cursor.visual_row)
                            > (top.source_line, top.visual_row);
                        let next = editor
                            .native_wheel_step_cached(
                                &mut heights,
                                &mut layouts,
                                top,
                                down_to_cursor,
                                cols,
                                cursor,
                            )
                            .unwrap();
                        if next == top {
                            break;
                        }
                        top = next;
                    }
                }
                if trace_case == Some(case_id) {
                    println!("phase case {case_id} step {step_id}: start={start_cursor:?} initial={initial_top:?} amount={amount} target={target:?} motion={motion_cursor:?} top_after_reveal={top:?}");
                }
                // Empirical update_topline hypothesis for zero scrolloff. It
                // still disagrees with reference traces; research only.
                if scrolloff == 0 && top.source_line < cursor.line {
                    let mut end = top;
                    for _ in 1..rows {
                        let next = editor
                            .native_wheel_step_cached(
                                &mut heights,
                                &mut layouts,
                                end,
                                true,
                                cols,
                                cursor,
                            )
                            .unwrap();
                        if next == end {
                            break;
                        }
                        end = next;
                    }
                    let last = editor
                        .native_wheel_final_cached(
                            &mut heights,
                            &mut layouts,
                            cursor.line,
                            cols,
                            cursor,
                        )
                        .unwrap();
                    if end.source_line < cursor.line
                        || (end.source_line == cursor.line
                            && end.visual_row < last
                            && last + 1 >= rows)
                    {
                        // Approximate scroll_cursor_halfway() for equal width,
                        // no folds or filler. Heights are in screen rows.
                        let line_height = |line, heights: &mut _, layouts: &mut _| {
                            editor
                                .native_wheel_final_cached(heights, layouts, line, cols, cursor)
                                .unwrap()
                                + 1
                        };
                        let mut used =
                            line_height(cursor.line, &mut heights, &mut layouts).min(rows);
                        let mut above = cursor.line;
                        let mut below = cursor.line;
                        top = WrappedTop {
                            source_line: cursor.line,
                            visual_row: 0,
                        };
                        while above > 0 {
                            above -= 1;
                            let above_height =
                                line_height(above, &mut heights, &mut layouts).min(rows);
                            used += above_height;
                            if below + 1 < lines.len() {
                                below += 1;
                                used += line_height(below, &mut heights, &mut layouts).min(rows);
                            }
                            if used > rows {
                                if used - above_height < rows {
                                    top = WrappedTop {
                                        source_line: above,
                                        visual_row: used - rows,
                                    };
                                }
                                break;
                            }
                            top = WrappedTop {
                                source_line: above,
                                visual_row: 0,
                            };
                        }
                    }
                }
                let before_adjust = top;
                top = adjust_skip_candidate(
                    &editor,
                    top,
                    cursor,
                    rows,
                    cols,
                    scrolloff,
                    virtual_block.then_some(coladd),
                    &mut heights,
                    &mut layouts,
                )
                .unwrap();
                let after_adjust = top;
                if case_id == 7 && step_id == 0 && case["lines"][0].as_str().unwrap().len() == 308 {
                    println!("seed3-case7-step0: start={initial_top:?} cursor={start_cursor:?} count={count} available={available} amount={amount} motion={motion_cursor:?} before_adjust={before_adjust:?} after_adjust={after_adjust:?}");
                }
                if case_id == 7 && step_id == 9 {
                    println!("case7-step9: initial_top={initial_top:?} start_cursor={start_cursor:?} count={count} available={available} amount={amount} motion_cursor={motion_cursor:?} before_adjust={before_adjust:?} after_adjust={after_adjust:?}");
                    println!("case7-step9 model last row={}", editor.native_wheel_final_cached(&mut heights, &mut layouts, 0, cols, cursor).unwrap());
                }
                // Additional empirical condition for a cursor re-entering a
                // previously clipped top line. Not proven source semantics.
                if top != before_adjust
                    || (initial_top.visual_row > 0
                        && initial_top.source_line == cursor.line
                        && start_cursor.line != cursor.line)
                    || (top.source_line == cursor.line
                        && cursor.visual_row >= top.visual_row + rows)
                {
                    top = cursor_columns_candidate(
                        &editor,
                        top,
                        cursor,
                        rows,
                        cols,
                        scrolloff,
                        virtual_block.then_some(coladd),
                        &mut heights,
                        &mut layouts,
                    )
                    .unwrap();
                }
                if case_id == 11
                    && step_id == 0
                    && case["lines"][1]
                        .as_str()
                        .unwrap()
                        .starts_with("one two-three")
                {
                    let visible = editor
                        .wrapped_viewport_for_cursor(top, rows, cols, cursor)
                        .unwrap();
                    let rows_debug: Vec<_> = visible
                        .iter()
                        .map(|v| (editor.source_line_at(v.source.start) + 1, v.visual_row))
                        .collect();
                    println!(
                        "fuzz case 11 before correction: top {top:?}, cursor {cursor:?}, coladd {coladd}, rows {rows_debug:?}"
                    );
                }
                if scrolloff != 0 {
                    cursor = correct_cursor(
                        &editor,
                        top,
                        cursor,
                        rows,
                        cols,
                        scrolloff,
                        one_more,
                        virtual_block,
                        &mut heights,
                        &mut layouts,
                    )
                    .unwrap();
                }
                if case_id == 7 && step_id == 0 && case["lines"][0].as_str().unwrap().len() == 308 {
                    println!("seed3-case7-step0 after cursor correction: top={top:?} cursor={cursor:?}");
                }
                if case_id == 7 && step_id == 9 {
                    println!("case7-step9 after cursor correction: top={top:?} cursor={cursor:?}");
                }
                // Probe a possible prior-skip condition; this is an empirical
                // hypothesis, not established Neovim semantics.
                if initial_top.visual_row > 0
                    && top.source_line == cursor.line
                    && cursor.visual_row < top.visual_row
                    && editor.native_wheel_final_cached(&mut heights, &mut layouts, cursor.line, cols, cursor).unwrap() + 1 > rows
                {
                    top = adjust_skip_candidate(
                        &editor,
                        top,
                        cursor,
                        rows,
                        cols,
                        scrolloff,
                        virtual_block.then_some(coladd),
                        &mut heights,
                        &mut layouts,
                    )
                    .unwrap();
                }
                if case.get("initial_anchor").is_some()
                    && ((case_id < 2 && step_id < 3)
                        || (case_id == 3 && (step_id == 0 || step_id == 3)))
                {
                    println!(
                        "eol phases case {case_id} step {step_id}: start {start_cursor:?}, target {target:?}, motion {motion_cursor:?}, corrected {cursor:?}, top {before_adjust:?} -> {after_adjust:?} -> {top:?}"
                    );
                }
                if case_id == 11
                    && step_id == 0
                    && case["lines"][1]
                        .as_str()
                        .unwrap()
                        .starts_with("one two-three")
                {
                    println!(
                        "fuzz phases: start {start_cursor:?}, target {target:?}, motion {motion_cursor:?}, corrected {cursor:?}, top {before_adjust:?} -> {after_adjust:?} -> {top:?}"
                    );
                }
                if let Some(anchor) = anchor {
                    let offset = editor.line_starts[cursor.line] + cursor.scalar;
                    assert!(
                        editor.commit_wrapped_block_wheel(offset, coladd, cols, model_desired),
                        "block commit case {case_id} step {step_id}"
                    );
                    let actual = editor.wrapped_block_cursor(cols).unwrap().unwrap();
                    assert_eq!(
                        (actual.cursor.line, actual.cursor.scalar, actual.coladd),
                        (cursor.line, cursor.scalar, coladd),
                        "block cursor case {case_id} step {step_id}"
                    );
                    assert_eq!(
                        editor.block_anchor_for_test(),
                        anchor,
                        "block anchor case {case_id} step {step_id}"
                    );
                    assert_eq!(editor.mode, Mode::VisualBlock);
                    if case.get("initial_anchor").is_some() {
                        assert!(
                            editor.block_wants_eol(),
                            "block $ intent case {case_id} step {step_id}"
                        );
                    }
                    assert_eq!(editor.version, original_version);
                    assert_eq!(editor.contents(), lines.join("\n"));
                }
                if let Some((_, mode)) = linear_visual_mode {
                    let offset = editor.line_starts[cursor.line] + cursor.scalar;
                    assert!(editor.commit_wrapped_wheel(offset, mode));
                    assert_eq!(editor.mode, mode, "visual mode case {case_id} step {step_id}");
                    assert_eq!(editor.anchor, linear_anchor, "visual anchor case {case_id} step {step_id}");
                    let reference = &step["anchor"];
                    let anchor_line = reference[1].as_u64().unwrap() as usize - 1;
                    let anchor_byte = reference[2].as_u64().unwrap() as usize - 1;
                    let anchor_scalar = lines[anchor_line].char_indices().take_while(|(at, _)| *at < anchor_byte).count();
                    assert_eq!(editor.anchor, editor.line_starts[anchor_line] + anchor_scalar, "Neovim anchor case {case_id} step {step_id}");
                    assert_eq!(editor.version, original_version);
                    assert_eq!(editor.contents(), lines.join("\n"));
                }
                let expected_top = WrappedTop {
                    source_line: step["topline"].as_u64().unwrap() as usize - 1,
                    visual_row: step["skipcol"].as_u64().unwrap() as usize / cols,
                };
                let expected_cursor = (
                    step["cursor"][1].as_u64().unwrap() as usize - 1,
                    step["cursor"][2].as_u64().unwrap() as usize,
                );
                let actual_byte = lines[cursor.line]
                    .chars()
                    .take(cursor.scalar)
                    .map(char::len_utf8)
                    .sum::<usize>()
                    + 1;
                let t = top != expected_top;
                let c = (cursor.line, actual_byte) != expected_cursor;
                let v = coladd != step["cursor"][3].as_u64().unwrap() as usize;
                total += 1;
                bad_top += usize::from(t);
                bad_cursor += usize::from(c);
                bad_coladd += usize::from(v);
                if (t || c || v) && examples.len() < 8 {
                    examples.push(format!("case {case_id} step {step_id}: top {top:?} vs {expected_top:?}, cursor {:?} vs {:?}",
                        (cursor.line + 1, actual_byte, coladd), (expected_cursor.0 + 1, expected_cursor.1, step["cursor"][3].as_u64().unwrap())));
                }
                trace_previous = step["cursor"].clone();
            }
        }
        println!(
            "sequential approximation (desired column {}): {total} steps, top {bad_top}, cursor {bad_cursor}, coladd {bad_coladd} mismatches",
            if unseeded {
                "modeled"
            } else {
                "from reference"
            }
        );
        for example in examples {
            println!("{example}");
        }
        assert_eq!(total, fixture["cases"].as_array().unwrap().len() * 10);
        if std::env::var("TWR_HALF_PAGE_STRICT").as_deref() == Ok("1") {
            assert_eq!(
                (bad_top, bad_cursor, bad_coladd),
                (0, 0, 0),
                "fixture mismatches"
            );
        }
    }

    // Diagnostic model only. It isolates Neovim's end-of-buffer limit on
    // downward viewport motion, without cursor_correct or adjust_skipcol.
    #[test]
    fn compare_half_page_view_baseline() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-half-page.json"
        ))
        .unwrap();
        let mut steps = 0usize;
        let mut mismatches = 0usize;
        let mut adjusted_mismatches = 0usize;
        let mut marker_mismatches = 0usize;
        let mut cursor_mismatches = 0usize;
        let mut corrected_cursor_mismatches = 0usize;
        let mut corrected_examples = Vec::new();
        let mut both_mismatches = 0usize;
        let mut joint_top_mismatches = 0usize;
        let mut joint_cursor_mismatches = 0usize;
        let mut desired_mismatches = 0usize;
        let mut desired_examples = Vec::new();
        let mut joint_examples = Vec::new();
        let mut cursor_examples = Vec::new();
        let mut examples = Vec::new();
        for (case_id, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
            let lines: Vec<&str> = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line.as_str().unwrap())
                .collect();
            let mut editor = Editor::new(lines.join("\n"));
            editor.indent_style.tabstop = 8;
            let width = case["width"].as_u64().unwrap() as usize;
            let rows = case["rows"].as_u64().unwrap() as usize;
            let scrolloff = case["scrolloff"].as_u64().unwrap() as usize;
            let one_more = matches!(case["steps"][0]["mode"].as_str(), Some("v" | "V"));
            let mut prev_cursor = case["initial_cursor"].clone();
            let mut top = WrappedTop {
                source_line: case["initial_topline"].as_u64().unwrap() as usize - 1,
                visual_row: case["initial_skipcol"].as_u64().unwrap() as usize / width,
            };
            for step in case["steps"].as_array().unwrap() {
                let line = prev_cursor[1].as_u64().unwrap() as usize - 1;
                let byte = prev_cursor[2].as_u64().unwrap() as usize - 1;
                let scalar = lines[line]
                    .char_indices()
                    .take_while(|(at, _)| *at < byte)
                    .count();
                let offset = editor.line_starts[line] + scalar;
                let cursor = editor
                    .wrapped_cursor_at(offset, width, true)
                    .unwrap()
                    .unwrap();
                let count = step["scroll"].as_u64().unwrap() as usize;
                let down = step["key"].as_str().unwrap().contains("C-d");
                let mut heights = std::collections::HashMap::new();
                let mut layouts = std::collections::HashMap::new();
                let mut available = 0usize;
                if down {
                    let mut probe = top;
                    for _ in 0..rows.saturating_add(count) {
                        let next = editor
                            .native_wheel_step_cached(
                                &mut heights,
                                &mut layouts,
                                probe,
                                true,
                                width,
                                cursor,
                            )
                            .unwrap();
                        if next == probe {
                            break;
                        }
                        probe = next;
                        available += 1;
                    }
                }
                let amount = if down {
                    count.min(available.saturating_add(1).saturating_sub(rows))
                } else {
                    count
                };
                let mut predicted = top;
                for _ in 0..amount {
                    let next = editor
                        .native_wheel_step_cached(
                            &mut heights,
                            &mut layouts,
                            predicted,
                            down,
                            width,
                            cursor,
                        )
                        .unwrap();
                    if next == predicted {
                        break;
                    }
                    predicted = next;
                }
                let expected = WrappedTop {
                    source_line: step["topline"].as_u64().unwrap() as usize - 1,
                    visual_row: step["skipcol"].as_u64().unwrap() as usize / width,
                };
                steps += 1;
                if predicted != expected {
                    mismatches += 1;
                    if examples.len() < 6 {
                        examples.push(format!(
                            "case {case_id} {} predicted {predicted:?}, expected {expected:?}",
                            step["key"]
                        ));
                    }
                }
                // Diagnostic oracle: isolate the documented reset when the
                // *reference* cursor is on the top line and that line fits.
                // This is not a candidate algorithm: it reads the answer.
                let final_line = step["cursor"][1].as_u64().unwrap() as usize - 1;
                let mut adjusted = predicted;
                if final_line == predicted.source_line {
                    let line_final = editor
                        .native_wheel_final_cached(
                            &mut heights,
                            &mut layouts,
                            final_line,
                            width,
                            cursor,
                        )
                        .unwrap();
                    if line_final + 1 <= rows {
                        adjusted.visual_row = 0;
                    }
                }
                adjusted_mismatches += usize::from(adjusted != expected);
                // A second oracle experiment additionally applies the upward
                // skip-column correction with Neovim's smoothscroll marker.
                // The cursor still comes from the reference, so this is not
                // usable as production behavior or an end-to-end check.
                let mut marker_adjusted = adjusted;
                if final_line == predicted.source_line && marker_adjusted.visual_row > 0 {
                    let byte = step["cursor"][2].as_u64().unwrap() as usize - 1;
                    let scalar = lines[final_line]
                        .char_indices()
                        .take_while(|(at, _)| *at < byte)
                        .count();
                    let offset = editor.line_starts[final_line] + scalar;
                    let final_cursor = editor
                        .wrapped_cursor_at(offset, width, true)
                        .unwrap()
                        .unwrap();
                    let vcol = editor
                        .native_wheel_vcol_cached(&mut layouts, final_cursor, width, false)
                        .unwrap();
                    while marker_adjusted.visual_row > 0
                        && vcol < marker_adjusted.visual_row * width + 3 + scrolloff * width
                    {
                        marker_adjusted.visual_row -= 1;
                    }
                }
                marker_mismatches += usize::from(marker_adjusted != expected);
                // Research approximation of nv_screengo. Clean Neovim 0.12.1
                // uses nostartofline, so pagescroll does not call g0 here.
                // This still does not reproduce cursor_correct.
                let mut target = WrappedTop {
                    source_line: cursor.line,
                    visual_row: cursor.visual_row,
                };
                let desired = prev_cursor[4].as_u64().unwrap() as usize - 1;
                let max_goal = (editor
                    .native_wheel_final_cached(
                        &mut heights,
                        &mut layouts,
                        cursor.line,
                        width,
                        cursor,
                    )
                    .unwrap()
                    + 1)
                    * width
                    - 1;
                let desired = desired.min(max_goal);
                for _ in 0..count {
                    let next = editor
                        .native_wheel_step_cached(
                            &mut heights,
                            &mut layouts,
                            target,
                            down,
                            width,
                            cursor,
                        )
                        .unwrap();
                    if next == target {
                        break;
                    }
                    target = next;
                }
                let goal = target.visual_row * width + desired % width;
                let expected_goal = step["cursor"][4].as_u64().unwrap() as usize - 1;
                if goal != expected_goal {
                    desired_mismatches += 1;
                    if desired_examples.len() < 8 {
                        desired_examples.push(format!(
                            "case {case_id} {} goal {} expected {}",
                            step["key"], goal, expected_goal
                        ));
                    }
                }
                let moved_cursor = screen_coladvance(
                    &editor,
                    &mut layouts,
                    target.source_line,
                    goal,
                    width,
                    one_more,
                )
                .unwrap();
                let actual_byte = lines[moved_cursor.line]
                    .chars()
                    .take(moved_cursor.scalar)
                    .map(char::len_utf8)
                    .sum::<usize>()
                    + 1;
                let expected_line = step["cursor"][1].as_u64().unwrap() as usize - 1;
                let expected_byte = step["cursor"][2].as_u64().unwrap() as usize;
                if (moved_cursor.line, actual_byte) != (expected_line, expected_byte) {
                    cursor_mismatches += 1;
                    both_mismatches += usize::from(predicted != expected);
                    if cursor_examples.len() < 20 {
                        cursor_examples.push(format!(
                            "case {case_id} {} cursor {:?} expected {:?}, reference top {:?}, predicted top {:?}",
                            step["key"],
                            (moved_cursor.line + 1, actual_byte),
                            (expected_line + 1, expected_byte),
                            expected,
                            predicted
                        ));
                    }
                }
                let corrected = if scrolloff == 0 {
                    moved_cursor
                } else {
                    correct_cursor(
                        &editor,
                        expected,
                        moved_cursor,
                        rows,
                        width,
                        scrolloff,
                        one_more,
                        false,
                        &mut heights,
                        &mut layouts,
                    )
                    .unwrap()
                };
                let corrected_byte = lines[corrected.line]
                    .chars()
                    .take(corrected.scalar)
                    .map(char::len_utf8)
                    .sum::<usize>()
                    + 1;
                if (corrected.line, corrected_byte) != (expected_line, expected_byte) {
                    corrected_cursor_mismatches += 1;
                    corrected_examples.push(format!(
                        "case {case_id} {} corrected {:?} expected {:?}, reference top {:?}, predicted top {:?}",
                        step["key"], (corrected.line + 1, corrected_byte),
                        (expected_line + 1, expected_byte), expected, predicted
                    ));
                }
                // No current-step answer is supplied to this joint experiment.
                let joint_top = adjust_skip_candidate(
                    &editor,
                    predicted,
                    moved_cursor,
                    rows,
                    width,
                    scrolloff,
                    None,
                    &mut heights,
                    &mut layouts,
                )
                .unwrap();
                if case_id == 7 && step["key"] == "1<C-d>" && expected_byte == 2 {
                    println!("case7-independent: previous_top={top:?} predicted={predicted:?} moved_cursor={moved_cursor:?} adjusted={joint_top:?} expected={expected:?}");
                }
                let joint_top = if joint_top != predicted
                    || (joint_top.source_line == moved_cursor.line
                        && moved_cursor.visual_row >= joint_top.visual_row + rows)
                {
                    cursor_columns_candidate(
                        &editor,
                        joint_top,
                        moved_cursor,
                        rows,
                        width,
                        scrolloff,
                        None,
                        &mut heights,
                        &mut layouts,
                    )
                    .unwrap()
                } else {
                    joint_top
                };
                let joint_cursor = if scrolloff == 0 {
                    moved_cursor
                } else {
                    correct_cursor(
                        &editor,
                        joint_top,
                        moved_cursor,
                        rows,
                        width,
                        scrolloff,
                        one_more,
                        false,
                        &mut heights,
                        &mut layouts,
                    )
                    .unwrap()
                };
                let line_last = editor.native_wheel_final_cached(&mut heights, &mut layouts, joint_cursor.line, width, joint_cursor).unwrap();
                let joint_top = if line_last + 1 > rows && joint_top.source_line == joint_cursor.line && joint_cursor.visual_row < joint_top.visual_row {
                    adjust_skip_candidate(&editor, joint_top, joint_cursor, rows, width, scrolloff, None, &mut heights, &mut layouts).unwrap()
                } else { joint_top };
                let joint_byte = lines[joint_cursor.line]
                    .chars()
                    .take(joint_cursor.scalar)
                    .map(char::len_utf8)
                    .sum::<usize>()
                    + 1;
                let bad_top = joint_top != expected;
                let bad_cursor = (joint_cursor.line, joint_byte) != (expected_line, expected_byte);
                joint_top_mismatches += usize::from(bad_top);
                joint_cursor_mismatches += usize::from(bad_cursor);
                if (bad_top || bad_cursor) && joint_examples.len() < 8 {
                    joint_examples.push(format!(
                        "case {case_id} {} joint {:?}/{:?}, expected {:?}/{:?}",
                        step["key"],
                        joint_top,
                        (joint_cursor.line + 1, joint_byte),
                        expected,
                        (expected_line + 1, expected_byte)
                    ));
                }
                // Isolate each event from any earlier model error.
                top = expected;
                prev_cursor = step["cursor"].clone();
            }
        }
        println!("half-page viewport diagnostic: {steps} steps, {mismatches} mismatches");
        println!("after oracle line-fits reset: {adjusted_mismatches} mismatches");
        println!("after oracle marker/scrolloff correction: {marker_mismatches} mismatches");
        println!("cursor approximation before cursor_correct: {cursor_mismatches} mismatches");
        println!(
            "cursor after approximate correction with reference top: {corrected_cursor_mismatches} mismatches"
        );
        for example in corrected_examples {
            println!("{example}");
        }
        println!("viewport and cursor both differ: {both_mismatches} events");
        println!("unadjusted desired column: {desired_mismatches} mismatches");
        for example in desired_examples {
            println!("{example}");
        }
        println!(
            "joint one-step experiment: top {joint_top_mismatches}, cursor {joint_cursor_mismatches} mismatches"
        );
        for example in joint_examples {
            println!("{example}");
        }
        for example in cursor_examples {
            println!("{example}");
        }
        for example in examples {
            println!("{example}");
        }
        assert_eq!(steps, 180);
        let cases = fixture["cases"].as_array().unwrap();
        if cases.iter().all(|case| case["scrolloff"] == 0) {
            // Narrow gate: compare cursor line and byte only. The viewport,
            // modes and full sequential behavior remain unverified.
            assert_eq!(cursor_mismatches, 0, "zero-scrolloff cursor reference");
        }
    }
}
