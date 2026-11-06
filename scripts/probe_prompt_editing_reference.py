#!/usr/bin/env python3
"""Synthetic command/search prompt deletion reference, clean Neovim RPC only."""
from pathlib import Path
import importlib.util

spec = importlib.util.spec_from_file_location('reference', Path(__file__).with_name('probe_search_expansion_reference.py'))
reference = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reference)
CASES = []
for prefix in ('/', ':', '?'):
    for label, query, keys in [
        ('one-character', 'foo', ['<C-h>']),
        ('unicode', 'ñ😀', ['<C-h>']),
        ('combining', 'e\u0301', ['<C-h>']),
        ('zwj', 'x👩\u200d💻', ['<C-h>']),
        ('skin', 'x👍🏽', ['<C-h>']),
        ('variation', 'x✈️', ['<C-h>']),
        ('leading-mark', '\u0301', ['<C-h>']),
        ('double-mark', 'xe\u0301\u0308', ['<C-h>']),
        ('cjk', 'foo 中文', ['<C-w>']),
        ('combining-word', 'foo e\u0301', ['<C-w>']),
        ('word', 'foo bar', ['<C-w>']),
        ('word-spaces', 'foo bar   ', ['<C-w>']),
        ('punctuation', 'foo.bar', ['<C-w>','<C-w>']),
        ('emoji-word', 'foo 😀😀', ['<C-w>']),
        ('unicode-word', 'foo ñÁfono', ['<C-w>']),
        ('underscore', 'foo_bar', ['<C-w>']),
        ('remove-all', 'foo bar', ['<C-u>']),
        ('empty-ctrl-u', '', ['<C-u>']),
        ('empty-ctrl-w', '', ['<C-w>']),
        ('empty-backspace', '', ['<C-h>']),
        ('backspace-through-empty', 'a', ['<BS>','<BS>']),
    ]:
        CASES.append({'label':prefix+'-'+label,'text':'synthetic buffer unchanged','prefix':prefix,'query':query,'keys':keys})

if __name__ == '__main__':
    reference.main(CASES, 'nvim-prompt-editing.json')
