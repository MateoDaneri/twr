impl Editor {
    fn wheel_positions_cached<'a>(&self, cache: &'a mut std::collections::HashMap<usize, Vec<WrapPosition>>, line: usize, cols: usize) -> Result<&'a [WrapPosition], &'static str> {
        if !cache.contains_key(&line) {
            let start=self.line_starts[line];
            let positions=self.wrapped_positions(&self.text[start..self.line_end(start)], cols)?;
            let used: usize=cache.values().map(Vec::len).sum();
            if used.saturating_add(positions.len()) > 262_144 {cache.clear();}
            cache.insert(line, positions);
        }
        Ok(cache.get(&line).unwrap())
    }
    fn wheel_vcol_cached(&self, cache: &mut std::collections::HashMap<usize, Vec<WrapPosition>>, cursor: WrapCursor, cols: usize, insert: bool) -> Result<usize, &'static str> {
        let start=self.line_starts[cursor.line]; let text=&self.text[start..self.line_end(start)];
        if cursor.scalar > text.len() { return Err("invalid wheel cursor"); }
        if self.folded_line_count(start).is_some() {return Ok(cursor.visual_row * cols + self.display_text_column(&text[..cursor.scalar]).min(cols-1));}
        let pos=self.wheel_positions_cached(cache,cursor.line,cols)?;
        if cursor.scalar == text.len() { return Ok(cursor.visual_row*cols + pos.last().map_or(0,|p|p.end%cols)); }
        let p=pos.get(cursor.scalar).ok_or("invalid wheel cursor")?;
        let offset=cursor.visual_row.checked_sub(p.row).ok_or("bad row")?.checked_mul(cols).ok_or("overflow")?;
        if p.end <= offset { return Err("invalid wheel cursor"); }
        let begin=p.start.saturating_sub(offset).min(cols-1); let end=p.end.saturating_sub(offset).min(cols);
        Ok(cursor.visual_row*cols+if text[cursor.scalar]=='\t' && !insert {end.saturating_sub(1)} else {begin})
    }
    fn wheel_coladvance_cached(&self, cache: &mut std::collections::HashMap<usize, Vec<WrapPosition>>, line: usize, wanted: usize, cols: usize, one_more: bool) -> Result<WrapCursor, &'static str> {
        let pos=self.wheel_positions_cached(cache,line,cols)?;
        if one_more && pos.last().is_some_and(|p| wanted >= p.row*cols+p.end) {
            let p=pos.last().unwrap(); return Ok(WrapCursor {line,scalar:pos.len(),visual_row:p.row+p.end/cols});
        }
        let mut scalar=pos.partition_point(|p|p.row*cols+p.start<=wanted).saturating_sub(1);
        while scalar>0 && pos[scalar-1] == pos[scalar] {scalar-=1;}
        Ok(WrapCursor {line,scalar,visual_row:pos.get(scalar).map_or(0,|p|p.row+p.end.saturating_sub(1)/cols)})
    }
    fn wheel_retain_byte_cached(&self, cache: &mut std::collections::HashMap<usize, Vec<WrapPosition>>, cursor: WrapCursor, target: usize, cols: usize) -> Result<WrapCursor, &'static str> {
        if self.source_row(self.visible_row(target)) != target {return Err("bad retained cursor");}
        let origin_start=self.line_starts[cursor.line]; let origin=&self.text[origin_start..self.line_end(origin_start)];
        let byte_col: usize=origin.iter().take(cursor.scalar).map(|c|c.len_utf8()).sum();
        let start=self.line_starts[target]; let chars=&self.text[start..self.line_end(start)];
        let mut bytes=0; let mut scalar=0;
        for c in chars {if bytes+c.len_utf8()>byte_col {break;} bytes+=c.len_utf8(); scalar+=1;}
        scalar=scalar.min(chars.len());
        if self.folded_line_count(start).is_some() {return Ok(WrapCursor{line:target,scalar,visual_row:0});}
        let pos=self.wheel_positions_cached(cache,target,cols)?;
        let visual_row=if scalar==chars.len() {pos.last().map_or(0,|p|p.row+p.end/cols)} else {pos.get(scalar).map_or(0,|p|p.row+p.end.saturating_sub(1)/cols)};
        Ok(WrapCursor{line:target,scalar,visual_row})
    }
    fn wheel_final_cached(&self, heights: &mut std::collections::HashMap<(usize, bool), usize>, layouts: &mut std::collections::HashMap<usize, Vec<WrapPosition>>, line: usize, cols: usize, cursor: WrapCursor) -> Result<usize, &'static str> {
        let start=self.line_starts[line];
        if self.folded_line_count(start).is_some() {return Ok(0);}
        let eol=cursor.line==line && cursor.scalar==self.line_end(start)-start;
        if let Some(result)=heights.get(&(line,eol)) {return Ok(*result);}
        let positions=self.wheel_positions_cached(layouts,line,cols)?;
        let mut last=positions.iter().map(|p|p.row+p.end.saturating_sub(1)/cols).max().unwrap_or(0);
        if eol && let Some(p)=positions.last() {last=last.max(p.row+p.end/cols);}
        heights.insert((line,eol),last); Ok(last)
    }
    fn wheel_step_cached(&self, heights: &mut std::collections::HashMap<(usize, bool), usize>, layouts: &mut std::collections::HashMap<usize, Vec<WrapPosition>>, top: WrappedTop, down: bool, cols: usize, cursor: WrapCursor) -> Result<WrappedTop, &'static str> {
        let visible=self.visible_row(top.source_line.min(self.line_starts.len()-1)); let source_line=self.source_row(visible);
        let last=self.wheel_final_cached(heights,layouts,source_line,cols,cursor)?; let current=top.visual_row.min(last);
        if down && current<last {return Ok(WrappedTop{source_line,visual_row:current+1});}
        if !down && current>0 {return Ok(WrappedTop{source_line,visual_row:current-1});}
        let next_visible=if down {visible.saturating_add(1)} else {visible.saturating_sub(1)}; let next_line=self.source_row(next_visible);
        if next_line==source_line {return Ok(WrappedTop{source_line,visual_row:current});}
        Ok(WrappedTop{source_line:next_line,visual_row:if down {0} else {self.wheel_final_cached(heights,layouts,next_line,cols,cursor)?}})
    }
    fn experimental_wrap_wheel(&self, mut top: WrappedTop, mut cursor: WrapCursor, mut goal: usize, rows: usize, cols: usize, delta: isize, one_more: bool, insert: bool) -> Result<(WrappedTop, WrapCursor,usize), &'static str> {
        if rows == 0 || rows > 100 || cols == 0 || cols > 200 { return Err("bad viewport"); }
        let mut final_rows=std::collections::HashMap::new();
        let mut layouts=std::collections::HashMap::new();
        let down = delta > 0;
        let original_line = cursor.line;
        let original_view = self.wrapped_viewport_for_cursor(top, rows, cols, cursor)?;
        let cursor_row = self.wrapped_cursor_cell(&original_view, cursor).map_or(0, |cell| cell.row);
        let original_vcol = self.wheel_vcol_cached(&mut layouts, cursor, cols, insert)?;
        let line_height = (self.wheel_final_cached(&mut final_rows, &mut layouts, cursor.line, cols, cursor)? + 1).min(rows);
        let mut scrolled = 0usize;
        for _ in 0..delta.unsigned_abs().min(100) {
            let next = self.wheel_step_cached(&mut final_rows, &mut layouts, top, down, cols, cursor)?;
            if next == top { break; }
            top = next;
            scrolled += 1;
        }
        if down && cursor.line < top.source_line {
            cursor = self.wheel_coladvance_cached(&mut layouts, top.source_line, goal, cols, one_more)?;
        } else if !down {
            let mut bottom = (cursor_row + scrolled + line_height - 1) as isize - (original_vcol / cols) as isize;
            let mut line = cursor.line;
            while bottom >= rows as isize && line > 0 {
                bottom -= (self.wheel_final_cached(&mut final_rows, &mut layouts, line, cols, cursor)? + 1).min(rows) as isize;
                let previous = self.source_row(self.visible_row(line).saturating_sub(1));
                if previous == line { break; }
                line = previous;
            }
            if line != cursor.line { cursor = self.wheel_coladvance_cached(&mut layouts, line, goal, cols, one_more)?; }
            if cursor.line < top.source_line {
                cursor = self.wheel_retain_byte_cached(&mut layouts, cursor, top.source_line, cols)?;
            }
        }
        let view = self.wrapped_viewport_for_cursor(top, rows, cols, cursor)?;
        if view.is_empty() { return Err("empty viewport"); }
        let last = view.last().unwrap();
        let last_line=self.source_line_at(last.source.start);
        let last_top = WrappedTop { source_line: last_line, visual_row: last.visual_row };
        let at_end = self.wheel_step_cached(&mut final_rows, &mut layouts, last_top, true, cols, cursor)? == last_top;
        let mut above_wanted = 4;
        let mut below_wanted = 4;
        if top.source_line == 0 { above_wanted = 0; below_wanted = below_wanted.min(rows / 2); }
        if at_end { below_wanted = 0; above_wanted = above_wanted.min(rows.saturating_sub(1) / 2); }
        let mut low = top.source_line;
        let mut high = last_line;
        if last.visual_row < self.wheel_final_cached(&mut final_rows, &mut layouts, last_line, cols, cursor)? && high > low {
            high = self.source_row(self.visible_row(high).saturating_sub(1));
        }
        let (mut above, mut below) = (0usize, 0usize);
        while (above < above_wanted || below < below_wanted) && low < high {
            if below < below_wanted && (below <= above || above >= above_wanted) {
                below += self.wheel_final_cached(&mut final_rows, &mut layouts, high, cols, cursor)? + 1;
                high = self.source_row(self.visible_row(high).saturating_sub(1));
            }
            if above < above_wanted && (above < below || below >= below_wanted) && low < high {
                above += self.wheel_final_cached(&mut final_rows, &mut layouts, low, cols, cursor)? + 1;
                low = self.source_row(self.visible_row(low).saturating_add(1));
            }
        }
        let mut target = cursor.line;
        if low==high {target=low;}
        else {
            if cursor.line<low && top.source_line>0 { target=low; }
            if cursor.line>high && !at_end { target=high; }
        }
        if target != cursor.line {
            cursor = self.wheel_retain_byte_cached(&mut layouts, cursor, target, cols)?;
        }
        if cursor.line==top.source_line {
            let positions=self.wheel_positions_cached(&mut layouts, cursor.line, cols)?;
            let size=positions.last().map_or(0,|p|p.row*cols+p.end);
            let mut so=4*cols;
            if top.source_line==0 && top.visual_row==0 { so=0; }
            else { so=so.min((rows-1)*cols/2); }
            while so>size && so>=2*cols { so-=cols; }
            if so>=cols && so>size { so-=cols; }
            let min=top.visual_row*cols+so;
            let max=top.visual_row*cols+rows*cols-so;
            let previous_vcol=self.wheel_vcol_cached(&mut layouts, cursor,cols,insert)?;
            let mut vcol=previous_vcol;
            if vcol<min { while vcol<min { vcol+=cols; } }
            else { while vcol>=max {vcol=vcol.saturating_sub(cols);} }
            let adjusted=self.wheel_coladvance_cached(&mut layouts, cursor.line,vcol,cols,one_more)?;
            if vcol!=previous_vcol {cursor=adjusted;goal=vcol;}
        }
        if cursor.line != original_line {cursor=self.wheel_coladvance_cached(&mut layouts, cursor.line,goal,cols,one_more)?;}
        // Neovim recalculates skipcol for a cursor in a line taller than the window.
        if cursor.line == top.source_line {
            let line_rows = self.wheel_final_cached(&mut final_rows, &mut layouts, cursor.line, cols, cursor)? + 1;
            if line_rows > rows && rows <= 8 {
                let vc = self.wheel_vcol_cached(&mut layouts, cursor,cols,insert)?;
                let max_skip = line_rows.saturating_sub(rows);
                top.visual_row = (vc / cols).saturating_sub(rows / 2).min(max_skip);
            }
        }
        Ok((top,cursor,goal))
    }
}

