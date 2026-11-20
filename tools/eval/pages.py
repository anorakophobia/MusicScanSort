"""Lists which source pages every target and output file contains, by exact image fingerprint.

    python tools/eval/pages.py SAMPLE [--out DIR] [--diff]

Page numbers are the program's page order (the same numbers as "Page N" in the run log);
'?' marks a page that is not in the source folder. Without --out the newest out-N folder is
used. --diff only prints the target files our output does not reproduce under the same name.
Exact but blind to re-scans: use compare.py for the score.
"""
import argparse
from pathlib import Path

from common import TARGETS, file_pages, latest_out, source_index


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('sample')
    parser.add_argument('--out', help='output folder (default: newest out-N)')
    parser.add_argument('--diff', action='store_true', help='only list differing target files')
    args = parser.parse_args()
    index = source_index(args.sample)
    target = file_pages(TARGETS / args.sample, index)
    ours_dir = Path(args.out) if args.out else latest_out(args.sample)
    ours = file_pages(ours_dir, index) if ours_dir else {}
    if args.diff:
        for part, pages in sorted(target.items()):
            if ours.get(part) != pages:
                print(f'{part:<40} target {list(pages)}  ours {list(ours.get(part, ()))}')
        return
    print('TARGET')
    for part, pages in target.items():
        print(f'  {part:<40} {list(pages)}')
    if ours:
        print(f'OURS ({ours_dir})')
        for part, pages in ours.items():
            print(f'  {part:<40} {list(pages)}')


if __name__ == '__main__':
    main()
