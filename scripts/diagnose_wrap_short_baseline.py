#!/usr/bin/env python3
"""Compare a short-line approximation of TWR's wrapped follow to Neovim.

This diagnostic is deliberately not an implementation or acceptance test. It
uses the final cursor from each Neovim reference step, then propagates the
predicted viewport; lines are assumed short, unfolded, and one screen row tall.
"""
import argparse
import json
from pathlib import Path


def follow(top, cursor, line_count, height, scrolloff):
    if cursor < top:
        top = cursor
    elif cursor >= top + height:
        top = cursor - height + 1
    row = cursor - top
    upper = min(scrolloff, height // 2)
    lower = min(scrolloff, (height - 1) // 2)
    if row < upper:
        top = max(0, top - (upper - row))
    elif row > height - 1 - lower:
        top = min(line_count - 1, top + row - (height - 1 - lower))
    return top


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--examples', type=int, default=8)
    args = parser.parse_args()
    fixture = Path(__file__).resolve().parents[1] / 'Resources/Fixtures/nvim-wrap-short-matrix.json'
    data = json.loads(fixture.read_text())
    result = {'reference': data['version'], 'cases': len(data['cases']), 'steps': 0,
              'cases_with_mismatch': 0, 'mismatched_steps': 0, 'examples': []}
    for case in data['cases']:
        top = case['initial_top']
        mismatch = False
        for index, (key, cursor, expected) in enumerate(case['steps']):
            old = top
            top = follow(top, cursor, case['line_count'], case['rows'], case['scrolloff'])
            result['steps'] += 1
            if top != expected:
                mismatch = True
                result['mismatched_steps'] += 1
                if len(result['examples']) < args.examples:
                    result['examples'].append({'rows': case['rows'], 'scrolloff': case['scrolloff'],
                        'line_count': case['line_count'], 'step': index, 'key': key,
                        'old_predicted_top': old, 'cursor': cursor,
                        'reference_top': expected, 'predicted_top': top})
        result['cases_with_mismatch'] += mismatch
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
