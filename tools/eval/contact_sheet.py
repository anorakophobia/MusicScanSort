"""Contact sheet of the upper part of the first page of every PDF in a folder.

    python tools/eval/contact_sheet.py FOLDER OUT.png [--from N] [--to M] [--cols 10]

Each tile is labelled with its page number in sorted file order and the last digits of the
file name; useful to see quickly what a batch of scans contains.
"""
import argparse
from pathlib import Path

import pymupdf
from PIL import Image, ImageDraw

WIDTH = 200


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('folder')
    parser.add_argument('output')
    parser.add_argument('--from', dest='start', type=int, default=0)
    parser.add_argument('--to', dest='end', type=int, default=None)
    parser.add_argument('--cols', type=int, default=10)
    args = parser.parse_args()
    files = sorted(Path(args.folder).glob('*.pdf'))[args.start:args.end]
    tiles = []
    for number, path in enumerate(files, args.start):
        with pymupdf.open(path) as doc:
            pix = doc[0].get_pixmap(dpi=25)
            image = Image.frombytes('RGB', (pix.width, pix.height), pix.samples)
        image = image.resize((WIDTH, int(WIDTH * image.height / image.width)))
        image = image.crop((0, 0, WIDTH, int(WIDTH * 0.6)))
        ImageDraw.Draw(image).text((2, 2), f'{number} {path.stem[-4:]}', fill=(255, 0, 0))
        tiles.append(image)
    if not tiles:
        raise SystemExit('no PDF files in range')
    rows = (len(tiles) + args.cols - 1) // args.cols
    height = tiles[0].height
    sheet = Image.new('RGB', (args.cols * (WIDTH + 3), rows * (height + 3)), 'white')
    for i, tile in enumerate(tiles):
        sheet.paste(tile, ((i % args.cols) * (WIDTH + 3), (i // args.cols) * (height + 3)))
    sheet.save(args.output)
    print(f'{len(tiles)} pages → {args.output}')


if __name__ == '__main__':
    main()
