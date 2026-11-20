# MusicScanSort

MusicScanSort sorts scanned music-sheet PDFs into one PDF per instrument part. It reads the
instrument label, voice and page order on each page, and writes files such as
`<Title> - Trompete 1.pdf`. A window shows the result, where you can correct it page by page.
The program also runs on the command line.

The window is in German. The program is built and tested on Windows only.

## What it does

1. **Analyse each page:** skip blank pages, turn the page upright, and read the title,
   instrument label, voice number, clef and page number. `ocrs` is an early-preview
   Latin-script OCR engine, so labels are re-read from enlarged crops when needed.
2. **Find the pieces:** the largest heading on the pages shows whether the folder holds
   other pieces. Their parts are named after them, like `<Other title> - <Part>.pdf`, and
   compared only with parts of the same piece. A scan with two parts on it goes unchanged
   into the files of both parts; in the window you can split it (**Teilen**).
3. **Group the pages into parts:** a scanner stack is usually all first pages,
   followed by the flipped stack of continuation pages in reverse order. Continuation
   pages are paired with their first page by that mirrored position and checked
   against their footer label. Pages that can't be confirmed go to
   `<Title> - UNKNOWN UNDETECTED.pdf` for manual review.
4. **Write one PDF per part:** the original pages are copied with their rotation
   corrected, without rasterizing them, so they keep their full quality. If a part was
   scanned more than once, the best scan is the main file and the others are written as
   `<Title> - <Part> - Copy 1.pdf`, `... - Copy 2.pdf`. Single pages scanned once too often
   go to `<Title> - DUPLICATED.pdf`.

Each run writes to a new folder `out-1`, `out-2`, … next to the scans, so earlier results stay.
The full rules are in [docs/spec.md](docs/spec.md).

## Getting started

Copy `musicscansort.exe` into a folder of scans named after the piece and double-click it. The
window shows that folder, and **Sortieren** starts the sorting. In a folder without PDFs, the
window lets you choose one.

From a terminal:

```powershell
# Show a folder of PDFs in the window, without sorting. It can hold scans or finished PDFs.
.\musicscansort.exe --dir ".\scans\Ein großes Stück\out-1"

# Start sorting at once into <dir>\out-N (next free number) and show the result in the window
.\musicscansort.exe --dir ".\scans\Ein großes Stück" --title "Ein großes Stück" --run

# The same sorting on the command line, without the window
.\musicscansort.exe --dir ".\scans\Ein großes Stück" --no-gui

# Dry run: analyse and print the grouping plan without writing files
.\musicscansort.exe --dir ".\scans\Ein großes Stück" --dry-run --verbose
```

## The window

The list on the left shows the PDFs of the folder; files with pages to check are highlighted.
Click one to see its pages on the right; the mouse wheel moves along them and Ctrl+wheel zooms.

**Folders**

- **Sortieren** sorts the folder into a new `out-N` and then shows the result. The status bar
  follows the run.
- **Ordner** above the list moves between the scans and their `out-N` folders. **Öffnen…**
  chooses any other folder, **Schließen** closes it.

**Pages**

- **Mark pages** with click, Ctrl+click and Shift+click.
- **Drag them** within the row to change the order, onto another PDF to append them, onto
  "+ Neues PDF" for a new file, or onto "Nicht verwendet" to take them out. Blank pages wait
  there too.
- **Drehen** (**R**) turns the marked pages.
- **Teilen** (**T**) splits one marked page that holds more than one piece or page: draw
  straight lines across or down it, and each part becomes a page of its own. The program
  suggests a line at a clear gap.
- **Duplizieren** (**Ctrl+D**) puts a copy of each marked page right behind it.
- **Entfernen** (**Del**) moves the marked pages to "Nicht verwendet".

**Files**

- **Umbenennen** (**F2** or double-click) renames the open PDF. The part after the last
  " - " is marked, so typing replaces just that.
