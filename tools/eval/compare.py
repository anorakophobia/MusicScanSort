"""Scores output folders against the expected results in sample-result-files.

    python tools/eval/compare.py [SAMPLE ...] [--out DIR] [-v]

Without SAMPLE all samples are scored; without --out the newest out-N folder of each sample.

Pages are matched by content, so the score also works when a target file holds a re-saved
or re-scanned page: every page is rendered as a small greyscale thumbnail and matched to a
(source page, rotation) by correlation; weak matches fall back to ORB features with a RANSAC
similarity check. Prints DIFF / MISSING / EXTRA lines and one SUMMARY line per sample:
exact = target files whose pages and rotations we reproduce under the same name,
found_incl_copies = target files reproduced by the main file or one of its "Copy N" files.
For an exact check by image fingerprint use pages.py.
"""
import argparse
import pickle
from pathlib import Path

import numpy as np
import pymupdf

from common import CACHE, TARGETS, latest_out, part_name, samples, source_pdfs

SIZE = 48


def thumb(page):
    pix = page.get_pixmap(dpi=12, colorspace=pymupdf.csGRAY)
    return np.frombuffer(pix.samples, np.uint8).reshape(pix.height, pix.width).astype(np.float32)


def norm(array):
    from PIL import Image
    image = Image.fromarray(array.astype(np.uint8)).resize((SIZE, int(SIZE * 1.41)))
    vector = np.asarray(image, np.float32).ravel()
    vector = vector - vector.mean()
    length = np.linalg.norm(vector)
    return vector / length if length else vector


def cached(name, kind, build):
    """Source caches are keyed by the number of source files, so added scans rebuild them."""
    CACHE.mkdir(exist_ok=True)
    path = CACHE / f"{name.replace(' ', '_')}-{len(source_pdfs(name))}.{kind}.pkl"
    if path.exists():
        return pickle.loads(path.read_bytes())
    value = build()
    path.write_bytes(pickle.dumps(value))
    return value


def thumbnails(name):
    def build():
        vectors = []  # (source page, rotation, vector)
        for index, path in enumerate(source_pdfs(name)):
            with pymupdf.open(path) as doc:
                base = thumb(doc[0])
                for rotation in (0, 90, 180, 270):
                    # rotation r clockwise = what /Rotate r would show
                    vectors.append((index, rotation, norm(np.rot90(base, -rotation // 90))))
        return vectors
    return cached(name, 'thumbs', build)


ORB = None


def orb_render(page, rotation=0):
    pix = page.get_pixmap(dpi=40, colorspace=pymupdf.csGRAY)
    array = np.frombuffer(pix.samples, np.uint8).reshape(pix.height, pix.width)
    return np.ascontiguousarray(np.rot90(array, -rotation // 90))


def orb_features(image):
    import cv2
    global ORB
    ORB = ORB or cv2.ORB_create(1500)
    return ORB.detectAndCompute(image, None)


def orb_source(name):
    def build():
        features = []
        for index, path in enumerate(source_pdfs(name)):
            with pymupdf.open(path) as doc:
                for rotation in (0, 90, 180, 270):
                    points, descriptors = orb_features(orb_render(doc[0], rotation))
                    features.append((index, rotation, np.float32([p.pt for p in points]), descriptors))
        return features
    return cached(name, 'orb', build)


def orb_match(name, page):
    """Matches a re-scanned page by counting RANSAC inliers of a similarity transform."""
    import cv2
    points, descriptors = orb_features(orb_render(page))
    if descriptors is None:
        return ('?', 0, 0.0)
    coordinates = np.float32([p.pt for p in points])
    matcher = cv2.BFMatcher(cv2.NORM_HAMMING, crossCheck=True)
    best = ('?', 0, 0)
    for index, rotation, source_points, source_descriptors in orb_source(name):
        if source_descriptors is None:
            continue
        matches = matcher.match(descriptors, source_descriptors)
        if len(matches) < 12:
            continue
        a = coordinates[[m.queryIdx for m in matches]]
        b = source_points[[m.trainIdx for m in matches]]
        transform, inliers = cv2.estimateAffinePartial2D(a, b, method=cv2.RANSAC, ransacReprojThreshold=4)
        if transform is None:
            continue
        angle = abs(np.degrees(np.arctan2(transform[1, 0], transform[0, 0])))
        count = int(inliers.sum()) if angle < 10 else 0
        if count > best[2]:
            best = (index, rotation, count)
    return best if best[2] >= 40 else ('?', best[1], best[2])


def match(vectors, page, name=None):
    vector = norm(thumb(page))
    best = max(vectors, key=lambda entry: float(entry[2] @ vector))
    score = float(best[2] @ vector)
    if score < 0.85 and name:
        index, rotation, _ = orb_match(name, page)
        return (index, rotation, 1.0 if index != '?' else 0.0)
    return (best[0] if score > 0.6 else '?', best[1], round(score, 2))


def load(vectors, folder, name=None):
    """{part: [(source page, rotation, confidence), ...]}; name enables the ORB fallback."""
    result = {}
    for path in sorted(Path(folder).rglob('*.pdf')):
        with pymupdf.open(path) as doc:
            result[part_name(path)] = [match(vectors, doc[p], name) for p in range(doc.page_count)]
    return result


def score(name, ours, verbose):
    vectors = thumbnails(name)
    target = load(vectors, TARGETS / name, name)
    output = load(vectors, ours)
    key = lambda pages: [(p[0], p[1]) for p in pages]
    exact = 0
    for part in sorted(set(target) | set(output)):
        a, b = target.get(part), output.get(part)
        if a is None:
            print(f'EXTRA   {part}: {key(b)}')
        elif b is None:
            print(f'MISSING {part}: {key(a)}')
        elif key(a) == key(b):
            exact += 1
            if verbose:
                print(f'OK      {part}: {key(a)}')
        else:
            print(f'DIFF    {part}: target={key(a)} ours={key(b)}')
    extras = sum(1 for part in output if part not in target)
    found = sum(1 for part, a in target.items() if any(
        key(b) == key(a) for other, b in output.items() if other.split(' - Copy')[0] == part))
    print(f'SUMMARY {name}: exact={exact}/{len(target)} found_incl_copies={found}/{len(target)} '
          f'extra_files={extras}  ({ours})')


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('samples', nargs='*', help='sample folder names (default: all)')
    parser.add_argument('--out', help='output folder to score (default: newest out-N)')
    parser.add_argument('-v', '--verbose', action='store_true', help='also list matching files')
    args = parser.parse_args()
    for name in args.samples or samples():
        ours = Path(args.out) if args.out else latest_out(name)
        if ours is None:
            print(f'SUMMARY {name}: no out-N folder')
            continue
        score(name, ours, args.verbose)


if __name__ == '__main__':
    main()
