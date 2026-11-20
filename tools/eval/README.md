# Evaluation tools

These tools run MusicScanSort on the samples in `sample-files/` and compare the output with
the expected results in `sample-result-files/`. Use them before and after a detection change
to see whether the change helps or breaks something else.

## Setup

- **Windows build:** Rust toolchain plus the Visual Studio C++ build tools. `build.cmd` finds
  them through `vswhere`.
- **Python 3.11 or later**, with:

  ```
  pip install pymupdf numpy pillow opencv-python
  ```

Run every command from the project root.

## Workflow

```powershell
tools\eval\build.cmd test --release               # unit tests
tools\eval\build.cmd build --release              # release build used by run.ps1
.\tools\eval\run.ps1                              # all samples (~20 min) or: .\tools\eval\run.ps1 'Piece A'
python tools\eval\compare.py                      # score the newest out-N of every sample
```

- **`run.ps1`** writes each run to the next `out-N` folder of the sample, as the program does
  without `--out`.
  - Each output folder also gets `run.log`, the verbose log, and `summary.txt`.
  - The script prints a table of run times.
  - Older `out-N` folders stay in place for comparison. Move them to the Recycle Bin when you no longer need them.

## Tools

| Tool | Purpose |
|---|---|
| `build.cmd ARGS` | Runs `cargo ARGS` in the Visual Studio build environment. |
| `run.ps1 [SAMPLE, ...]` | Runs samples into their next `out-N` with `--dpi 150 --workers 4 --verbose` and reports run times. |
| `compare.py [SAMPLE ...] [--out DIR] [-v]` | Scores output against the target by page content and prints `DIFF` / `MISSING` / `EXTRA` lines and a `SUMMARY` line. |
| `pages.py SAMPLE [--out DIR] [--diff]` | Lists exactly which source pages each target and output file contains, matched by image fingerprint. |
| `page_table.py SAMPLE [--out DIR]` | Prints one line per source page: its target file, then the detected rotation, first-page flag, page number and label from `run.log`. |
| `duplicates.py [-v]` | Checks rankings of duplicate scans against the main files the target chose. |
| `contact_sheet.py FOLDER OUT.png` | Saves thumbnails of a folder's first pages, to see quickly what a batch of scans contains. |

Page numbers in all tools are the program's page order, the same numbers as `Page N` in the
log.

### Reading the scores

The `SUMMARY` line of `compare.py` has two scores:

- **`exact`:** target files whose pages and rotations we reproduce under the same file name.
- **`found_incl_copies`:** also counts a target file that one of our `Copy N` files reproduces,
  i.e. the right pages, but we picked another scan as the main file.

### Choosing between `compare.py` and `pages.py`

- **`compare.py` matches by content.** It also matches pages in the target that were scanned
  again and are not in the source.
  - It can mix up near-identical parts, such as Klarinette 2 and 3, or a cut-off scan and a
    full re-scan.
  - If a `DIFF` looks wrong, check it with `pages.py`.
- **`pages.py` is exact.** It only recognises pages that are in the source folder. A page that
  isn't shows as `?`.

### Caches

`compare.py` caches source thumbnails in `tools/eval/.cache`, keyed by the number of source
files. The cache is rebuilt automatically when scans are added. Move the folder to the Recycle
Bin if source files are replaced.

## Learning from corrections by hand

A result folder corrected in the window holds `Corrections.md` (see spec §14.9). To learn from it:

1. Read its sections to find which stage went wrong: moved pages and renames point to labels,
   voices and grouping; turned pages point to orientation; added and removed pages point to blank
   detection. `musicscansort.json` next to it has all OCR lines of each page.
2. Turn the folder into a sample: copy the scans to `sample-files/<sample>` and the PDFs of the
   corrected folder to `sample-result-files/<sample>`. From then on the workflow above checks every
   change against it as well.
3. Leave out corrections that only choose another `Copy N` as main file; see the rule on duplicate
   scans in `CLAUDE.md`.

## Baseline

The scores of the last accepted run, and the per-sample differences that are not detection
errors, are in `baseline.md` next to this file. It is git-ignored because it names the sample
pieces, so it only exists where the samples are.

Two kinds of difference remain that are not detection errors:

- **Pages missing from `sample-files`:** the target contains pages that are not in the source.
  `compare.py` may still report a near match for them; `pages.py` shows that their pages are not
  in the source.
- **Choice of duplicate scan:** a part was scanned more than once, and the target uses a different
  scan as its main file. `duplicates.py` shows that no measurable quality rule reproduces more
  than about 29 of the 50 target choices. In most pairs both scans are equally good. Don't tune
  the ranking to single target choices.
