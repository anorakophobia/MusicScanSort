"""Checks rankings of duplicate scans against the main files chosen in sample-result-files.

    python tools/eval/duplicates.py [-v]

For every part that our newest out-N output holds more than once (main file plus "Copy N")
and whose target file is one of those scans, each ranking below picks a main scan from the
skew and ink darkness that the run log (out-N/run.log) records per page. Prints how many target
picks each ranking reproduces; -v also lists every pair as page:|skew|/ink darkness.
The program's rule (grouping.rs, scan_rank) is "worst bucket, first-page ink, later"; its
legible-label criterion is left out here because it only separates cut-off scans.
"""
import sys

from common import TARGETS, file_pages, latest_out, page_metrics, read_log, samples, source_index


def bucket(skew):
    return int(skew / 0.5)


def mean_ink(scan):
    return sum(page[1] for page in scan) / len(scan)


# A scan is a list of (|skew|, ink darkness, page) with its first page first; lower key wins.
RANKINGS = {
    'worst bucket, first-page ink, later (program)': lambda s: (bucket(max(p[0] for p in s)), s[0][1], -s[0][2]),
    'first page: bucket, ink, later': lambda s: (bucket(s[0][0]), s[0][1], -s[0][2]),
    'worst bucket, mean ink, later': lambda s: (bucket(max(p[0] for p in s)), mean_ink(s), -s[0][2]),
    'mean ink, later': lambda s: (mean_ink(s), -s[0][2]),
    'first-page ink, later': lambda s: (s[0][1], -s[0][2]),
    'earlier scan': lambda s: s[0][2],
    'later scan': lambda s: -s[0][2],
}


def pairs():
    rows = []
    for name in samples():
        out = latest_out(name)
        if out is None or not (out / 'run.log').exists():
            print(f'{name}: no out-N/run.log, skipped')
            continue
        index = source_index(name)
        target = file_pages(TARGETS / name, index)
        ours = file_pages(out, index)
        metrics = page_metrics(read_log(out / 'run.log'))
        parts = {}
        for part, pages in ours.items():
            parts.setdefault(part.split(' - Copy')[0], []).append(pages)
        for part, scans in parts.items():
            kept = target.get(part)
            if len(scans) < 2 or kept not in scans:
                continue
            measured = [[metrics.get(p, (0.0, 0.0)) + (p,) for p in scan if p != '?'] for scan in scans]
            rows.append((name, part, measured, scans.index(kept)))
    return rows


def main():
    rows = pairs()
    for label, key in RANKINGS.items():
        hits = sum(min(range(len(scans)), key=lambda i: key(scans[i])) == kept
                   for _, _, scans, kept in rows)
        print(f'{hits:3}/{len(rows)}  {label}')
    if '-v' in sys.argv:
        for name, part, scans, kept in rows:
            listed = ' | '.join(' '.join(f'{p[2]}:{p[0]:.2f}/{p[1]:.3f}' for p in scan) for scan in scans)
            print(f'{name[:18]:<18} {part:<28} target=scan {kept}  {listed}')


if __name__ == '__main__':
    main()
