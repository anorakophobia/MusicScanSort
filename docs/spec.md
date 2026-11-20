# MusicScanSort — Implementation Specification

## 1. Overview

**MusicScanSort** is a program that takes scanned music sheet PDFs (one or more files, each containing one or more pages), identifies the instrument each page belongs to via OCR / image analysis, and produces per-instrument output PDFs with correctly oriented pages in the right order. It runs in a window where the result can be corrected (§14), or on the command line with `--no-gui`.

### 1.1 High-Level Pipeline

```
┌─────────────┐    ┌──────────────┐    ┌────────────────┐    ┌──────────────┐    ┌────────────┐
│  CLI Parse   │───▶│  PDF Ingest  │───▶│  Page Analysis │───▶│  Grouping &  │───▶│  PDF Write │
│  & Config    │    │  & Flatten   │    │  (per page)    │    │  Ordering    │    │  (output)  │
└─────────────┘    └──────────────┘    └────────────────┘    └──────────────┘    └────────────┘
```

Each stage is described in detail below.

---

## 2. CLI Interface

### 2.1 Invocation

```
musicscansort [OPTIONS]
```

### 2.2 Parameters

| Flag / Argument | Type   | Required | Default                        | Description                                                                 |
|-----------------|--------|----------|--------------------------------|-----------------------------------------------------------------------------|
| `--dir`         | path   | No       | Current working directory      | Folder to show in the window (§14.1) and to scan for input PDF files.      |
| `--title`       | string | No       | Basename of the working dir    | Title of the music piece; used in output file names. Pages that show another title are named after that piece (§5.10). |
| `--single-piece` | flag  | No       | off                            | Treat all pages as one piece, even when they show other titles (§5.10).     |
| `--out`         | path   | No       | `<working_dir>/out-N`          | Directory where per-instrument PDFs are written. Created if it doesn't exist. Without `--out`, each run writes to a new folder `out-N`, with N one above the highest existing `out-N` (first run: `out-1`). |
| `--dpi`         | int    | No       | `150`                          | Rasterisation DPI for analysis only; output pages are copied from the source PDFs. |
| `--label-dpi`   | int    | No       | `300`                          | Rasterisation DPI for the second read of unclear labels (§5.4). A value not above `--dpi` turns the second read off. |
| `--blank-threshold` | float | No   | `0.005`                        | Dark-pixel ratio below which a page is blank (§5.1).                        |
| `--workers`     | int    | No       | `4`                            | Number of pages analysed in parallel.                                       |
| `--dry-run`     | flag   | No       | off                            | Analyse and print the grouping plan without writing PDFs. Shows no window.  |
| `--no-gui`      | flag   | No       | off                            | Sort on the command line, without the window (§14).                         |
| `--run`         | flag   | No       | off                            | Start sorting at once in the window, instead of only showing the folder (§14.1). Not together with `--no-gui` or `--dry-run`. |
| `-v`, `--verbose` | flag | No       | off                            | Per-page `DEBUG` logging (§8.1).                                            |

### 2.3 Validation Rules

1. `--dir` must exist and be readable; abort with exit code **1** and a descriptive message if not.
2. `--out` must not be the same directory as `--dir` (to avoid overwriting source files).
3. On the command line (`--no-gui`, `--dry-run`): if `--dir` contains zero PDF files (recursively), abort with exit code **1** and message `"No PDF files found in <dir>"`.

### 2.4 Exit Codes

| Code | Meaning                              |
|------|--------------------------------------|
| 0    | Success                              |
| 1    | Configuration / input error          |
| 2    | Runtime / processing error           |

### 2.5 Starting by Double-Click

A double-click passes no parameters and starts the program in the folder of the executable, so the window shows that folder without analysing anything (§14.1).

The console window that Windows opens for a double-click closes as soon as the window is up. If the window cannot start, or with `--no-gui`, the console stays open after the summary or an error until Enter is pressed. This wait and the closing only happen when the program is alone in its console and neither input nor output is redirected, so runs from a terminal or a script end immediately and keep their log.

---

## 3. Data Model

### 3.1 Core Types

```
PageImage
├── source_file: string          # Original PDF file path
├── source_page_index: int       # 0-based page index within source PDF
├── global_order: int            # Sequential index across ALL pages from ALL files
├── image: RawImage              # Rasterised page bitmap (at `--dpi`)
├── orientation: Rotation        # Detected orientation correction (0°, 90°, 180°, 270°)
├── corrected_image: RawImage    # Image after orientation correction
└── analysis: PageAnalysis       # Result of content analysis

PageAnalysis
├── is_blank: bool               # True if page is (nearly) blank
├── music_title: string | null   # Detected title of the piece
├── instrument_raw: string | null        # Raw detected instrument text
├── instrument_normalized: string | null # Mapped/normalised instrument name
├── voice_number: int | null     # Voice suffix (e.g. "1" in "Trompete 1")
├── clef: "treble" | "bass" | null       # Detected clef type
├── page_number: int | null      # Detected page number (if printed on page)
├── is_part_start: bool          # Probable first page of an instrument part
├── is_final_page: bool          # True if double barline detected at bottom-right
├── has_title_evidence: bool     # Title read, other header text of its piece, printed page number 1, no staff (cover), or no centred part label above the music
├── orientation_from_text: bool  # Chosen direction is backed by a readable title or instrument label
├── skew_degrees: float          # Measured scan skew before the analysis-only deskew (§5.2)
├── ink_darkness: float          # Share of fully black pixels among ink pixels (§7.1 rule 7)
├── staff_top: int | null        # Top of the first staff in pixels at --dpi
├── is_score_page: bool          # First page listing several instruments; its label is ignored (§5.9)
├── label_line: OcrLine | null   # OCR line of a label with an unclear key, for the second read (§5.4)
├── content_crop: rect | null    # Part of the scan holding the music, when it is a small original (§5.2)
├── text_lines: list[OcrLine]    # All OCR lines of the page
├── staff_gap: float | null      # Distance between the lines of the first staff in pixels at --dpi
├── piece: string | null         # Title of the piece when it is not the expected title (§5.10)
├── piece_evidence: bool         # Header text other than the title ties the page to its piece (§5.10)
├── part_region: (top, height) | null  # Part of the upright scan when the scan holds two parts (§5.10)
└── duplicate_copy: int          # Set by grouping: 0 for the first scan of a part, n for its n-th re-scan

InstrumentGroup
├── key: (piece, instrument_name, voice_number, copy)  # Piece (§5.10), normalised name (incl. clef suffix such as " C"), voice, 0 = main file / n = Copy n
├── pages: list[PageImage]       # Ordered list of pages belonging to this group
└── output_filename: string      # Computed output file name
```

---

## 4. Stage 1 — PDF Ingestion & Page Flattening

### 4.1 File Discovery

1. Recursively scan `--dir` for files matching `*.pdf` (case-insensitive).
2. Sort discovered files **lexicographically** by full path to establish a deterministic global ordering (scanned files from a scanner typically have sequential numeric names).

### 4.2 Page Extraction

For each PDF (in the sorted order):

1. Open the PDF.
2. For each page (0-indexed within the file):
   - Rasterise to an image at `--dpi` (default **150 DPI**, which the detection is tuned for and the sample sets are evaluated at). Pixel sizes in the detection are written for 150 DPI and scaled by `--dpi / 150`, so 150 DPI uses them as written. Brightness thresholds and the OCR input sizes are not scaled.
   - Assign a `global_order` value starting from 0, incrementing by 1 for every page across all files.
   - Create a `PageImage` record.

### 4.3 Output of This Stage

An ordered `list[PageImage]` representing every page in global scan order.

---

## 5. Stage 2 — Per-Page Analysis

Each `PageImage` is independently analysed. The following sub-steps run sequentially per page.

### 5.1 Blank Page Detection

**Goal:** Detect and discard (nearly) empty pages.

**Algorithm:**
1. Convert the original rasterised image to greyscale.
2. Apply adaptive thresholding: a pixel is dark when it is more than 10 grey levels darker than the mean of its 15 × 15 neighbourhood.
3. Calculate the ratio of dark pixels to total pixels.
4. If ratio < **0.5 %** (configurable threshold), mark `is_blank = true`.
5. Also mark a page blank when less than **0.15 %** of its pixels are darker than brightness 140. Show-through from the back and scan edges have contrast but no printed ink: such pages reach at most 0.09 %, and the shortest printed page in the sample sets has 0.5 %.