- **Löschen**, or **Del** with no page marked, moves all pages of the open PDF to
  "Nicht verwendet"; saving puts the emptied PDF in the Recycle Bin. Its name is then free for
  another PDF.

**Checking and saving**

- **Markierungen** (**M**) shows frames where a run read the title (blue), the instrument
  label (green) and the page number (orange). On pages it could not place, grey frames show
  the other text it read near the label. Hovering over a page tells what was read.
- **Rückgängig** (**Ctrl+Z**) takes back the last edit.
- **Speichern** (**Ctrl+S**) writes all changes to the folder, with the old versions in the
  Recycle Bin. **Alles verwerfen** returns to the last saved state.

### Learning from corrections

Each run writes `musicscansort.json` to its `out-N`, with what it read on every page and where it
put the page. Each save there writes `Corrections.md` next to it (its text is in German). That file lists everything changed
by hand since the run: renamed files, moved, added, removed, turned and split pages, new orders,
and for each of these pages what the program read on it. Hand it to an AI coding agent to find the
causes. A folder corrected this way and its scans can also become a new sample with its expected
result (see [tools/eval/README.md](tools/eval/README.md)).

## Command line options

| Flag | Default | Description |
|---|---|---|
| `--dir DIR` | Current directory | Folder to show in the window and to sort (recursive scan) |
| `--title TITLE` | Directory name | Piece title used in output filenames; pages showing another title are named after that piece |
| `--single-piece` | Off | Treat all pages as one piece, even when they show other titles |
| `--out OUTDIR` | `<dir>\out-N` | Output directory; by default each run gets the next free number (`out-1`, `out-2`, …) |
| `--dpi N` | `150` | Rasterization DPI used for analysis; output pages are copied from source PDFs |
| `--label-dpi N` | `300` | Rasterization DPI for reading unclear labels again; a value not above `--dpi` turns this off |
| `--blank-threshold RATIO` | `0.005` | Dark-pixel ratio below which a page is blank |
| `--workers N` | `4` | Number of parallel page-analysis workers |
| `--dry-run` | Off | Analyse and print groups without writing PDFs; shows no window |
| `--no-gui` | Off | Sort on the command line, without the window |
| `--run` | Off | Start sorting at once in the window, instead of only showing the folder |
| `-v`, `--verbose` | Off | Enable per-page debug logging |

PDFs in the selected output directory and root-level conventional output
folders named `out` or `out-*` are excluded from recursive input discovery.
Output names replace characters that are invalid in Windows filenames and are
made unique within each run. Detected orientation corrections are stored as page
rotation, without rotating or resampling the page artwork.

## Building from source

The release build embeds PDFium and both OCR model files into one portable executable.
The model files are fetched once before compiling and are not in version control.

1. Install the Rust stable toolchain and Visual Studio 2022 Build Tools with the
   **Desktop development with C++** workload and a Windows 10/11 SDK.
2. Open an **x64 Native Tools Command Prompt for VS 2022**, change to the
   repository root, download the OCR models, and build the release executable:

   ```powershell
   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\download-models.ps1
   cargo build --release
   ```

3. Distribute `target\release\musicscansort.exe`. It does not need Python, PDFium, the
   Visual C++ Redistributable or separate model files at runtime. It does need permission
   to write temporary files while initializing OCR; those temporary model files are removed
   when the process exits.

Review `THIRD-PARTY-NOTICES.txt` before redistributing the executable; the OCR model licensing
should be verified for the intended distribution.

To run the tests, use `cargo test` from the same developer prompt.

## Project structure

```text
src/                         Rust program: window, command line and processing pipeline
scripts/download-models.ps1  Fetch the ONNX OCR models before building
models/                      Downloaded model assets (git-ignored)
docs/spec.md                 Processing behavior specification
tools/eval/                  Run the samples and score them against sample-result-files
CLAUDE.md                    Working rules for AI coding agents
```

## License

MIT, see [LICENSE](LICENSE). The executable also embeds third-party components with their own
licenses, listed in [THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt).
