#!/usr/bin/env python3
"""Synthetic Kitty RGBA demo. No files, network, clipboard or user config reads."""
import argparse
import base64
import binascii
import struct
import sys
import zlib

# First 24 entries of Kitty's rowcolumn-diacritics table, also in the pinned core.
DIACRITICS = [0x305, 0x30D, 0x30E, 0x310, 0x312, 0x33D, 0x33E, 0x33F,
              0x346, 0x34A, 0x34B, 0x34C, 0x350, 0x351, 0x352, 0x357,
              0x35B, 0x363, 0x364, 0x365, 0x366, 0x367, 0x368, 0x369]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tmux', action='store_true', help='Wrap graphics in tmux DCS passthrough')
    parser.add_argument('--once', action='store_true', help='Leave one frame for automated PTY inspection')
    parser.add_argument('--png', action='store_true', help='Send a real PNG instead of raw RGBA')
    parser.add_argument('--fixed', action='store_true', help='Diagnostic fixed placement; tmux cannot track its text anchor')
    args = parser.parse_args()
    virtual = args.tmux and not args.fixed

    def output(data):
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()

    def graphics(command):
        if args.tmux:
            command = b'\x1bPtmux;' + command.replace(b'\x1b', b'\x1b\x1b') + b'\x1b\\'
        output(command)

    def pixels(replace=False):
        colors = [(255, 0, 0, 255), (0, 255, 0, 255), (0, 0, 255, 255), (255, 255, 0, 255)]
        if replace:
            colors.reverse()
        raw = bytes(channel for y in range(64) for x in range(64)
                    for channel in colors[(y // 32) * 2 + x // 32])
        if args.png:
            def chunk(kind, data):
                return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', binascii.crc32(kind + data))
            rows = b''.join(b'\0' + raw[y * 256:(y + 1) * 256] for y in range(64))
            raw = (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>2I5B', 64, 64, 8, 6, 0, 0, 0))
                   + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))
        encoded = base64.b64encode(raw)
        chunks = [encoded[n:n + 4096] for n in range(0, len(encoded), 4096)]
        for n, chunk in enumerate(chunks):
            parameters = (b'a=T,f=' + (b'100' if args.png else b'32') + b',s=64,v=64,i=42,p=1,c=24,r=8,q=2,' + (b'U=1,' if virtual else b'')) if n == 0 else b''
            parameters += b'm=1' if n + 1 < len(chunks) else b'm=0'
            graphics(b'\x1b_G' + parameters + b';' + chunk + b'\x1b\\')
        if virtual:
            for row in range(8):
                cells = ''.join('\U0010eeee' + chr(DIACRITICS[row]) + chr(DIACRITICS[col]) for col in range(24))
                output(f'\x1b[{row + 3};1H\x1b[38;5;42m'.encode('ascii') + cells.encode('utf-8') + b'\x1b[0m')

    output(b'\x1b[?1049h\x1b[2J\x1b[HKitty RGBA / synthetic fixture\r\n')
    try:
        output('Unicode: ñ界 e\u0301 😀\r\n'.encode('utf-8'))
        output(b'\x1b[3;1H')
        pixels()
        output(b'\x1b[13;1HFRAME-READY red/green above blue/yellow\r\n')
        if args.once:
            return
        input('Enter: replace pixels under the same image ID. ')
        output(b'\x1b[3;1H')
        pixels(replace=True)
        output(b'\x1b[15;1HREPLACED yellow/blue above green/red\r\n')
        input('Enter: delete the image. ')
        graphics(b'\x1b_Ga=d,d=I,i=42,q=2\x1b\\')
        output(b'\x1b[17;1HDELETED: the image rectangle should be empty\r\n')
        input('Enter: return to the shell. ')
    finally:
        if not args.once:
            graphics(b'\x1b_Ga=d,d=I,i=42,q=2\x1b\\')
            output(b'\x1b[0m\x1b[?1049l')


if __name__ == '__main__':
    try:
        main()
    except (EOFError, KeyboardInterrupt):
        pass
