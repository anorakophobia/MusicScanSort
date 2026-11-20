mod analysis;
mod grouping;
mod instrument_map;
mod ocr;
mod output;
mod pdf;
mod pieces;
mod pipeline;
mod record;
mod session;
mod ui;
mod worker;

use anyhow::{Context, Result, bail};
use clap::Parser;
use image::RgbImage;
use log::error;
use ocr::OcrLine;
use pipeline::{InputError, SortConfig};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(
    name = "musicscansort",
    about = "Scan music sheet PDFs, identify instruments, and produce per-instrument output PDFs."
)]
struct Args {
    /// Folder of PDFs to show and to sort. Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,

    /// Piece title used in output filenames. Defaults to the working directory name.
    #[arg(long, value_name = "TITLE")]
    title: Option<String>,

    /// Treat all pages as one piece, even when they show other titles.
    #[arg(long)]
    single_piece: bool,

    /// Output directory. Defaults to <dir>/out-N, with N one above the highest existing run.
    #[arg(long, value_name = "OUTDIR")]
    out: Option<PathBuf>,

    /// DPI used to rasterize PDF pages.
    #[arg(long, default_value_t = 150, value_name = "DPI")]
    dpi: u32,

    /// DPI used to read unclear labels again. Not above --dpi: no second read.
    #[arg(long, default_value_t = 300, value_name = "DPI")]
    label_dpi: u32,

    /// Dark-pixel ratio below which a page is considered blank.
    #[arg(long, default_value_t = 0.005, value_name = "RATIO")]
    blank_threshold: f32,

    /// Number of page-analysis workers.
    #[arg(long, default_value_t = 4, value_name = "N")]
    workers: usize,

    /// Analyse and print the grouping plan without writing PDFs. Shows no window.
    #[arg(long)]
    dry_run: bool,

    /// Sort on the command line, without the window.
    #[arg(long)]
    no_gui: bool,

    /// Start sorting at once in the window, instead of only showing the folder.
    #[arg(long, conflicts_with_all = ["no_gui", "dry_run"])]
    run: bool,

