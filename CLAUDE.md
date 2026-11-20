# MusicScanSort

Rust program that sorts scanned music-sheet PDFs into one PDF per instrument part and lets the
user correct the result in a window.

- **Behaviour:** [docs/spec.md](docs/spec.md) is the reference. When detection or grouping
  behaviour changes, update the spec in the same change. The README only has a short overview
  and needs updating only when the overview or the CLI changes.
- **Code:** `src/analysis.rs` handles per-page detection (orientation, skew, first page, OCR
  label, voice). `src/pieces.rs` finds the pieces in a folder and which page belongs to which.
  `src/grouping.rs` groups the pages into parts, fills voices and ranks duplicate scans.
  `src/pipeline.rs` runs all stages. `src/main.rs` contains the CLI. For the window, `src/ui.rs`
  is the interface, `src/session.rs` holds the edits and what a save does, `src/worker.rs` is the
  thread for all PDF work. `src/record.rs` writes the run record and, on each save, the
  corrections made by hand to `Corrections.md`.
- **Marks in the page view (spec §14.4):** the analysis keeps the OCR lines it used in
  `PageAnalysis::marks`. `analysis::on_scan` maps a line back to the unturned scan page, through
  part crop, deskew, turn and content crop in reverse. The record stores these places, and
  `ui::paint_marks` draws them with the page's total turn. A change to that chain of crops and
  turns must keep the test `a_line_of_the_upright_page_is_found_again_on_the_scan` passing.
- **Split pages (spec §14.5):** a part is a `PageRef` with `crop`, the part of the source page
  before `rotation`, used to render and save it (`output::crop_page` shrinks media and crop box).
  Its `ScanRef::area` is the same part on the scan page before any turn, used for the marks and
  kept in the record. `Crop::of_shown` builds both from a part of the shown page.
- **Pages without page numbers (spec §5.8, §6.2):** a number at the first staff is a bar number,
  never a page number. `analysis::last_bar_number` reads the bar numbers at the left of the
  staves (`PageAnalysis::last_bar`), the second read at `--label-dpi` fills the missing ones, and
  `grouping::order_by_bars` sorts continuation pages by them. `grouping::sort_out_repeated_scans`
  moves pages scanned once too often to `DUPLICATED` (spec §7.1 rule 8).

## Build and test

`cargo` only works inside the Visual Studio build environment. Use the wrapper from the project
root:

```powershell
tools\eval\build.cmd test --release
tools\eval\build.cmd build --release
```

## Checking a change against the samples

The scans are in `sample-files/<sample>`. The expected output is in
`sample-result-files/<sample>`. The workflow, the tools and the baseline scores are in
[tools/eval/README.md](tools/eval/README.md):

1. Run the samples with `.\tools\eval\run.ps1 [SAMPLE,...]`. All samples together take about 20 minutes,
   so run them in the background.
2. Score them with `python tools/eval/compare.py`.
3. Check suspicious differences with `pages.py SAMPLE --diff`.
4. Compare the scores with `tools/eval/baseline.md`. A change must not lower any sample's score.
5. Update the baseline when scores improve. It is git-ignored: never copy sample piece names
   into tracked files (code, tests, docs).

## Rules

- **Window:** the program opens a window by default, so always pass `--no-gui` in tests and scripts
  unless the window is what you test. PDFium can only be loaded once per process, so tests that
  need it share one test function.
- **Output folders:** the user wants each run in a numbered `out-N` folder, which is the program
  default. Don't create output folders with other names.
- **Known differences:** `baseline.md` lists differences that are not detection errors, because the
  target contains pages missing from the source. Don't try to fix them.
- **Duplicate scans:** the main file should be the best scan. Only about 29 of the 50 target picks
  can be predicted by any quality rule, so don't tune `scan_rank` to single target picks. Change it
  only for a clear, visible quality defect.
- **DPI:** detection is tuned for the default of 150 DPI. Write pixel sizes for 150 DPI and pass
  them through `scaled`, so that 150 DPI stays unchanged. Even so, at 300 DPI the OCR reads and
  some clef, orientation and first-page results change and all scores drop or stay level, so
  re-score all samples before raising the default.
- **Deleting:** never delete anything permanently, not even your own temporary files. Move them to
  the Recycle Bin, for example old `out-N` folders after a sample run. Never empty the Recycle Bin.
- **Git:** the project is a git repository (branch `main`, MIT license). Commit only when the
  user asks. The repository is public material: no sample names, scans or personal data.
- **Window text:** egui's default font has no "→" and other rarer symbols; a missing glyph makes
  the window tests panic. Use plain characters in window text.
- **Language:** the window and the content of `Corrections.md` are in German; code, docs and spec are in English.
- **Helper scripts:** put one-off debug scripts in the session scratchpad. Add a script to
  `tools/eval/` only if it is reusable.