**Behaviour:** Run this inexpensive check before orientation detection. Blank pages are excluded from all downstream processing without running orientation analysis or OCR. Log a message: `"Page <global_order> (from <source_file>) is blank — skipping."`.

### 5.2 Music Area, Orientation Detection & Correction

**Music area:** A small original may lie on a larger scan (A5 on A4, in the middle or in a corner). All position rules in the analysis assume the music fills the page, so the analysis only sees the part that holds the music, stored as `content_crop`; the output keeps the whole page.

- A row or column has ink when more than 0.7% of its pixels are darker than brightness 140.
- A side of the page is cut to the range with ink, plus 2.5% of the page as margin, only when that range covers at least 35% and less than 60% of that side. Below 35% the page is just short (the last lines of a part). From 60% the page is treated as a full page: the sample sets, which need no crop, never go below 62%.
- The crop only applies when the side along the staves is cut, because the staves of a small original are short. Staves over the full width with an empty part below them are the last lines of a part on a normal page.

**Goal:** Determine the correct upright orientation of the page before any OCR or grouping logic runs.

Each non-blank page is evaluated in all four orientations: **0°, 90°, 180°, and 270°**. The correct orientation is the one that maximizes the combined detection score across the following signals:

- **Staff-line score:** the dominant staff lines should be approximately horizontal, not vertical or diagonal
- **OCR readability score:** OCR text should be upright and readable with higher confidence when the page is correctly oriented
- **Layout plausibility score:** title and instrument text should appear in the expected top portions of the page rather than upside-down or sideways

**Required algorithm:**

1. Score staff-line horizontality for all four candidate orientations using image processing only: count dark horizontal runs of at least 1/4 of the page width. If no candidate reaches 0.3, the page is a dense score whose staff lines are interrupted by many barlines; score again with runs of at least 1/12 of the page width.
2. If staff lines clearly favour one axis (0°/180° or 90°/270°: best score ≥ 0.05, ahead by more than 0.02 and by at least 20%), run OCR only on the two candidates on that axis; otherwise run OCR on all four candidates.
3. For each OCR candidate, recognize text only in the top 35% header crop, downscaled if needed so its longest side is at most 1200 pixels.
4. Compute the combined score (table below) and select the orientation with the best score.
5. Apply the selected rotation and store the result as `corrected_image`; record the rotation in `orientation`, and set `orientation_from_text` when the choice was clear and the expected title or a mapped instrument label was read in a candidate.
6. Estimate the remaining small skew of the corrected page (−2° to +2°: the angle whose row projection of ink pixels is sharpest, searched in 0.25° steps and refined in 0.05° steps; 0 unless it beats the unskewed projection by 2%). Store it as `skew_degrees` and, from 0.1°, straighten the analysis image by shifting columns vertically. The output keeps the original page content.
7. Use the corrected image for all subsequent OCR, title detection, instrument detection, and grouping.

The combined score is a weighted sum of component scores, each in the range 0–1. When the expected title is read in at least one candidate (similarity ≥ 0.55), the title weights apply:

| Component | Weight | With title | Scoring rule |
|-----------|--------|------------|--------------|
| Horizontal staff lines | 0.45 | 0.35 | Total length of the long horizontal dark runs from step 1, capped at 1. |
| OCR readability | 0.40 | 0.25 | Share of alphanumeric characters and line length (75%) combined with the number of recognized text lines, capped at four (25%). |
| Layout plausibility | 0.15 | 0.10 | Confidence-weighted closeness of the recognized text to the top of the header crop. |
| Title similarity | — | 0.30 | Best similarity of a header line, or of all header lines joined, to the expected title. |
| Instrument label | 0.30 | 0.20 | 1 when a header line starts with a mapped instrument name. |

Staff-line scoring uses the rasterised candidate image. Orientation OCR must use the header crop rather than the full page to limit processing time. Log elapsed time for each orientation candidate and OCR call at DEBUG level; log OCR failures at WARNING level.

**Fallback rules:**

- If staff-line detection is weak, fall back to OCR readability and text-layout plausibility for all four candidates.
- If every candidate has a zero score, or the top two candidate scores differ by no more than 0.02, the result is ambiguous. Continuation pages typically end up here, because their label is only in the small footer line.
  - If the staff axis is clear, choose between its two directions. First read the bottom 20% of the page (centre 70% of the width, enlarged) in both directions; when one direction shows the expected title with similarity ≥ 0.5 and at least 0.2 above the other, take it. Otherwise compare the ink at the start and the end of the staff systems (first and last 10% of their width): staves start with clef and key signature but end with a plain barline, so take the direction with more ink at the start. The width of the systems runs from the first to the last column with ink in more than 5% of the staff rows; a column with ink in 85% or more of them is a scan edge or paper border and is ignored.
  - Otherwise keep the original orientation and log a warning rather than discarding the page.
- When OCR is unavailable or returns no text, its readability and layout scores are zero; staff-line evidence can still select an orientation when it distinguishes one candidate.
- After all pages are analysed, up to three pages in a row whose direction is not backed by a readable title or instrument label are turned by 180° when the non-blank pages on both sides agree on the opposite direction (a scanner feeds a stack in one direction). Their text and staff position are then read again the right way up.

**Acceptance criterion:** A correctly oriented page is one where the dominant staff lines are horizontal and the OCR text is upright and readable. This is a mandatory preprocessing step for every non-blank page.

> **Implementation note:** Orientation detection must be done before title/instrument analysis. The page is considered correctly oriented only when the score is highest for the true upright state, not just when OCR happens to return any text.

### 5.3 Music Title Detection

**Goal:** Extract the piece title from the page, if present.

**Location heuristic:**
- The expected title is the title supplied with `--title` (or the resolved working-directory name when `--title` is omitted).
- Scan the top 35% of the page across its full width for a matching title. The title may be off-centre or split across adjacent OCR regions.
- Nearby text above or below the title (dedications, composer/arranger credits, and other annotations) must not be treated as the title unless it matches the expected piece title.

**Rules:**
1. Extract candidate text regions from the title band and compare normalized OCR text to the expected title, ignoring case, punctuation, spacing, and German diacritic differences.
2. Combine adjacent text regions on the same line when comparing, to support titles split by OCR. Also compare word windows inside a longer line, because OCR may merge the title with the part label and credits (`B♭ TRUMPET 4 PIECE TITLE arr. …`).
3. Select the candidate with the strongest title similarity; unrelated nearby text must not override a closer title match.
4. If no candidate reaches a similarity score of 0.55, store `music_title = null`.
5. Store the supplied expected title as `music_title` when a qualifying match is found.

### 5.4 Instrument Name Detection

**Goal:** Identify which instrument this page belongs to.

**Location heuristic:**
- Search all OCR text detected on the page for phrases matching the designated instrument patterns; do not reject a candidate solely because it falls outside the upper-left area.
- Use proximity to the first staff and left-side placement as ranking signals between multiple mapped candidates, not as hard crop boundaries.
- Ignore text inside parentheses / brackets, title matches, page numbers, and non-letter noise.

**Extraction rules:**
1. Run OCR over the full page (downscaled when needed for performance) and retain each text region's position and confidence.
2. If no instrument is found or its voice number is missing, retry OCR on enlarged upper-left and bottom-center label crops. Continuation-page instrument labels may appear in small text at the bottom center of the page. When a fallback read only adds the voice number (`HORN 4`), keep the key of the first read (`Eb HORN`). When the first read of a first page names no key that would change the part (`Klarinette`) and the upper-left crop names one for the same instrument (`Klarinette in Eb`), the crop's label is used, with the crop's voice, or else the voice of the first read.
   - If the label of a first page names no key but a key would change the output part (`HORN` → Horn F or Horn Es, `TUBA` → Tuba C, B or Es), re-read a close-up of the label (its part of the OCR line, with room on the left for the key sign, enlarged to about 64 px text height). Use the close-up label only if it yields a keyed label. If it reads the same unkeyed label but with a voice number (`Trombone` → `Trombone 1`) and no voice was found yet, take that voice number.