#[cfg(test)]
mod wheel_reference_audit {
    use super::*;
    #[test]
    fn compare_wheel_reference() {
        let mut data: serde_json::Value = serde_json::from_str(include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-wide-unicode.json")).unwrap();
        let ascii: serde_json::Value = serde_json::from_str(include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-matrix.json")).unwrap();
        for mut case in ascii["cases"].as_array().unwrap().clone() {
            case["width"] = serde_json::json!(20);
            case["initial_cursor"] = serde_json::json!([0, 1, 1, 0, 1]);
            data["cases"].as_array_mut().unwrap().push(case);
        }
        let mut mismatches = 0; let mut bad_top = 0; let mut bad_cursor = 0; let mut bad_goal = 0; let mut steps = 0; let mut examples = Vec::new();
        for (case_idx, case) in data["cases"].as_array().unwrap().iter().enumerate() {
            let lines: Vec<_> = case["lines"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
            let mut editor = Editor::new(lines.join("\n"));
            editor.indent_style.tabstop = 8;
            let mode = case["steps"][0]["mode"].as_str().unwrap_or("n");
            let one_more = matches!(mode, "v" | "V" | "i");
            let insert = mode == "i";
            let rows = case["rows"].as_u64().unwrap() as usize;
            let width = case["width"].as_u64().unwrap() as usize;
            let mut top = WrappedTop { source_line: case["initial_topline"].as_u64().unwrap() as usize - 1, visual_row: case["initial_skipcol"].as_u64().unwrap() as usize / width };
            let initial = case["initial_cursor"].as_array().unwrap();
            let start_line = initial[1].as_u64().unwrap() as usize - 1;
            let start_byte = initial[2].as_u64().unwrap() as usize - 1;
            let start_scalar = lines[start_line].char_indices().take_while(|(byte, _)| *byte < start_byte).count();
            let mut cursor = editor.wrapped_cursor_at(editor.line_starts[start_line] + start_scalar, width, one_more).unwrap().unwrap();
            let mut goal = initial[4].as_u64().unwrap() as usize - 1;
            for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                let delta = if step["key"] == "down" { 3 } else { -3 };
                (top, cursor, goal) = editor.experimental_wrap_wheel(top, cursor, goal, rows, width, delta, one_more, insert).unwrap();
                let pos = step["cursor"].as_array().unwrap();
                let line = pos[1].as_u64().unwrap() as usize - 1;
                let byte = pos[2].as_u64().unwrap() as usize - 1;
                let scalar = lines[line].char_indices().take_while(|(byte_index, _)| *byte_index < byte).count();
                let expected = editor.wrapped_cursor_at(editor.line_starts[line] + scalar, width, one_more).unwrap().unwrap();
                let expected_top = WrappedTop { source_line: step["topline"].as_u64().unwrap() as usize - 1, visual_row: step["skipcol"].as_u64().unwrap() as usize / width };
                steps += 1;
                if goal + 1 != pos[4].as_u64().unwrap() as usize { bad_goal += 1; if examples.len() < 12 { examples.push(format!("case={case_idx} step={index} goal={goal} expected={} cursor={cursor:?} top={top:?}", pos[4])); } }
                if top != expected_top { bad_top += 1; }
                if cursor != expected { bad_cursor += 1; }
                if top != expected_top || cursor != expected {
                    mismatches += 1;
                    if examples.len() < 12 { examples.push(format!("case={case_idx} step={index} key={} top={top:?}/{expected_top:?} cursor={cursor:?}/{expected:?}",step["key"])); }
                }
            }
        }
        println!("WHEEL_AUDIT steps={steps} mismatches={mismatches} top={bad_top} cursor={bad_cursor} goal={bad_goal} examples={examples:?}");
        assert_eq!((mismatches, bad_goal), (0, 0), "wheel model differs from reference: {examples:?}");
    }
}

#[cfg(test)]
mod wheel_research_perf {
    use super::*;
    #[test]
    fn diagnostic_wheel_cost() {
        for size in [4096usize, 16384, 65536] {
            let editor=Editor::new("ab cd ".repeat(size / 6));
            let cursor=editor.wrapped_cursor_at(0, 80, false).unwrap().unwrap();
            let mut state=(WrappedTop { source_line: 0, visual_row: 0 }, cursor, 0);
            let start=std::time::Instant::now();
            for _ in 0..10 { state=editor.experimental_wrap_wheel(state.0,state.1,state.2,24,80,3,false,false).unwrap(); }
            eprintln!("PERF bytes={size} events=10 micros={}",start.elapsed().as_micros());
        }
    }
}
#[cfg(test)]
mod wheel_cache_properties {
    use super::*;
    #[test]
    fn cached_positions_have_a_per_event_bound() {
        let line="xy ".repeat(23_000);
        let editor=Editor::new([line.clone(),line.clone(),line.clone(),line.clone(),line].join("\n"));
        let mut cache=std::collections::HashMap::new();
        for index in 0..5 {
            assert_eq!(editor.wheel_positions_cached(&mut cache,index,80).unwrap().len(),69_000);
            assert!(cache.values().map(Vec::len).sum::<usize>() <= 262_144);
        }
    }
    #[test]
    fn positions_are_monotonic_for_binary_search() {
        let alphabet=['a',' ','\t','界','\u{301}'];
        for width in [2,3,5,13,20] {
            for mut n in 0..15625usize {
                let mut line=Vec::new();
                for _ in 0..6 {line.push(alphabet[n%5]);n/=5;}
                let editor=Editor::new(String::new());
                let positions=editor.wrapped_positions(&line,width).unwrap();
                for pair in positions.windows(2) {
                    assert!(pair[0].row*width+pair[0].start <= pair[1].row*width+pair[1].start,"width={width} line={line:?} positions={positions:?}");
                }
            }
        }
    }
}
