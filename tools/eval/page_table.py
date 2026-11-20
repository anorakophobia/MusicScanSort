"""One line per source page: the target file it belongs to and what the run log detected.

    python tools/eval/page_table.py SAMPLE [--out DIR]

Reads out-N/run.log of the newest (or given) output folder. Columns: page number, target
file and page within it (T=? if no target uses the page), detected rotation, first-page flag,
printed page number and the raw instrument label.
"""
import argparse
import re
from pathlib import Path

import pymupdf

from common import TARGETS, latest_out, page_hash, part_name, read_log, source_index

FIELDS = 'orientation correction|instrument_raw|page_number|is_part_start'


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('sample')
    parser.add_argument('--out', help='output folder with run.log (default: newest out-N)')
    args = parser.parse_args()
    out = Path(args.out) if args.out else latest_out(args.sample)
    if out is None or not (out / 'run.log').exists():
        raise SystemExit(f'No run.log in {out}; create runs with tools/eval/run.ps1.')
    index = source_index(args.sample)
    target = {}
    for path in sorted((TARGETS / args.sample).rglob('*.pdf')):
        with pymupdf.open(path) as doc:
            for page in range(doc.page_count):
                target[index.get(page_hash(doc, page))] = (part_name(path), page)
    info = {}
    for line in read_log(out / 'run.log').splitlines():
        found = re.search(rf'Page (\d+): ({FIELDS}) = (.*)$', line.rstrip())
        if found:
            info.setdefault(int(found.group(1)), {})[found.group(2)] = found.group(3)
    for page in range(len(index)):
        part, number = target.get(page, ('?', 0))
        d = info.get(page, {})
        rotation = re.sub(r'\D.*', '', d.get('orientation correction', ''))
        print(f"{page:3} T={part:<28}#{number} | rot={rotation:<3} start={d.get('is_part_start', '')[:5]:<5} "
              f"pn={d.get('page_number', '')[:9]:<9} inst={d.get('instrument_raw', '')[:70]}")


if __name__ == '__main__':
    main()