    /// Enable per-page DEBUG logging.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Clone, Debug, Default)]
struct PageAnalysis {
    is_blank: bool,
    music_title: Option<String>,
    instrument_raw: Option<String>,
    instrument_normalized: Option<String>,
    voice_number: Option<u32>,
    clef: Option<String>,
    page_number: Option<u32>,
    is_part_start: bool,
    is_final_page: bool,
    /// The page shows the piece title, a printed page number 1 or has no staff (cover).
    has_title_evidence: bool,
    /// Set by grouping: 0 for the main scan, n > 0 for the weaker duplicate scans of the same part.
    duplicate_copy: u32,
    /// The chosen page direction is backed by a readable title or instrument label.
    orientation_from_text: bool,
    /// Measured scan skew in degrees (before analysis-only deskewing).
    skew_degrees: f32,
    /// Share of fully black pixels among all ink pixels; over-darkened scans score higher.
    ink_darkness: f32,
    /// Top of the first staff in pixels at `--dpi`.
    staff_top: Option<u32>,
    /// A first page that lists several instruments; its label is ignored.
    is_score_page: bool,
    /// The OCR line of a label whose key stayed unclear, for the close-up at `--label-dpi`.
    label_line: Option<OcrLine>,
    /// Where the title, label and page number were read, for the marks in the window.
    marks: MarkLines,
    /// Size of the whole scan page in pixels at `--dpi`, before any crop or turn.
    scan_size: (u32, u32),
    /// Part of the scan (left, top, width, height at `--dpi`) that holds the music, when a small
    /// original lies on a larger scan. The analysis only sees this part.
    content_crop: Option<(u32, u32, u32, u32)>,
    /// All OCR lines of the page.
    text_lines: Vec<OcrLine>,
    /// Distance between the lines of the first staff in pixels at `--dpi`.
    staff_gap: Option<f32>,
    /// The highest trusted bar number at the left of the staves. Orders pages without printed
    /// page numbers.
    last_bar: Option<u32>,
    /// The title of the piece when it is not the expected title.
    piece: Option<String>,
    /// Header text other than the title ties this page to its piece, so it is a first page.
    piece_evidence: bool,
    /// Part of the upright scan (top, height at `--dpi`) when the scan holds two parts.
    part_region: Option<(u32, u32)>,
}

/// OCR lines of the upright page that the analysis used, in the coordinates of `text_lines`.
#[derive(Clone, Debug, Default)]
struct MarkLines {
    title: Option<OcrLine>,
    label: Option<OcrLine>,
    page_number: Option<OcrLine>,
    /// Lines of the extra reads of the label corners, which `text_lines` lacks.
    extra: Vec<OcrLine>,
}

#[derive(Clone)]
struct PageImage {
    source_file: PathBuf,
    source_page_index: usize,
    global_order: usize,
    /// Position among all pages once scans with two parts have one entry per part. The run
    /// record names pages by it.
    id: usize,
    image: Arc<RgbImage>,
    corrected_image: Arc<RgbImage>,
    orientation: u16,
    analysis: PageAnalysis,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct GroupKey {
    /// The piece when it is not the expected title.
    piece: Option<String>,
    instrument_name: String,
    voice_number: Option<u32>,
    /// 0 for the main file; n > 0 marks the n-th duplicate scan of the same part.
    copy: u32,
}

impl GroupKey {
    fn label(&self) -> String {
        let mut label = self.instrument_name.clone();
        if let Some(voice) = self.voice_number {
            // The voice belongs to the instrument: "Horn F 1 Melodie".
            match label.strip_suffix(" Melodie") {
                Some(instrument) => label = format!("{instrument} {voice} Melodie"),
                None => label.push_str(&format!(" {voice}")),
            }
        }
        if self.copy > 0 {
            label.push_str(&format!(" - Copy {}", self.copy));
        }
        label
    }
}

struct InstrumentGroup {
    key: GroupKey,
    pages: Vec<PageImage>,
    output_filename: String,
}

fn main() -> ExitCode {
    let args = Args::parse();
    env_logger::Builder::new()
        .filter_level(if args.verbose {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        })
        .format(|buf, record| {
            use std::io::Write;
            writeln!(
                buf,
                "{:<8} {}: {}",
                record.level(),
                record.target(),
                record.args()
            )
        })
        .init();

    let own_console = started_in_own_window();
    let without_window = args.no_gui || args.dry_run;
    let result = if without_window {
        run_without_window(&args)
    } else {
        run_with_window(&args, own_console)
    };
    let exit_code = match &result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("{err:#}");
            exit_code(err)
        }
    };
    // With the window, the console is only still there when the window could not start.
    if own_console && (without_window || result.is_err()) {
        wait_for_enter();
    }
    exit_code
}

/// 1 for an error in the settings or the input folder, 2 for one while processing (spec §2.4).
fn exit_code(err: &anyhow::Error) -> ExitCode {
    if err.downcast_ref::<InputError>().is_some() {
        ExitCode::from(1)
    } else {
        ExitCode::from(2)
    }
}

/// True after a double-click: a terminal shares its console with us, and scripts redirect the output.
#[cfg(windows)]
fn started_in_own_window() -> bool {
    use std::io::IsTerminal;

    unsafe extern "system" {
        fn GetConsoleProcessList(process_list: *mut u32, process_count: u32) -> u32;
    }
    let mut process_list = [0u32; 2];
    let attached = unsafe { GetConsoleProcessList(process_list.as_mut_ptr(), 2) };
    attached == 1
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
}

#[cfg(not(windows))]
fn started_in_own_window() -> bool {
    false
}

fn wait_for_enter() {
    use std::io::Write;

    print!("\nPress Enter to close this window...");
    let _ = std::io::stdout().flush();
    let _ = std::io::stdin().read_line(&mut String::new());
}