3. Remove parenthesized/bracketed text and an optional part number from each candidate. Accept a number before the instrument name with a dot (for example, `1. Trompete in B`) as well as after the name (for example, `Trompete 1`).
4. Combine adjacent OCR regions on the same line into contiguous text candidates, to support instrument labels split across multiple OCR words. A candidate that runs into the title is dropped (`Klarinette in Bl?tenwalzer`: an umlaut lost by OCR would turn the first letters of the title into a key). A candidate word after the first counts as title when it has at least 4 letters and is at least 70% similar to a title word of at least 4 letters.
5. Confirm a five-line staff is present before accepting an instrument match; this prevents instrument names in a contents page from being mistaken for a score-page label.
6. Compare every candidate against the instrument mapping using the detected clef. If a candidate is rejected by the detected clef, accept it only when mapping the same text under both treble and bass yields one unambiguous output name. Preserve clef-based distinctions when the mappings differ.
7. If several mapped candidates remain, prefer candidates that include a part number or explicit intonation as part of the match, then stronger OCR confidence, proximity to the upper-left first-staff area, and shorter candidate text. Position must not prevent a match elsewhere on the page.
8. Return the matched instrument text and its part number as `voice_number` (e.g. `"Trompete 1"`, `"1. Trompete in B"` or `"Trompete I in B"` → voice=`1`). Roman voices may be misread as `Il` or `11`. English ordinals give the voice too: `1st`, `2nd`, `3rd`, `4th`, also split or glued by OCR (`1 st`, `2nd.C Flute`) and `Ist`/`lst` for `1st` at the start of a label; text before an ordinal is a section heading on the same line (`4. The Last Dance 3rd Bb Trombone` → voice 3). A number above 6 is not a voice but a catalogue or handwritten number (`Nr. 506`). A `Direktion` label outranks an instrument named next to it (`Direktion` above `1. Flügelhorn B`). If no designated pattern is found, set both to `null` so grouping can apply its continuation-page rule; do not promote arbitrary score directions or descriptive text to instrument labels.
9. **Second read:** after all pages are analysed and before the orientation is harmonised (§5.2), pages with an open label are rendered again at `--label-dpi`, with the same rotation and skew correction as the first read. The whole-page OCR reduces its input to 1800 px anyway, so the second read only uses the label crops:
   - It applies to non-blank pages with a staff that are not score pages (§5.9) and have no label, and to first pages whose label has an unclear key. A missing voice number alone is not read again: on the samples this was most of the extra run time and changed no output.
   - An unclear key is read from the close-up of step 2. It is used only for the same label with a key (`HORN` → `Eb HORN`). The close-up may also add a voice number the first read did not find.
   - A missing label is read from the upper-left and bottom-center crops, in the margins only. First pages prefer the upper-left crop, continuation pages the bottom-center crop. An explicit clef marker (§5.6) in these crops counts.
   - The second read only fills gaps: a label or voice number found by the first read is never replaced by another one.
   - It also reads the bar numbers of continuation pages without them and without a page number (§6.2).
   - A crop listing four or more different instruments is ignored. A page that cannot be rendered or read again keeps its first read.

### 5.5 Instrument Name Normalisation

**Goal:** Map the raw detected text to a canonical instrument name using the mapping table.

**Mapping table** (case-insensitive matching, whitespace-normalised):

For B-flat and E-flat intonations, accept the written forms `Bb`/`Eb`, a lowercase `b` following the note letter, and the musical flat sign `♭` (U+266D), e.g. `Trompete in Bb`, `Trompete in B♭`, or `Trompete in B flat`. These forms are equivalent for matching. This flat-sign rule applies only to B and E intonations; C and F remain unchanged.

| Detected patterns (case-insensitive)                                          | Clef       | Output name        |
|-------------------------------------------------------------------------------|------------|--------------------|
| `Bariton B`, `B Bariton`, `Bariton in B`                                      | treble     | `Bariton B`        |
| `Bariton B`, `B Bariton`, `Bariton in B`                                      | bass       | `Bariton B C`      |
| `Bariton C`, `C Bariton`, `Bariton in C`                                      | bass       | `Bariton C`        |
| `Bariton` (no key)                                                            | treble / bass | `Bariton B` / `Bariton C` |
| `Bass`, `Baß` (no key)                                                        | treble / bass | `Bass B` / `Bass C` |
| `Bass in B`, `B Bass`                                                         | any        | `Bass B`           |
| `Bass in Es`, `Bass Es`                                                       | any        | `Bass Es`          |
| `Es Bass`                                                                     | treble     | `Bass Es`          |
| `Tenorhorn`, `Tenor Horn`, `B Tenorhorn`, `Bb Tenorhorn`, `Tenorhorn in B`    | treble     | `Tenorhorn`        |
| `Flöte`, `Flute`, `Floete`, `Flöte in C`                                      | treble     | `Flöte`            |
| `Flöte in Des`, `Flöte Des`, `Des Flöte`                                      | treble     | `Flöte Des`        |
| `Piccolo`                                                                     | treble     | `Flöte Piccolo`    |
| `Flügelhorn`, `Fluegelhorn`, `Bugle`                                          | treble     | `Flügelhorn`       |
| `Horn F`, `F Horn`, `Horn in F`, `Althorn in F`, `Althorn-Melodie in F`       | treble     | `Horn F`           |
| `Horn-Melodie`, `Horn-Melodie in F` (a part beside `Horn in F`)               | any        | `Horn F Melodie`   |
| `Horn Es`, `Es Horn`, `Eb Horn`, `Horn Eb`, `Horn in Eb`, `Althorn`, `Althorn-Melodie` | treble     | `Horn Es`          |
| `Klarinette`, `Clarinette`, `B Klarinette`, `Bb Klarinette`, `Klarinette in B`, `Clarinette` | treble     | `Klarinette`       |
| `Bassklarinette`, `B Bassklarinette`, `Bb Bassklarinette`                     | treble     | `Klarinette Bass`  |
| `Bassposaune`, `Bass Trombone`                                                | any        | `Posaune Bass`     |
| `Posaune`, `C Posaune`, `Trombone`, `C Trombone`, `Posaune in C`              | bass       | `Posaune`          |
| `Posaune B`, `B Posaune`, `B Trombone`, `Posaune in B`                        | treble     | `Posaune B`        |
| `Posaune B`, `B Posaune`, `B Trombone`, `Posaune in B`                        | bass       | `Posaune B C`      |
| `Altsaxophon`, `Es Altsaxophon`, `Eb Altsaxophon`, `Alt Saxophon`, `Altsaxophon in Eb` | treble     | `Saxophon Alt`     |
| `Tenorsaxophon`, `B Tenorsaxophon`, `Bb Tenorsaxophon`, `Tenor Saxophon`, `Tenorsaxophon in B` | treble     | `Saxophon Tenor`   |
| `Baritonsaxophon`, `E Baritonsaxophon`, `Eb Baritonsaxophon`                  | treble     | `Saxophon Bariton` |
| `Trompete`, `B Trompete`, `Bb Trompete`, `Trumpet`, `Trompete in B`           | treble     | `Trompete`         |
| `Solo-Trompete`, `Solo Trompete in B`, `Trompete Solo`, `Solo Trumpet`        | treble     | `Trompete Solo`    |
| `Tuba`, `C Tuba`, `Tuba in C`                                                 | bass       | `Tuba C`           |
| `Tuba B`, `B Tuba`, `Bb Tuba`, `Tuba in B`                                    | treble     | `Tuba B`           |
| `Tuba B`, `B Tuba`, `Bb Tuba`, `Tuba in B`                                    | bass       | `Tuba B C`         |
| `Tuba Es`, `Es Tuba`, `Eb Tuba`, `Tuba in Eb`                                 | treble     | `Tuba Es`          |
| `Bass` read by OCR as `Ba?`, `BaR`, `BaB`, `Bal` (from `Baß`)                 | as `Bass`  | as `Bass`          |
| `Schlagzeug`, `Drums`                                                         | treble     | `Drums Schlagzeug` |
| `Percussion`                                                                  | treble     | `Drums Percussion` |
| `Mallet Percussion`                                                           | treble     | `Drums Mallet Percussion` |
| `Glockenspiel`, `Lyra`                                                        | treble     | `Drums Glockenspiel`|
| `Kleine Trommel`                                                              | treble     | `Drums Kleine Trommel`|
| `Timpani`                                                                     | treble     | `Drums Timpani`    |
| `Pauken`                                                                      | treble     | `Drums Pauken`     |
| `Fagott`                                                                      | treble     | `Fagott`           |
| `Bassoon`                                                                     | treble     | `Bassoon`          |
| `Euphonium B`, `Bb Euphonium`                                                 | treble     | `Euphonium B`      |
| `Euphonium B`, `Bb Euphonium`                                                 | bass       | `Euphonium B C`    |
| `Euphonium`, `C Euphonium`, `Baritone Horn`                                   | bass       | `Euphonium C`      |
| `Tuba Es`, `Eb Tuba`                                                          | bass       | `Tuba Es C`        |
| `Posaune`, `Trombone` (no key prefix)                                         | treble     | `Posaune B`        |
| `Bb Solo Trombone`, `Posaune B Solo`                                          | treble / bass | `Posaune B Solo` / `Posaune B C Solo` |
| `Solo Trombone`, `Posaune Solo`                                               | bass       | `Posaune Solo`     |
| `Eb Clarinet`, `Klarinette in Es`                                             | treble     | `Klarinette Es`    |
| `Clarinet`, `Bass Clarinet`, `Alto/Tenor/Baritone Saxophone`                  | treble     | German equivalents |
| `Kontrabass` / `Double Bass`                                                  | any        | `Kontrabass` / `Bass Double` |
| `Bass Guitar` / `Guitar`                                                      | any        | `Gitarre Bass` / `Gitarre E` |
| `Drum Set`, `Becken, Große Trommel`, `Mallets`                                | any        | `Drums Set`, `Drums Becken Große Trommel`, `Drums Mallets` |
| `Große Trommel`, `Bass Drum`                                                  | any        | `Drums Große Trommel` |
| `Direktion`, `Partitur`/`Score`                                               | any        | `Direktion`, `Partitur` |
| `Oboe`                                                                        | treble     | `Oboe`             |
| `Gesang`                                                                      | treble     | `Gesang`           |

