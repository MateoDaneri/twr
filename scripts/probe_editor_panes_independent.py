#!/usr/bin/env python3
"""Compare pane histories with one isolated, clean Neovim per scenario.

Creates private synthetic processes and files under /private/tmp. It never opens
real projects, user configuration or existing editor/terminal sessions.
"""
import contextlib
import io
import json
from pathlib import Path
import random
import tempfile

import probe_editor_panes_reference as probe


def scenarios():
    targeted = [
        ('subtree-left-close', ['v', 't', 's', 'b', 'c']),
        ('subtree-right-close', ['v', 's', 't', 'c']),
        ('horizontal-above-close', ['s', 't', 'v', 'b', 'c']),
        ('horizontal-below-close', ['s', 'v', 't', 'c']),
        ('three-row-close-middle', ['v', 't', 's', 'b', 'v', 't', 'w', 'w', 'c']),
        ('three-col-close-middle', ['s', 't', 'v', 'b', 's', 't', 'w', 'w', 'c']),
        ('nested-left-last', ['v', 't', 's', 'b', 'v', 'c']),
        ('nested-right-first', ['v', 's', 'b', 'v', 't', 'c']),
    ]
    result = [(label, [['<C-w>', key] for key in keys]) for label, keys in targeted]
    rng = random.Random(31877)
    for index in range(32):
        commands = []
        count = 1
        for _ in range(rng.randint(7, 13)):
            choices = ['t', 'b', 'w', 'W', 'p']
            if count < 5:
                choices += ['v', 's']
            if count > 1:
                choices += ['c']
            key = rng.choice(choices)
            if key in ('v', 's'):
                count += 1
            elif key == 'c':
                count -= 1
            commands.append(['<C-w>', key])
        while count > 1:
            commands.append(['<C-w>', 'c'])
            count -= 1
        label = 'generated-' + chr(97 + index // 26) + chr(97 + index % 26)
        result.append((label, commands))
    return result


def main():
    observed = []
    data = None
    for case in scenarios():
        # A fresh server for each case avoids winnr('#') history leaking across
        # scenarios. probe.main has its own private HOME, socket and deadline.
        probe.CASES = [case]
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            probe.main(compare_baseline=False)
        receipt = json.loads(output.getvalue())
        data = json.loads(Path(receipt['reference']).read_text())
        observed.extend(data['cases'])
    data['cases'] = observed
    data['scope'] = 'Independent clean Neovim processes; topology, focus history and cursors only; no GUI/tmux'
    generated = probe.rust_fixture(data).replace('const CASES:', 'const INDEPENDENT_CASES:', 1)
    stage = Path(tempfile.mkdtemp(prefix='tw-independent-pane-reference-', dir='/private/tmp'))
    stage.chmod(0o700)
    output = stage / 'results.json'
    output.write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n')
    (stage / 'reference.rs').write_text(generated)
    baseline = Path(__file__).resolve().parents[1] / 'Resources/Fixtures/nvim-editor-panes-independent.json'
    if baseline.exists() and json.loads(baseline.read_text()) != data:
        raise RuntimeError('Independent reference changed; evidence: ' + str(output))
    if baseline.exists() and baseline.with_suffix('.rs').read_text() != generated:
        raise RuntimeError('Independent Rust fixture changed; evidence: ' + str(stage / 'reference.rs'))
    print(json.dumps({'reference': str(output), 'cases': len(observed), 'baseline_compared': baseline.exists()}))


if __name__ == '__main__':
    main()