/// The settings of a sorting run from the command line.
fn sort_config(args: &Args) -> Result<SortConfig> {
    let cwd = std::env::current_dir().context("Cannot determine current directory")?;
    let requested_input_dir = args.dir.clone().unwrap_or_else(|| cwd.clone());
    let input_dir =
        pdf::normalize_path(&requested_input_dir.canonicalize().with_context(|| {
            InputError(format!(
                "Working directory does not exist: {}",
                requested_input_dir.display()
            ))
        })?);
    if !input_dir.is_dir() {
        bail!(InputError(format!(
            "--dir is not a directory: {}",
            input_dir.display()
        )));
    }

    let title = args
        .title
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| pipeline::default_title(&input_dir));
    let output_dir = match &args.out {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => cwd.join(path),
        None => pipeline::next_run_dir(&input_dir),
    };
    if pdf::normalize_path(&output_dir) == input_dir {
        bail!(InputError(format!(
            "--out must not be the same directory as --dir ({})",
            input_dir.display()
        )));
    }
    if args.dpi == 0 || args.label_dpi == 0 || args.workers == 0 {
        bail!(InputError(
            "--dpi, --label-dpi and --workers must be greater than zero".into()
        ));
    }
    if !(0.0..=1.0).contains(&args.blank_threshold) {
        bail!(InputError(
            "--blank-threshold must be between 0 and 1".into()
        ));
    }
    Ok(SortConfig {
        input_dir,
        title,
        output_dir,
        single_piece: args.single_piece,
        dpi: args.dpi,
        label_dpi: args.label_dpi,
        blank_threshold: args.blank_threshold,
        workers: args.workers,
        dry_run: args.dry_run,
    })
}

fn run_without_window(args: &Args) -> Result<()> {
    let config = sort_config(args)?;
    let pdfium = pdfium_bundled::bind_bundled().context("Cannot load embedded PDFium library")?;
    let outcome = pipeline::sort(&config, &pdfium, &pipeline::NoReporter)?;

    println!();
    println!("=== MusicScanSort Summary ===");
    println!("Title:           {}", config.title);
    if outcome.piece_titles.len() > 1 {
        println!("Other pieces:    {}", outcome.piece_titles[1..].join(", "));
    }
    println!(
        "Input:          {} pages from {} file(s)",
        outcome.total_pages, outcome.pdf_count
    );
    if !outcome.skipped.is_empty() {
        println!("Not readable:    {}", outcome.skipped.join(", "));
    }
    println!("Blank pages:     {} (skipped)", outcome.blank_count);
    println!("Output files:    {}", outcome.written.len());
    println!(
        "Undetected:      {} page(s) (written to UNDETECTED file(s))",
        outcome.undetected_count
    );
    if outcome.duplicated_count > 0 {
        println!(
            "Duplicated:      {} page(s) (written to the DUPLICATED file)",
            outcome.duplicated_count
        );
    }
    if config.dry_run {
        println!("Mode:            DRY RUN — no files written");
    }
    println!();
    if !outcome.written.is_empty() {
        println!("Output files:");
        for group in &outcome.groups {
            if !group.pages.is_empty() {
                println!(
                    "  {:<60} ({} page(s))",
                    group.output_filename,
                    group.pages.len()
                );
            }
        }
    }
    Ok(())
}

fn run_with_window(args: &Args, own_console: bool) -> Result<()> {
    let start = if args.run {
        ui::Start::SortFolder
    } else {
        ui::Start::ShowFolder
    };
    ui::run(sort_config(args)?, start, own_console)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dpi_and_workers() {
        let args = Args::parse_from(["musicscansort"]);
        assert_eq!(args.dpi, 150);
        assert_eq!(args.label_dpi, 300);
        assert_eq!(args.workers, 4);
    }

    #[test]
    fn input_errors_exit_with_1_and_others_with_2() {
        let missing = std::env::temp_dir().join("musicscansort-no-such-folder");
        let args = Args::parse_from(["musicscansort", "--dir", missing.to_str().unwrap()]);
        let err = sort_config(&args).unwrap_err();
        assert_eq!(exit_code(&err), ExitCode::from(1));
        let wrapped = Err::<(), _>(err).context("Sorting failed").unwrap_err();
        assert_eq!(exit_code(&wrapped), ExitCode::from(1));
        assert_eq!(exit_code(&anyhow::anyhow!("OCR failed")), ExitCode::from(2));
    }

    #[test]
    fn the_window_shows_unless_it_is_turned_off() {
        assert!(!Args::parse_from(["musicscansort"]).no_gui);
        assert!(Args::parse_from(["musicscansort", "--no-gui"]).no_gui);
        assert!(!Args::parse_from(["musicscansort"]).run);
        assert!(Args::parse_from(["musicscansort", "--run"]).run);
        assert!(
            Args::try_parse_from(["musicscansort", "--no-gui", "--run"]).is_err(),
            "--run is about the window"
        );
    }
}