**Matching algorithm:**
1. Normalise the raw text: trim whitespace, collapse multiple spaces, lowercase.
2. OCR also reads `ü` as `ii` (`Fliigelhorn`) and glues `in` to `Melodie` (`Horn-MelodieinF`); both are undone. Canonicalise key tokens before matching: `Bb` → `B`, `Eb` → `Es`, and the common OCR misreads `Rb`/`3b`/`Bh` → `B` and `Fb`/`Fs`/`Eh` → `Es`. `EBass` is read as `E Bass`. A candidate that keeps its key prefix (`Bb Trombone`) is a longer match than one without it (`Trombone`) and is preferred.
3. Strip leading key prefixes (`B `, `Bb `, `C `, `Es `, `Eb `, `E `, `F `) and try to match the remainder against known instrument root names.
4. If a match is found but the clef determines the variant (e.g. `Bariton B` in treble vs. bass), detect the clef first (§5.6) before resolving.
5. If no match is found, store the raw text as-is and set `instrument_normalized = null`. This page will be flagged `UNDETECTED` in the output.

**English baritone labels:** some editions print the German Tenorhorn as `Baritone TC` beside a `Bb Baritone` part, with `Baritone BC` as the same Bariton part in bass clef. So when a piece has both a label `Baritone TC` and a keyed `Bb Baritone` without clef marker, every page labelled `Baritone TC` becomes `Tenorhorn`, and `Bb Baritone` stays `Bariton B`. Without a `Bb Baritone` in the piece, `Baritone TC` stays `Bariton B`.

### 5.6 Clef Detection

**Goal:** Determine whether the first staff system on the page uses a treble clef or bass clef.

**Algorithm:**
1. Locate the first staff: five evenly spaced horizontal lines in the upper 65% of the page (line segments of at least 1/16 of the page width, as barlines split the lines).
2. Find where the staff starts and examine the following 5 line gaps, where the clef sits. The staff starts at the first column from which at least four of its lines have ink for 2 line gaps in a row, so that a thin vertical scan edge is not mistaken for the staff. A column with ink in the middle of all four gaps between the lines is solid: the start column must not be solid, and at most half of the columns in the run may be, so that a wide dark border is not mistaken for the staff either.
3. Compare the ink just above the top line and just below the bottom line (from 0.5 to 2 line gaps outside the staff) with the ink inside the staff:
   - **Treble clef (𝄞):** extends above and below the staff, so the outside ink exceeds 8% of the inside ink.
   - **Bass clef (𝄢):** stays inside the staff.
4. If no staff geometry is found, fall back to the ink distribution in the left 12% next to the staff top: more than 70% of it in the upper half means `bass`, otherwise `treble`. Without a staff, default to `treble`.
5. An explicit clef marker in the instrument label overrides the image-based result: `(BC)` means bass clef and `(TC)` treble clef (e.g. `Bb Tuba (BC)`). Only a marker on an OCR line that also contains an instrument name counts.

### 5.7 Final-Page Detection (Double Barline)

**Goal:** Detect whether this page is the last page of an instrument's part.

**Algorithm:**
1. Examine the **bottom-right 15%** of the page.
2. Look for a **double vertical barline** (two thick vertical lines close together at the end of the last staff line).
3. Mark `is_final_page = true` if detected.

**Usage:** This signal helps confirm that a page ends an instrument group. The *next* page with a different instrument starts a new group.

### 5.8 Page Number Detection

**Goal:** Extract printed page numbers, if any.

**Location heuristic:**
- Top or bottom margin (outer 8% vertically), in the left or right 20% or the centre 20% horizontally.

**Rules:**
1. Look for isolated numeric OCR results containing only one to three digits in the margin regions. Do not extract digits from mixed text such as the catalog number `No. 1234`.
   - A number less than 5.5 staff gaps above the first staff, or beside it, is a bar number or the count of a multi-bar rest, not a page number. On continuation pages the first staff starts close to the top margin, so its bar number lies in the margin region. In the samples bar numbers stood up to 5 gaps above the staff and page numbers 6.6 gaps or more.
2. Parse the isolated digits as a positive integer and store in `page_number`; reject zero.
3. Treat page number `1` as strong evidence that a new instrument part starts. OCR may merge a page number with the label next to it (`2 Posaune 3`); a single digit from 2 to 9 at the start of such a line also counts.
4. Use other page numbers as secondary ordering/validation signals during grouping (§6).

### 5.9 Part-Start Detection

First pages leave room above the first staff for the title block; continuation pages start their music close to the top margin. Mark a page as a probable part start when any of these holds (staff position = top line of the first staff as a share of the page height):

1. Its isolated printed page number is `1`;
2. The first staff begins between 11.5% and 22% down the page, and no printed page number from 2 to 9 was read (a higher number is handwritten, such as the number of the stack);
3. The expected piece title is detected above the first staff (§5.3), wherever that staff begins;
4. On a small original (`content_crop`, §5.2), a large heading stands above the first staff: a line at least 2.2 staff gaps high with at least five letters that names no instrument. This is the title of a piece, also of a second piece in the same folder, whose words the expected title does not match. On full pages this rule does not apply, because OCR noise above the first staff can look like a heading;
5. The page has an instrument label but no staff at all (cover page of a part); or
6. Header lines other than the title tie the page to a piece, no printed page number from 2 to 9 was read and the first staff begins at least 9% down the page (§5.10).

The same words below the first staff are lyrics or a footer and are not the title. A title or heading above the staff outranks a printed page number, because handwritten stack numbers are also read as page numbers. A heading also counts as title evidence for the title rule below.

Further rules:

- **Score pages:** a part-start page whose OCR lines name four or more different mapped instruments is a full score or instrumentation list; its instrument label is ignored.
- **Title rule (applied before grouping):** when the piece title is read on at least 30% of the non-blank pages, titles are reliable, so a part start without title evidence (`has_title_evidence`, §3.1) is turned into a continuation page. Some editions print the part label centred above the music on continuation pages; such a page has no title evidence.

