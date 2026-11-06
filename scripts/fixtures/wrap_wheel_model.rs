// Diagnostic only: incomplete Neovim wheel model, injected into an isolated
// snapshot by probe_wrap_wheel_model.py. Never used by the running application.
impl Editor {
    fn wheel_vcol(&self, cursor: WrapCursor, cols: usize) -> Result<usize, &'static str> {
        let start=self.line_starts[cursor.line]; let text=&self.text[start..self.line_end(start)];
        let pos=self.wrapped_positions(text,cols)?;
        Ok(pos.get(cursor.scalar).map_or(0,|p|p.row*cols+p.start))
    }
    fn wheel_coladvance(&self, line: usize, wanted: usize, cols: usize) -> Result<WrapCursor, &'static str> {
        let start=self.line_starts[line]; let text=&self.text[start..self.line_end(start)];
        let pos=self.wrapped_positions(text,cols)?;
        let scalar=pos.iter().rposition(|p|p.row*cols+p.start<=wanted).unwrap_or(0);
        Ok(WrapCursor {line,scalar,visual_row:pos.get(scalar).map_or(0,|p|p.row+p.end.saturating_sub(1)/cols)})
    }
    fn experimental_wrap_wheel(&self, mut top: WrappedTop, mut cursor: WrapCursor, mut goal: usize, rows: usize, cols: usize, delta: isize) -> Result<(WrappedTop, WrapCursor,usize), &'static str> {
        if rows == 0 || rows > 100 || cols == 0 || cols > 200 { return Err("bad viewport"); }
        let down = delta > 0;
        for _ in 0..delta.unsigned_abs().min(100) {
            let next = self.step_wrapped_top_for_cursor(top, down, cols, cursor)?;
            if next == top { break; }
            top = next;
        }
        let view = self.wrapped_viewport_for_cursor(top, rows, cols, cursor)?;
        if view.is_empty() { return Err("empty viewport"); }
        let last = view.last().unwrap();
        let last_line=self.source_line_at(last.source.start);
        let last_top = WrappedTop { source_line: last_line, visual_row: last.visual_row };
        let at_end = self.step_wrapped_top_for_cursor(last_top, true, cols, cursor)? == last_top;
        let mut above_wanted = 4;
        let mut below_wanted = 4;
        if top.source_line == 0 { above_wanted = 0; below_wanted = below_wanted.min(rows / 2); }
        if at_end { below_wanted = 0; above_wanted = above_wanted.min(rows.saturating_sub(1) / 2); }
        let mut low = top.source_line;
        let mut high = last_line;
        if last.visual_row < self.wrapped_final_row(last_line, cols, Some(cursor))? && high > low {
            high = self.source_row(self.visible_row(high).saturating_sub(1));
        }
        let (mut above, mut below) = (0usize, 0usize);
        while (above < above_wanted || below < below_wanted) && low < high {
            if below < below_wanted && (below <= above || above >= above_wanted) {
                below += self.wrapped_final_row(high, cols, Some(cursor))? + 1;
                high = self.source_row(self.visible_row(high).saturating_sub(1));
            }
            if above < above_wanted && (above < below || below >= below_wanted) && low < high {
                above += self.wrapped_final_row(low, cols, Some(cursor))? + 1;
                low = self.source_row(self.visible_row(low).saturating_add(1));
            }
        }
        let mut target = cursor.line;
        if low==high {target=low;}
        else {
            if cursor.line<low && top.source_line>0 { target=low; }
            if cursor.line>high && !at_end { target=high; }
        }
        let previous_line = cursor.line;
        if target != cursor.line {
            let start = self.line_starts[target];
            let len = self.line_end(start) - start;
            cursor = self.wrapped_cursor_at(start + cursor.scalar.min(len.saturating_sub(1)), cols, false)?.ok_or("bad cursor")?;
        }
        if cursor.line==top.source_line {
            let start=self.line_starts[cursor.line]; let text=&self.text[start..self.line_end(start)];
            let positions=self.wrapped_positions(text,cols)?;
            let size=positions.last().map_or(0,|p|p.row*cols+p.end);
            let mut so=4*cols;
            if top.source_line==0 && top.visual_row==0 { so=0; }
            else { so=so.min((rows-1)*cols/2); }
            while so>size && so>=2*cols { so-=cols; }
            if so>=cols && so>size { so-=cols; }
            let min=top.visual_row*cols+so;
            let max=top.visual_row*cols+rows*cols-so;
            let previous_vcol=self.wheel_vcol(cursor,cols)?;
            let mut vcol=previous_vcol;
            if vcol<min { while vcol<min { vcol+=cols; } }
            else { while vcol>=max {vcol=vcol.saturating_sub(cols);} }
            let adjusted=self.wheel_coladvance(cursor.line,vcol,cols)?;
            if vcol!=previous_vcol {cursor=adjusted;goal=vcol;}
        }
        if cursor.line != previous_line {cursor=self.wheel_coladvance(cursor.line,goal,cols)?;}
        // Neovim recalculates skipcol for a cursor in a line taller than the window.
        if cursor.line == top.source_line {
            let line_rows = self.wrapped_final_row(cursor.line, cols, Some(cursor))? + 1;
            if line_rows > rows && rows <= 8 {
                let vc = self.wheel_vcol(cursor,cols)?;
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
        let data: serde_json::Value = serde_json::from_str(include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-matrix.json")).unwrap();
        let mut mismatches = 0; let mut bad_top = 0; let mut bad_cursor = 0; let mut steps = 0; let mut examples = Vec::new();
        for (case_idx, case) in data["cases"].as_array().unwrap().iter().enumerate() {
            let lines: Vec<_> = case["lines"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
            let editor = Editor::new(lines.join("\n"));
            let rows = case["rows"].as_u64().unwrap() as usize;
            let mut top = WrappedTop { source_line: case["initial_topline"].as_u64().unwrap() as usize - 1, visual_row: case["initial_skipcol"].as_u64().unwrap() as usize / 20 };
            let mut cursor = editor.wrapped_cursor_at(0, 20, false).unwrap().unwrap();
            let mut goal = 0;
            for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                let delta = if step["key"] == "down" { 3 } else { -3 };
                (top, cursor, goal) = editor.experimental_wrap_wheel(top, cursor, goal, rows, 20, delta).unwrap();
                let pos = step["cursor"].as_array().unwrap();
                let line = pos[1].as_u64().unwrap() as usize - 1;
                let scalar = pos[2].as_u64().unwrap() as usize - 1;
                let expected = editor.wrapped_cursor_at(editor.line_starts[line] + scalar, 20, false).unwrap().unwrap();
                let expected_top = WrappedTop { source_line: step["topline"].as_u64().unwrap() as usize - 1, visual_row: step["skipcol"].as_u64().unwrap() as usize / 20 };
                steps += 1;
                if top != expected_top { bad_top += 1; }
                if cursor != expected { bad_cursor += 1; }
                if top != expected_top || cursor != expected {
                    mismatches += 1;
                    if examples.len() < 12 { examples.push(format!("case={case_idx} step={index} key={} top={top:?}/{expected_top:?} cursor={cursor:?}/{expected:?}",step["key"])); }
                }
            }
        }
        assert_eq!(mismatches, 0, "Wheel reference: {mismatches}/{steps} differ; top={bad_top}, cursor={bad_cursor}; examples={examples:?}");
    }
}
