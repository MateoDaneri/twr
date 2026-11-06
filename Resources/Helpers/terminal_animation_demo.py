#!/usr/bin/env python3
"""Two synthetic Kitty animation frames; no user files, configuration or network."""
import argparse
import base64
import sys

DIACRITICS = [0x305, 0x30D, 0x30E, 0x310, 0x312, 0x33D, 0x33E, 0x33F,
              0x346, 0x34A, 0x34B, 0x34C, 0x350, 0x351, 0x352, 0x357,
              0x35B, 0x363, 0x364, 0x365, 0x366, 0x367, 0x368, 0x369]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tmux', action='store_true')
    parser.add_argument('--once', action='store_true', help='Leave animation playing for automated PTY checks')
    parser.add_argument('--frame-ms', type=int, default=750)
    parser.add_argument('--raster', choices=['small', 'square'], default='small')
    args = parser.parse_args()
    if not 100 <= args.frame_ms <= 5000:
        parser.error('--frame-ms must be in 100..5000')
    width, height = (16, 8) if args.raster == 'small' else (64, 64)

    def out(data):
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()

    def kitty(parameters, pixels=b''):
        encoded = base64.b64encode(pixels)
        chunks = [encoded[n:n + 4096] for n in range(0, len(encoded), 4096)] or [b'']
        for index, chunk in enumerate(chunks):
            header = parameters if index == 0 else ''
            if pixels:
                header += (',' if header else '') + ('m=1' if index + 1 < len(chunks) else 'm=0')
            data = b'\x1b_G' + header.encode('ascii') + (b';' + chunk if pixels else b'') + b'\x1b\\'
            if args.tmux:
                data = b'\x1bPtmux;' + data.replace(b'\x1b', b'\x1b\x1b') + b'\x1b\\'
            out(data)

    def frame(reverse):
        return b''.join(bytes([0, 0, 255, 255] if (x < width // 2) == reverse else [255, 0, 0, 255])
                        for y in range(height) for x in range(width))

    out(b'\x1b[?1049h\x1b[2J\x1b[HKitty ANIMATION / synthetic two-frame fixture\r\n')
    try:
        out(b'\x1b[3;1H')
        kitty(f'a=T,f=32,s={width},v={height},i=42,p=1,c=24,r=8,q=2' + (',U=1' if args.tmux else ''), frame(False))
        kitty(f'a=f,f=32,s={width},v={height},i=42,z={args.frame_ms},q=2', frame(True))
        kitty(f'a=a,i=42,r=1,z={args.frame_ms},q=2')
        if args.tmux:
            for row in range(8):
                cells = ''.join('\U0010eeee' + chr(DIACRITICS[row]) + chr(DIACRITICS[col]) for col in range(24))
                out(f'\x1b[{row + 3};1H\x1b[38;5;42m'.encode('ascii') + cells.encode('utf-8') + b'\x1b[0m')
        kitty('a=a,i=42,s=3,q=2')
        out(b'\x1b[14;1HFRAME-READY: red/blue halves must keep swapping without more output\r\n')
        if args.once:
            return
        input('Enter: pause. ')
        kitty('a=a,i=42,s=1,q=2')
        out(b'\x1b[16;1HPAUSED: halves must stay still\r\n')
        input('Enter: resume. ')
        kitty('a=a,i=42,s=3,q=2')
        out(b'\x1b[18;1HRESUMED: halves must swap again\r\n')
        input('Enter: delete. ')
        kitty('a=d,d=I,i=42,q=2')
        out(b'\x1b[20;1HDELETED: empty image area\r\n')
        input('Enter: shell. ')
    finally:
        if not args.once:
            kitty('a=d,d=I,i=42,q=2')
            out(b'\x1b[0m\x1b[?1049l')


if __name__ == '__main__':
    try:
        main()
    except (EOFError, KeyboardInterrupt):
        pass