The title alone is insufficient: contents pages and title pages may contain instrument names but no five-line staff. Instrument extraction must require a detected staff. An unlabelled probable part start must not be appended to the preceding instrument; assign it to `UNKNOWN UNDETECTED` and let following unlabelled score pages continue that group until another detected part start or instrument label.

### 5.10 Several Pieces and Two Parts on One Scan

A folder may hold more than one piece. After all pages are read and turned, in this order (`--single-piece` skips steps 1 and 4, so the expected title is the only one):

1. **Find the pieces.** The candidate of a page is its tallest line above the first staff in the top 35% of the page, with at least five letters, starting with a capital, at least 2.2 staff gaps high, at least 60% of the median height of the expected title where that is read, and no staff lines running through it (music read as text has at least two long rules through the line). The first staff must begin at least 9% down the page: text above music that starts higher up is a direction on a continuation page (`spielen`, `a tempo`). A line that names an instrument or a credit (`Text`, `Musik`, `Arr.`, `von`, ...) or is only a form (`Marsch`, `Polka`, ...) gives no candidate; the title is read clean on other pages. Similar readings (≥ 0.75) are one heading, and readings close to the expected title (≥ 0.7) are misreads of it. A heading becomes another piece when at most 20% of its pages also show the expected title, because composers and subtitles appear together with the title and the titles of two pieces do not, and when it differs from the known pieces (< 0.55). A heading on one page only must also read cleanly (several words of plain letters, capitals only in front, because music read as text often comes out in capitals) on a page that shows no known title and no line of a known piece. Its name is the most frequent reading with all letters read; capitals become `Thomas-Polka` style.
2. **Pages lying across.** A page without staff, label or title may lie across, because the black stripes of a bad scan look like staff lines. When a known title or a label can be read after turning it by 90°, 180° or 270°, it is turned and read again.
3. **Two parts on one scan.** Two small originals can lie one above the other. A line in the middle of the page (30–75% down) that shows a known title (≥ 0.7), with a part label within three line heights and a label or title higher up the page, starts a second part. Lyrics have no label beside them and footers lie lower; conductor's scores are not split. The scan then counts as two entries, each read on its own, and each writes the **whole unchanged scan** to its output file.
4. **Assign the pages.** The best matching title in the header lines decides (≥ 0.7, stricter than §5.3, because titles can share words and half a title may fit the wrong piece better). A page whose title was not read takes the piece whose pages print the same other header lines (similarity ≥ 0.8 and the same digits): a catalogue number or credit that at least two pages of that piece and of no other piece print. Such a line is first-page text, so a page tied to a piece this way counts as first page with title evidence, unless a printed page number from 2 to 9 was read or the first staff begins less than 9% down the page. A page without evidence belongs to the piece before it (the piece after it at the start of the folder); a piece seen on one page only is a stray page, and pages around it do not join it. The pages that now have another title are read again with that title.

Pages of the expected title keep `piece = null`; all other pages store the name of their piece.

---

## 6. Stage 3 — Grouping & Page Ordering

### 6.1 Instrument Group Assignment

**Scan order.** A stack of parts is usually fed as all first pages in part order, followed by the flipped stack of continuation pages in reverse order: F1 F2 … Fn Cn … C1. The flipped stack may also come first, and a single part may be scanned as F C C …. Continuation pages carry at most a small footer label (`Title - 1. Instrument`), which is often unreadable. A continuation page is therefore attached to a part only when its mirrored position or its own label confirms it; everything else goes to `UNKNOWN UNDETECTED` for manual review instead of being attached to a neighbouring part.

**Pieces.** The piece (§5.10) is part of the identity of a part: the same instrument in two pieces is not a duplicate scan, and voices are filled, unnumbered clarinets renamed and duplicates counted and ranked per piece. Mirrored and sequential attachment work across pieces, because a continuation page takes the part of its first page. A label on a continuation page is looked up in its assumed piece first, then in another piece when only one has that part.

**Preparation**, in this order:

1. Apply the title rule (§5.9).
2. Name headerless first pages from their back pages (see below).
3. Fill missing voice numbers of first pages:
   - When the parts of an instrument were scanned k times, each voice up to the highest one read should appear k times. Unnumbered first pages of that instrument fill the voices that are short of scans, lowest voice first, taking the last unnumbered pages (voices 2 and 3 read → the unnumbered page is voice 1).
   - A first page still without a voice inherits the voice of the directly preceding first page of the same instrument (a duplicate scan), if that instrument has numbered parts.
   - An unnumbered `Klarinette` among numbered `Klarinette` parts is the Es clarinet (`Klarinette Es`).
