#!/usr/bin/env python3
"""Compare synthetic wrapped Ctrl+e/y with mouse-wheel traces in clean Neovim.

Uses isolated HOME/XDG, no personal configuration, clipboard or sessions.
Run without --record to independently reproduce the checked-in fixture.
"""
import argparse
import importlib.util
import json
from pathlib import Path

source = Path(__file__).resolve().with_name('probe_wrap_wheel_wide.py')
spec = importlib.util.spec_from_file_location('probe_wrap_wheel_wide', source)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
collect = module.collect


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--record', action='store_true')
    args = parser.parse_args()
    cases = []
    for mode in ('normal', 'visual', 'visual-line', 'visual-block'):
        for count in (1, 2, 7):
            details = mode == 'visual-block'
            keys = collect(mode=mode, details=details, control=True,
                           scroll_amount=count, compact=True)
            wheel = collect(mode=mode, details=details, scroll_amount=count, compact=True)
            if keys != wheel:
                raise RuntimeError(f'Control keys differ from mouse wheel: {mode}, count {count}')
            cases.extend(keys['cases'])
            print(f'{mode} count {count}: {len(keys["cases"])} cases matched', flush=True)
    large = collect(control=True, scroll_amount=101, compact=True, long_line_test=True)
    large_wheel = collect(scroll_amount=101, compact=True, long_line_test=True)
    if large != large_wheel:
        raise RuntimeError('Control keys differ from mouse wheel: long line, count 101')
    cases.extend(large['cases'])
    data = {'version': keys['version'], 'cases': cases}
    fixture = Path(__file__).resolve().parents[1] / 'Resources/Fixtures/nvim-wrap-control-scroll.json'
    if args.record:
        if fixture.is_symlink():
            raise RuntimeError('Refusing a symlink fixture')
        fixture.write_text(json.dumps(data, ensure_ascii=False, separators=(',', ':')) + '\n')
    elif json.loads(fixture.read_text()) != data:
        raise RuntimeError('Neovim control-scroll reference differs')
    print(json.dumps({'cases': len(cases), 'steps': sum(len(c['steps']) for c in cases),
                      'recorded': args.record}))


if __name__ == '__main__':
    main()
