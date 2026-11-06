#!/usr/bin/env python3
"""Interactive synthetic crop/layer/scroll fixture; no filesystem or network IO."""
import argparse
import base64
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tmux', action='store_true')
    parser.add_argument('--fixed', action='store_true', help='Reproduce untracked fixed placements through tmux')
    args = parser.parse_args()
    anchored = args.tmux and not args.fixed

    def out(data):
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()

    def kitty(parameters, pixels=b''):
        command = b'\x1b_G' + parameters.encode('ascii')
        if pixels:
            command += b';' + base64.b64encode(pixels)
        command += b'\x1b\\'
        if args.tmux:
            command = b'\x1bPtmux;' + command.replace(b'\x1b', b'\x1b\x1b') + b'\x1b\\'
        out(command)

    def at(row, col):
        out(f'\x1b[{row};{col}H'.encode('ascii'))

    def screen(title):
        kitty('a=d,d=I,i=200,q=2')
        kitty('a=d,d=A,q=2')
        out(b'\x1b[0m\x1b[2J\x1b[H' + title.encode('ascii'))
        if anchored:
            kitty('a=T,U=1,f=32,s=1,v=1,i=200,p=1,c=1,r=1,q=2', bytes([0, 0, 0, 0]))
            at(2, 1)
            out('\x1b[38;5;200m\U0010eeee\u0305\u0305\x1b[0m'.encode('utf-8'))

    def relative(row, col):
        return f',P=200,Q=1,H={col - 1},V={row - 2}' if anchored else ''

    def image(image_id, pixel, row, col, columns, rows, z):
        at(row, col)
        kitty(f'a=T,f=32,s=1,v=1,i={image_id},p=1,c={columns},r={rows},z={z},q=2' + relative(row, col), bytes(pixel))

    out(b'\x1b[?1049h')
    try:
        screen('CROP: right column of RGBA 2x2 -> GREEN above YELLOW')
        kitty('a=t,f=32,s=2,v=2,i=1,q=2', bytes([255, 0, 0, 255, 0, 255, 0, 255,
                                                              0, 0, 255, 255, 255, 255, 0, 255]))
        at(3, 2)
        kitty('a=p,i=1,p=1,x=1,y=0,w=1,h=2,c=8,r=6,q=2' + relative(3, 2))
        at(12, 1)
        input('Enter: test layers. ')

        screen('LAYERS: blue below BG; red below text; yellow above text')
        image(1, [0, 0, 255, 255], 4, 2, 30, 6, -2147483648)
        at(5, 3)
        out(b'\x1b[42m' + b' ' * 27 + b'\x1b[0m')
        image(2, [255, 0, 0, 255], 5, 9, 10, 3, -1)
        at(6, 10)
        out(b'\x1b[1;37mTEXT\x1b[0m')
        image(3, [255, 255, 0, 255], 5, 23, 6, 3, 1)
        at(6, 23)
        out(b'HIDDEN')
        at(13, 1)
        input('TEXT stays visible on red. HIDDEN is covered by yellow. Enter: scroll. ')

        screen('SCROLL: the image must leave the viewport with its anchor')
        image(1, [0, 255, 255, 255], 3, 2, 12, 6, 0)
        at(12, 1)
        input('Enter: produce 120 rows. ')
        for row in range(120):
            out(f'ROW-{row:03d}\r\n'.encode('ascii'))
        input('SCROLL-FINISHED: image should be gone. Enter: shell. ')
    finally:
        kitty('a=d,d=I,i=200,q=2')
        kitty('a=d,d=A,q=2')
        out(b'\x1b[0m\x1b[?1049l')


if __name__ == '__main__':
    try:
        main()
    except (EOFError, KeyboardInterrupt):
        pass
