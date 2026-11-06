#!/usr/bin/env python3
"""Compare the pre-fix logical-line paging rule with Neovim's wrapped trace.

This is a diagnostic, not a test of the GUI. Each step starts from the recorded
Neovim state so that one mismatch cannot cascade into subsequent comparisons.
"""
import json
from pathlib import Path

fixture = Path(__file__).resolve().parents[1] / 'Resources/Fixtures/nvim-wrap-half-page.json'
cases = json.loads(fixture.read_text())['cases']
summary = {'steps': 0, 'cursor_line_mismatches': 0, 'viewport_mismatches': 0,
           'count_mismatches': 0, 'examples': []}
for index, case in enumerate(cases):
    rows = case['rows']
    last = len(case['lines']) - 1
    row = case['initial_cursor'][1] - 1
    top = case['initial_topline'] - 1
    scroll = max(1, rows // 2)
    for step in case['steps']:
        key = step['key']
        prefix = key.split('<', 1)[0]
        if prefix:
            scroll = int(prefix)  # the pre-fix rule retained it without a window clamp
        down = 'C-d' in key
        amount = min(scroll, last - row if down else row)
        predicted_row = row + amount if down else row - amount
        predicted_top = min(last, top + amount) if down else max(0, top - amount)
        expected_row, expected_top = step['cursor'][1] - 1, step['topline'] - 1
        wrong_row = predicted_row != expected_row
        wrong_top = predicted_top != expected_top or step['skipcol'] != 0
        wrong_count = scroll != step['scroll']
        summary['steps'] += 1
        summary['cursor_line_mismatches'] += wrong_row
        summary['viewport_mismatches'] += wrong_top
        summary['count_mismatches'] += wrong_count
        if (wrong_row or wrong_top or wrong_count) and len(summary['examples']) < 5:
            summary['examples'].append({'case': index, 'key': key,
                'predicted': [predicted_row + 1, predicted_top + 1, 0, scroll],
                'reference': [expected_row + 1, expected_top + 1, step['skipcol'], step['scroll']]})
        row, top = expected_row, expected_top
print(json.dumps(summary, indent=2))
