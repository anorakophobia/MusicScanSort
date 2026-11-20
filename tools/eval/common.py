"""Shared helpers for the evaluation tools: sample paths, page fingerprints and run logs."""
import hashlib
import re
from pathlib import Path

import pymupdf

ROOT = Path(__file__).resolve().parents[2]
SOURCES = ROOT / 'sample-files'
TARGETS = ROOT / 'sample-result-files'
CACHE = Path(__file__).with_name('.cache')


def samples():
    return sorted(path.name for path in SOURCES.iterdir() if path.is_dir())


def source_pdfs(name):
    return sorted((SOURCES / name).glob('*.pdf'))


def latest_out(name):
    """The newest numbered output folder `out-N` of a sample (None if there is none)."""
    runs = [path for path in (SOURCES / name).glob('out-*')
            if path.is_dir() and path.name[4:].isdigit()]
    return max(runs, key=lambda path: int(path.name[4:]), default=None)


def page_hash(doc, page):
    """Fingerprint of the scanned image on a page; output PDFs copy the source images unchanged."""
    images = doc[page].get_images(full=True)
    return hashlib.md5(doc.extract_image(images[0][0])['image']).hexdigest() if images else None


def source_index(name):
    """Maps the fingerprint of every source page to its page number in the program's order."""
    index, order = {}, 0
    for path in source_pdfs(name):
        with pymupdf.open(path) as doc:
            for page in range(doc.page_count):
                index[page_hash(doc, page)] = order
                order += 1
    return index


def part_name(path):
    """`<Title> - <Part>.pdf` → `<Part>`."""
    return path.stem.split(' - ', 1)[-1]


def file_pages(folder, index):
    """{part: (source page, ...)} for every PDF in a target or output folder; '?' = not in source."""
    result = {}
    for path in sorted(Path(folder).rglob('*.pdf')):
        with pymupdf.open(path) as doc:
            result[part_name(path)] = tuple(index.get(page_hash(doc, page), '?')
                                            for page in range(doc.page_count))
    return result


def read_log(path):
    """Run log as text with wrapped lines joined (PowerShell wraps long lines)."""
    raw = Path(path).read_bytes()
    text = raw.decode('utf-16') if raw[:2] in (b'\xff\xfe', b'\xfe\xff') else raw.decode('utf-8', 'replace')
    return re.sub(r'\r?\n(?![A-Z]{4,8}\s)', '', text)


def page_metrics(log_text):
    """{page: (|skew| in degrees, ink darkness)} from the DEBUG lines of a run log."""
    return {int(page): (abs(float(skew)), float(ink)) for page, skew, ink in re.findall(
        r'Page (\d+): skew correction = ([-\d.]+)\S*, ink darkness\s*=\s*([\d.]+)', log_text)}