4. Mark duplicate scans: first pages with the same instrument and voice (`Direktion` and `Partitur` only when the page shows the title or is a small original, because the pages of a conductor's score repeat its label) get `duplicate_copy` 0, 1, 2, … in scan order, so each scan forms its own group. Which one becomes the main file is decided after grouping (§7.1 rule 7).

**Assignment:**

1. A first page opens the group keyed by its instrument, voice and `duplicate_copy`. A first page with unmapped label text opens `<text> UNDETECTED`; one without any label opens `UNKNOWN UNDETECTED`.
2. **Conductor score:** a `Direktion` or `Partitur` part is usually last and has unlabelled pages. From its first page on, every non-blank page belongs to it, until another labelled first page starts a new group.
3. **Blocks:** split the remaining pages into blocks of consecutive first pages and the non-first pages that follow them. The trailing pages of a block are moved before the next block's first pages (flipped stack scanned first) when the block has no first pages of its own, or when their count differs from its own first pages but equals the next block's.
4. **Sequential block** (one first page, F C C …): the following pages belong to it. A page whose own label conflicts switches to the part its label resolves to (rule 6), or to `UNKNOWN UNDETECTED`.
5. **Mirrored block** (several first pages): continuation slot k pairs with first page n+1−k. Footer labels are evidence; a label scores +3 for the same instrument and voice, +2 for the same instrument without a voice to compare, and −1.5 for a conflict.
   - If the counts are equal and the labels agree at least as often as they conflict, use the full mirror.
   - Otherwise align the slots by dynamic programming: the first-page index must strictly decrease, each slot may stay unassigned, and each step along the mirror diagonal earns +1.2. Each run of consecutive diagonal assignments is accepted when it has at least one agreement and more agreements than conflicts, or when it covers the whole block with equal counts.
   - A conflicting label inside an accepted run is logged as a warning, and the mirrored assignment is kept.
   - A block with both leading and trailing continuation pages whose counts add up to its first pages is split: the first first pages pair with the leading pages, the rest with the trailing pages.
   - Blank pages keep their slot in the mirror but are not written.
6. **Own label:** a continuation page still unassigned is attached by its own label when it resolves to exactly one existing part: the same instrument and voice; an unnumbered label when the instrument has only one part; or a numbered label when only an unnumbered part of that instrument exists. A label that resolves to no part may have lost its first letters to OCR (`- Bass` for `E - Bass`): it belongs to the only part whose first-page label ends with the same words and has more in front of them. Otherwise it goes to `UNKNOWN UNDETECTED` with a warning.

**Headerless first pages:** When first pages were scanned too short and lost their header, a stack shows n ≥ 3 unlabelled pages followed by n back pages that carry their part labels (U1..Un Cn..C1). Before grouping, each such first page takes the label of its mirrored back page (Uk ↔ C(n+1−k)). Guards: the unlabelled run must not directly follow a first page; at least 60% of the back pages must be labelled, with distinct labels; and fewer than half of those labels may already belong to a first page with title evidence earlier in the input (then the labelled pages are the backs of that ordinary stack). Full re-scans of the same parts later in the input do not count; the headerless pages then become duplicate scans of them, and as incomplete scans they go to `UNKNOWN UNDETECTED` (§7.1 rule 8).

### 6.2 Ordering Within Groups

Pages within each `InstrumentGroup` are ordered by:

1. **Primary:** `global_order` (preserves scan sequence, including the mirrored pairing of §6.1).
2. **Page numbers:** only when every page of the group has a distinct page number (a first page without one counts as page 1), sort by page number. Otherwise keep the scan order.
3. **Bar numbers:** editions without page numbers often print the number of the first bar at the left of each staff. A page's bar is read from the numbers of two or three digits in the left 15% of the page: a number counts when a number above it is 1 to 40 bars smaller or one below it 1 to 40 bars larger, and the highest of at least two such numbers is the page's bar. Single digits there are mostly time signatures; OCR often loses a digit (`36` for `136`), which only makes a number smaller, and sometimes reads a stray number (`331`), which has no neighbour. The bars of one page all lie before those of the next page, so the highest ones order the pages. When no page-number order applies and every continuation page of the group has a distinct bar, the continuation pages are sorted by it; first pages stay in front and blank pages behind the page they followed. A stack of back pages scanned the wrong way round is put right this way. A continuation page with a staff, no page number and no bar is read again in the second read (§5.4 step 9): the left 15% of the page at `--label-dpi`, at full size.
4. **Validation:** If the page numbers that were read are not monotonically increasing, log a warning but **do not re-order** — trust the scan order, as page numbers may be misread.

### 6.3 Final-Page Validation

For each group, if the last page has `is_final_page = true`, this confirms the grouping. If a mid-group page has `is_final_page = true`, log a warning: `"Possible grouping error: page <global_order> has a final barline but is not the last page in group <instrument_name>."`.

---

## 7. Stage 4 — Output PDF Generation

### 7.1 Output File Naming

Template: `<title> - <InstrumentName>.pdf`

**Rules:**

1. `<title>` comes from the `--title` parameter (or working directory name). A part of another piece (§5.10) uses the title of that piece instead, in the same output folder. `UNKNOWN UNDETECTED` and `DUPLICATED` pages use `--title`.
2. `<InstrumentName>` is the normalised name from the mapping table.
3. If the group has a `voice_number`, append it with a space: `<title> - <InstrumentName> <voice>.pdf`
   - Example: `Ein großes Stück - Trompete 1.pdf`
   - A melody part takes the voice before `Melodie`: `Ein großes Stück - Horn F 1 Melodie.pdf`
4. **Duplicate handling:** If the computed filename already exists (because the same instrument appears in multiple disjoint groups), append a `-2`, `-3`, etc. suffix before `.pdf`:
   - `Ein großes Stück - Klarinette.pdf`
   - `Ein großes Stück - Klarinette-2.pdf`
5. **UNDETECTED pages:** Use whatever raw text was detected plus `UNDETECTED`:
   - `Ein großes Stück - SomeText UNDETECTED.pdf`
6. Sanitise filenames: replace characters invalid in Windows filenames (`\ / : * ? " < > |`) with `_`.
7. **Duplicate scans:** When the same part (instrument and voice) has several first pages, the best scan becomes the main file and the others are written as `<title> - <InstrumentName> - Copy N.pdf`. Ranking is done after grouping, when all pages of each scan are known: a legible own part label on the first page, then the smaller skew of the most crooked non-blank page in 0.5° steps (smaller differences are within the estimate's noise), then the less over-darkened first page (smaller share of fully black pixels, grey < 80, among ink pixels, grey < 200), then the later scan.
8. **Single pages scanned again:** before rule 7, the scans of a part are checked for pages scanned more than once. Two continuation pages are the same page when they share at least 4 OCR words of four or more letters and at least 30% of all their words (two scans of a page share 36–62%, two pages of a part about 5%); all first pages of a part are one page.
   - When each scan holds the same number of pages and no page twice, nothing changes, also when the words of two scans of a page do not match.
   - Otherwise the part gets as many files as the page scanned the fewest times was scanned. Each file takes one scan of each page, the best first (a legible label on a first page, then the smaller skew in 0.5° steps, then the less over-darkened page, then the later scan). The remaining scans go to one file `<title> - DUPLICATED.pdf`, where the user can check them, so that `UNKNOWN UNDETECTED` holds only pages that were not recognised or could not be placed: a copy with a first page only, a page scanned twice into the same part, or a first page that lost its header to a short scan.

### 7.2 PDF Assembly

For each `InstrumentGroup`:

1. Create a new PDF document.
2. For each page in `group.pages` (in order):
   - Import the original PDF page identified by `source_file` and `source_page_index`.
   - Apply the detected orientation correction as page rotation metadata.
   - Do not rasterise, re-encode, scale, or otherwise recreate page artwork.
3. Write the PDF to `<out>/<output_filename>`.

The rasterized and orientation-corrected images are analysis-only. Output page
content comes from the source PDF, preserving vector artwork and embedded
image resolution.

### 7.3 Output Directory

- Create `--out` directory (and parents) if it does not exist.
- Without `--out`, use the next numbered run folder `<working_dir>/out-N` (see §2), so earlier runs are kept side by side.
- A sorting run does **not** delete pre-existing files in the output directory. It only overwrites files with conflicting names (and logs a warning when doing so). Saving in the window replaces files as described in §14.6.
- Every run except `--dry-run`, with or without the window, also writes the run record `musicscansort.json` to the output directory (§14.9). A stopped or failed run writes none. Failing to write it is only logged.

---

## 8. Logging & Reporting

### 8.1 Console Output

Use structured log levels:

| Level   | Usage                                                          |
|---------|----------------------------------------------------------------|
| `INFO`  | Stage transitions, file counts, group summaries                |
| `WARN`  | Low-confidence detections, ordering anomalies, duplicate names |
| `ERROR` | Fatal errors, unreadable files                                 |
| `DEBUG` | Per-page analysis details (enabled with a `--verbose` flag)    |

### 8.2 Summary Report

At the end of a successful run, print a summary:

```
=== MusicScanSort Summary ===
Title:           Ein großes Stück
Other pieces:    Eine andere Weise          (only when there are others)
Input:           72 pages from 65 files
Blank pages:     3 (skipped)
Output files:    18
Undetected:      2 pages (written to UNDETECTED files)
Duplicated:      1 page (written to the DUPLICATED file; only when there are some)

Output files:
  Ein großes Stück - Trompete 1.pdf     (3 pages)
  Ein großes Stück - Trompete 2.pdf     (3 pages)
  Ein großes Stück - Klarinette 1.pdf   (2 pages)
  ...
```

---

## 9. Error Handling

| Scenario                              | Behaviour                                                                                          |
|---------------------------------------|----------------------------------------------------------------------------------------------------|
| Unreadable / corrupt PDF              | Log `ERROR`, skip the file, continue processing remaining files. Include file name in the summary. |
| OCR returns no text on a non-blank pg | Treat as a continuation page; attach it only when its mirrored position confirms it (§6.1), otherwise `UNKNOWN UNDETECTED` with a `WARN`. |
| Ambiguous instrument match            | Use best match by Levenshtein distance. Log `WARN` with candidates.                               |
| Output directory not writable         | Abort with exit code **1**.                                                                        |
| Single orphan page at start           | Group as `UNKNOWN UNDETECTED`. Log `WARN`.                                                         |

---

## 10. Technology Recommendations

> **Implementation status:** The program is implemented in Rust. This
> table describes the current implementation.

| Concern                | Current implementation                         |
|------------------------|-------------------------------------------------|
| Language               | Rust (edition 2024)                             |
| PDF reading            | PDFium via `pdfium-render` / `pdfium-bundled`   |
| PDF writing            | PDFium page import from original source files    |
| OCR                    | `ocrs` with RTen and embedded ONNX models       |
| Image processing       | `image` plus Rust analysis routines             |
| CLI argument parsing   | `clap`                                           |
| Window                 | `egui` / `eframe`, `rfd` (folder dialog), `trash` (Recycle Bin) |
| Logging                | `log` / `env_logger`                             |

The `ocrs` engine is an early-preview Latin-script recognizer, so German
instrument-name recognition may be limited. If full-page OCR finds no
instrument, analysis retries an enlarged upper-left header crop and tolerates
single-character OCR errors in mapped instrument names. The ONNX models must
be downloaded before building and are embedded in the resulting executable.
Rasterized pages are used only for analysis. Output PDFs import the original
source pages, preserving vector content and source image resolution; detected
orientation corrections are applied as page rotation rather than resampling
page artwork.

---

## 11. Testing Strategy

### 11.1 Unit Tests

| Module                  | Test Cases                                                                                |
|-------------------------|-------------------------------------------------------------------------------------------|
| CLI parsing             | Default values, explicit overrides, invalid paths, missing dirs                           |
| Blank detection         | White page, near-white page (below threshold), page with minimal marks (above threshold)  |
| Orientation detection   | 0°, 90°, 180°, 270° rotated sample pages                                                 |
| Instrument normalisation| Every row in the mapping table, unknown text, text in brackets, voice number extraction (also English ordinals) |
| Grouping logic          | Sequential pages, continuation pages, orphan pages, duplicate instrument names             |
| File naming             | Titles with special chars, duplicate collision handling, voice numbers                     |
| Editing (§14)           | Moving, appending, new files, undo, discard, what a save has to do                         |
| Saving (§14.6)          | Real PDFs: files that are their own sources, swapped names, a file open in another program |
| Window (§14)            | Simulated mouse and keys: marking, dragging, wheel, saving, results of the PDF thread       |
| Marks (§14.4)           | Each mark lands on its OCR line of the scan for every turn, with skew and for both parts of a two-part scan; frames turn with the page; the switch |
| Exit codes (§2.4)       | Input errors exit with 1, other errors with 2                                              |
| Page numbers (§5.8)     | Bar numbers and rest counts above or beside the first staff are no page numbers           |
| Bar order (§6.2)        | Page bar from neighbouring numbers at the staves (lost digits, stray numbers); pages sorted by it only when every continuation page has one |
| Cut labels (§6.1)       | A continuation label without a part joins the only part whose first-page label ends with it |
| Baritone TC (§5.5)      | `Baritone TC` becomes `Tenorhorn` only in a piece that also has `Bb Baritone` |
| Repeated scans (§7.1)   | A copy without its continuation pages and a page scanned twice go to `DUPLICATED`; complete copies stay whole |
| Deleting a file (§14.5) | Button and Del with no page marked empty the file into "Nicht verwendet"; undo; the save removes it |

Orientation integration validation should also include representative known-upright and 90°/180°/270° sample pages, with the selected correction checked against their known orientation.

### 11.2 Integration / End-to-End Tests

The real scan sets are in `sample-files/<piece>`; the expected per-instrument PDFs, sorted by hand, are in `sample-result-files/<piece>`. The tools in `tools/eval/` run the CLI on them and compare each output file with the expected one page by page (content and rotation). Their README describes the workflow, the baseline scores and the remaining differences that are not detection errors. Every detection or grouping change must keep or improve the score of every sample.

The test `real_run_from_scans_to_edited_files` takes a copy of real scans through the window code with the real PDF thread: sort, show, edit, save, open the result, edit, save. It is not part of the normal test run; its comment says how to start it.

### 11.3 Test Fixtures

Create a small synthetic test set (5–10 pages) with:
- One correctly oriented page with clear instrument and title.
- One rotated page (90°).
- One blank page.
- One page without instrument text (continuation).
- One page with a double barline (final page).

---

## 12. Project Structure

```
MusicScanSort/
├── README.md
├── CLAUDE.md                       # Working rules for AI coding agents
├── Cargo.toml
├── Cargo.lock
├── src/                            # Rust window, CLI and processing pipeline
├── scripts/download-models.ps1     # Fetch OCR models for the build
├── models/                         # Downloaded ONNX model assets
├── docs/
│   └── spec.md                     # This file
├── tools/eval/                     # Run the samples and score them against the expected results
├── sample-files/                   # Real-world sample scans (not committed to VCS)
└── sample-result-files/            # Expected output for the samples (not committed to VCS)
```

---

## 13. Open Questions / Decisions for Implementer

1. **OCR language support:** Instrument names are predominantly German. Verify the Rust OCR engine's recognition quality on the German sample set; the current `ocrs` engine supports Latin-script recognition but may not recognize German instrument names reliably.
2. **Confidence thresholds:** The spec uses a generic 0.5% dark-pixel threshold for blank detection. This may need tuning with real scanned data.
3. **Duplicate-scan choice:** No measurable quality rule reproduces more than about 29 of the 50 main-file choices in the expected results; in most pairs both scans are equally good. Change the ranking only for a clear, visible quality defect.

Resolved: the DPI is configurable (`--dpi`), pages are analysed in parallel (`--workers`), `--dry-run` prints the grouping plan, and the clef is read from the extent of the clef glyph (§5.6).

---

## 14. The Window

Without `--no-gui` or `--dry-run` the program runs in a maximized window (`egui`/`eframe`). The sorting itself is the same as on the command line. All PDF work runs on one thread of its own, so the window never waits for it. The interface is in German and always dark, whatever the system setting; the pages stay white on it.

### 14.1 Start

The window starts by showing the folder of `--dir` (or the current folder) as in §14.7. Nothing is analysed until the user asks for it with "Sortieren".

| Situation | What the window does |
|-----------|----------------------|
| The folder holds PDFs or `out-N` folders | Shows it |
| It holds neither | Shows a start page with "Ordner mit PDFs öffnen…". "Sortieren" stays available, for PDFs in subfolders. |
| `--run` | Starts sorting that folder at once, into the next `out-N`. Without any PDF to sort it only shows the folder. |

"Öffnen…" in the toolbar chooses another folder and shows it the same way. "Schließen" closes the folder, after asking about unsaved changes as on closing the window, and shows the start page; "Sortieren" stays off until a folder is open again. "Sortieren" sorts the folder on show (§14.8).

### 14.2 Progress

The status bar shows the step, the count within it, the share of the whole run and the time so far, and a button to stop the run. The share weighs the steps by their rough duration, so it is an estimate.

A file appears in the list as soon as it is written. Which pages form which file is only known once all pages are read (§6), so the list fills during the last seconds of a run and the status bar is what shows progress before that. Nothing can be edited until all files are written. A stopped or failed run says so, names a folder it left incomplete and goes back to showing the folder it was sorting.

After the run the status bar shows the number of files, of undetected pages, of blank pages and of pages scanned more than once (when there are some), the other pieces found and the scans that could not be read.

### 14.3 List of PDFs

The list on the left shows the files by name, with the number of pages on the right. The part of the name before " - " is the piece: if there are several, the files stand under a heading per piece and without the piece in their row. Within a piece they are sorted by name, ignoring case and with umlauts next to their base letters. A `*` marks a changed file, and `UNDETECTED` and `DUPLICATED` files are highlighted. Arrow keys move through the list, F2 or a double-click renames.

Two rows stay below the list: "+ Neues PDF", and "Nicht verwendet" with the pages no file holds. Those are the blank pages of the run and the pages the user removed, so that a page wrongly taken for blank can be put back. They are never saved.

### 14.4 Page View

The right side shows the pages of the open file in a row, as high as the window. The mouse wheel moves along the row whichever way it turns, and Ctrl+wheel zooms. Below each page is its position, its source file and its page there.

**Marks.** On pages the run record knows (§14.9), frames show where the run read what it used, each with a tag of what it made of it:

| Frame | Tag |
|-------|-----|
| Blue: the title line (§5.3) | `Titel: <title>` |
| Green: the label line (§5.4) | `„<raw label>“: <instrument> <voice>`, or `kein Instrument` |
| Orange: the page number (§5.8) | `Seite <number>` |
| Grey, only on pages the run put into `UNKNOWN UNDETECTED` | none; other text above the first staff or in the bottom quarter, with at least two letters and no staff lines through it |

The frames turn with the page. Hovering over a page shows what the run read on it, as in the corrections, and for an undetected page why: no label found, a label that is no known instrument, or an instrument that fitted no part. "Markierungen" in the toolbar and the key M switch frames and hover text off and on. They are on at every start; the choice is not kept.

The frames follow the OCR lines, which may hold more than the word used, such as a label and title on one line. A label read only in the close-up at `--label-dpi` (§5.4) is framed where the first read saw it, or not at all. Folders of runs from before this feature show no frames.

### 14.5 Editing

| Action | Effect |
|--------|--------|
| Click, Ctrl+click, Shift+click, Ctrl+A, Esc | Marks pages as in the Explorer |
| Dragging within the page view | Moves the pages to the line that shows where they land. Marked pages stay together in their order. |
| Dragging onto a file in the list | Appends the pages to that file. The open file stays open. |
| Dragging onto "+ Neues PDF" | Asks for a name and moves the pages into a new file |
| Dragging onto "Nicht verwendet", "Entfernen", or Del | Takes the marked pages out of the file |
| "Löschen", or Del with no page marked | Moves all pages of the open file to "Nicht verwendet" and opens the next file. Saving moves the emptied file to the Recycle Bin; Ctrl+Z takes it back until then. |
| "Umbenennen", F2, or double-click in the list | Renames the open file. The dialog opens with what follows the last " - " marked, such as "Copy 1" in "Stück - Horn - Copy 1", so that typing replaces it. A name another file with pages has is refused. The name of a deleted (emptied) file is free: that file leaves the list, saving still moves it to the Recycle Bin, and Ctrl+Z brings it back. The same holds for the name of a new file. |
| R | Turns the marked pages by 90° clockwise |
| "Teilen", or T with one page marked | Splits the page into parts (below) |
| "Duplizieren", or Ctrl+D | Puts a copy of each marked page right behind it in the same file and marks the copies |
| Ctrl+Z | Takes back the last edit, as far back as the last save |

Dragging a page that is not marked moves that page alone. Near the edge of the row or list, a drag scrolls it. Moving a page moves that one entry: a scan with two parts (§5.10) stays in the file of the other part. A name gets the safe characters of §7.1, has at most 200 characters and must not be in use. A file without pages stays in the list until saving, and goes then.

**Splitting a page.** For a scan that holds more than one page or piece, "Teilen" shows the marked page large. Dragging over it draws a line straight across the whole page: across if the drag goes more sideways, down if it goes more downwards, through the point where it starts. Several lines are allowed; lines across and down together cut the page into a grid. Dragging a line moves it, a click chooses it, and Del or "Linie löschen" deletes it. The parts are tinted and numbered in reading order, row by row from the left. When the page has ink on both sides of a clear white gap in its middle, the dialog starts with a line through the widest gap: across in the middle 20 to 80 % of the height, or down in the middle 30 to 70 % of the width of a page wider than high. "Teilen" or Enter replaces the page with its parts, which stay marked so that they can be dragged on at once, and Ctrl+Z joins them again. A part is a page like any other: it can be turned, moved, removed and split again. Its caption says "(Teil)".

A part keeps the source page whole and only shows a part of it, so nothing loses quality. Saving copies the source page and shrinks its media box and crop box to the part, so that every viewer and printer shows only the part, while the file still holds the whole scan for each part. The frames of §14.4 show only the marks whose centre lies in the part.

The toolbar groups the buttons by what they act on: folder ("Öffnen…", "Schließen", "Sortieren"), saving ("Speichern", "Alles verwerfen", "Rückgängig"), the open file ("Umbenennen", "Löschen"), the marked pages ("Drehen", "Teilen", "Duplizieren", "Entfernen"), then "Markierungen".

### 14.6 Saving

Edits stay in memory until "Speichern" (Ctrl+S), which applies all of them to the folder. "Alles verwerfen" asks first and returns to the last saved state without touching a file. Closing the window or changing the folder with unsaved changes asks whether to save.

Only changed files are touched:

1. A new name that an unknown file in the folder already has stops the save. Nothing has changed.
2. All new files are written as temporary files, while the old ones are still there to copy pages from.
3. If any file concerned is open in another program, the save stops. Nothing has changed, the temporary files are gone, and the edits stay.
4. Files that only got a new name keep their content unchanged.
5. Replaced and emptied files go to the Recycle Bin. This is the only way the window removes anything. If the Recycle Bin takes none of them, the renames are undone and the save stops with nothing changed. On a drive without a Recycle Bin, Windows may delete them instead (not tested).
6. The temporary files take their final names.

The first save after a run therefore replaces the automatic result in the same `out-N`, with the old versions in the Recycle Bin.

A failure in step 5 or 6 can leave the folder half saved. The temporary files then stay, named `~musicscansort-<numbers> <file name>.tmp`, because they can hold the only copy of a page, and no later save touches them. The window says so, and shows what the folder now holds as in §14.7. The other edits are gone from the window.

### 14.7 Opening a Folder

Showing a folder means showing the PDFs directly in it, whether scans, an earlier `out-N` or any other folder, without analysis. Subfolders stay out. Each file keeps its name and its pages, and all of §14.3 to §14.6 applies. Saving replaces the files in that folder, which are also the source of the pages, so the folder is read again after it. Pages left in "Nicht verwendet" are then only in the old version in the Recycle Bin. A PDF that cannot be read is named in the status bar, not listed and not touched. This also goes for a folder of scans: saving there replaces the scans, with the old versions in the Recycle Bin.

### 14.8 Sorting and Moving Between Folders

"Sortieren" analyses the folder on show including its subfolders, with unsaved changes first saved or dropped by the user's choice, and writes to the next `out-N`. Afterwards the window shows that result. It is greyed out in a folder named `out-N`, which is already a result; other folders cannot be told apart from scans, so it stays available there.

A `--title` applies to every run of the folder it was given for. An `--out` applies to the first run only, so that a second run does not write over the first; later runs go to the next `out-N`.

A choice above the list moves between a folder and its results: "Scans" is the folder itself, followed by its `out-N` folders, newest first. For a folder named `out-N`, the folder above it counts as the folder of scans. A result written elsewhere with `--out` is listed as well until the user moves away. The choice is hidden when there is only one entry.

### 14.9 Record of Corrections

The corrections made by hand after a run are kept, so that they can be studied to improve the detection.

The run record `musicscansort.json` holds:

- **For each page after §5.10:** its scan file and page, and the part of the scan if it holds two. It also holds what the analysis read before grouping: blank, first page, last page, score page, turn and whether text backed it, skew, title, piece, the raw label and the instrument and voice it became, page number, clef, and all OCR lines. Then whether the run put it into `UNKNOWN UNDETECTED`, and the marks of §14.4: kind, OCR text and place as shares of the scan page before any turn, crop or deskew.
- **The files as the run wrote them:** names, and for each page its record entry and its turn against the scan, and for a part of a split page the part of the scan it shows.
- **The files as the folder holds them since the last save,** in the same form.

Each successful save in a folder with a run record updates the last part and writes `Corrections.md` next to it. Its text is in German. A `Korrekturen.md` that earlier versions wrote goes to the Recycle Bin on that save. That file compares the run's files with the files now, so it always shows all corrections since the run, and a correction that was undone disappears. A save in a folder without a record, such as a folder of scans or a result from before this feature, writes neither file. A failure to write them is only logged.

| Section | Holds |
|---------|-------|
| Dateien umbenannt | A file of the run under another name. Each file now counts as the run's file it holds the most pages of, so a renamed file is still found. A rename that only changes the ` - Copy N` ending counts as another choice of main file. |
| Neue Dateien | Files none of the run's files became |
| Seiten in eine andere Datei verschoben | Pages in another file than the run put them |
| Seiten ergänzt, die das Programm weggelassen hat | Pages the run left out, now in a file |
| Seiten entfernt | Pages of the run's files that no file holds now |
| Seiten gedreht | Pages with another turn against the scan than the run gave them |
| Reihenfolge geändert | Files whose pages from the run's file are in another order |
| Seiten geteilt | Pages split into parts, with the number of parts and the files that hold them. A part beyond the first is not listed again as moved, turned or reordered. |
| Seiten unbekannter Herkunft | Pages of files the record does not know (below) |
| Was das Programm auf diesen Seiten gelesen hat | Everything the record holds for each page above, with up to 12 OCR lines |

Showing a result folder ties each PDF to the record when the record has a file of that name, ignoring case, with as many pages. A PDF changed by another program then keeps no tie, and its pages are of unknown origin. Pages listed as removed may then lie in such a file. `Corrections.md` gives times in the local time zone of the computer; the record stores them as seconds since 1970 in UTC.
