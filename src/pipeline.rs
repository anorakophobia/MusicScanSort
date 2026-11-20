//! The sorting run from the scans to the written part files, without any user interface.

use crate::ocr::Ocr;
use crate::pdf;
use crate::{InstrumentGroup, PageImage, analysis, grouping, output, pieces, record};
use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use pdfium_bundled::pdfium_render::prelude::Pdfium;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// An error in the settings or the input folder, as opposed to one while processing. The program
/// exits with code 1 for it and with 2 for any other error (spec §2.4).
#[derive(Debug)]
pub struct InputError(pub String);

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InputError {}

#[derive(Clone, Debug)]
pub struct SortConfig {
    pub input_dir: PathBuf,
    pub title: String,
    pub output_dir: PathBuf,
    pub single_piece: bool,
    pub dpi: u32,
    pub label_dpi: u32,
    pub blank_threshold: f32,
    pub workers: usize,
    pub dry_run: bool,
}

impl SortConfig {
    /// The same settings for another folder, with the title and output folder that folder gets
    /// by default.
    pub fn for_folder(&self, input_dir: &Path) -> Self {
        Self {
            input_dir: input_dir.to_path_buf(),
            title: default_title(input_dir),
            output_dir: next_run_dir(input_dir),
            ..self.clone()
        }
    }
}

pub fn default_title(input_dir: &Path) -> String {
    input_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Music")
        .to_owned()
}

/// `<dir>/out-N` for the next run: one above the highest existing `out-N` folder.
pub fn next_run_dir(input_dir: &Path) -> PathBuf {
    let last_run = std::fs::read_dir(input_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| run_number(&entry.file_name().to_string_lossy()))
        .max()
        .unwrap_or(0);
    input_dir.join(format!("out-{}", last_run + 1))
}

/// The `out-N` folders directly in this folder, newest first.
pub fn run_folders(input_dir: &Path) -> Vec<PathBuf> {
    let mut runs: Vec<(u32, PathBuf)> = std::fs::read_dir(input_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            run_number(&entry.file_name().to_string_lossy()).map(|number| (number, entry.path()))
        })
        .collect();
    runs.sort_by_key(|(number, _)| std::cmp::Reverse(*number));
    runs.into_iter().map(|(_, folder)| folder).collect()
}

/// Whether this folder is named like the result of a run.
pub fn is_run_folder(folder: &Path) -> bool {
    folder
        .file_name()
        .is_some_and(|name| run_number(&name.to_string_lossy()).is_some())
}

fn run_number(folder_name: &str) -> Option<u32> {
    let suffix = folder_name
        .get(..4)?
        .eq_ignore_ascii_case("out-")
        .then(|| &folder_name[4..])?;
    suffix.parse().ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    Reading,
    Analysing,
    Pieces,
    Labels,
    Writing,
}

/// Follows a run. The default does nothing, which is what the command line uses.
pub trait Reporter: Sync {
    fn progress(&self, _stage: Stage, _done: usize, _total: usize) {}
    fn file_written(&self, _group: &InstrumentGroup) {}
    fn cancelled(&self) -> bool {
        false
    }
}

pub struct NoReporter;
impl Reporter for NoReporter {}

/// The error of a run that the user stopped.
#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Counts the finished items of one stage. Usable from the analysis workers.
pub struct StageCounter<'a> {
    reporter: &'a dyn Reporter,
    stage: Stage,
    total: usize,
    done: AtomicUsize,
}

impl<'a> StageCounter<'a> {
    pub fn new(reporter: &'a dyn Reporter, stage: Stage, total: usize) -> Self {
        reporter.progress(stage, 0, total);
        Self {
            reporter,
            stage,
            total,
            done: AtomicUsize::new(0),
        }
    }

    pub fn check_cancelled(&self) -> Result<()> {
        if self.reporter.cancelled() {
            return Err(Cancelled.into());
        }
        Ok(())
    }

    pub fn finished_one(&self) {
        let done = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        self.reporter.progress(self.stage, done, self.total);
    }
}

pub struct SortOutcome {
    pub groups: Vec<InstrumentGroup>,
    /// All analysed pages, also those no part file holds.
    pub pages: Vec<PageImage>,
    pub written: Vec<String>,
    pub piece_titles: Vec<String>,
    /// Scans that could not be read, so no file holds them.
    pub skipped: Vec<String>,
    pub pdf_count: usize,
    pub total_pages: usize,
    pub blank_count: usize,
    pub undetected_count: usize,
    pub duplicated_count: usize,
}

pub fn sort(config: &SortConfig, pdfium: &Pdfium, reporter: &dyn Reporter) -> Result<SortOutcome> {
    let SortConfig {
        input_dir,
        title,
        output_dir,
        dpi,
        label_dpi,
        workers,
        ..
    } = config;
    let (dpi, label_dpi, workers) = (*dpi, *label_dpi, *workers);

    info!("=== MusicScanSort starting ===");
    info!("Working dir: {}", input_dir.display());
    info!("Title:       {title}");
    info!("Output dir:  {}", output_dir.display());
    info!("DPI:         {dpi}");
    info!("Dry run:     {}", config.dry_run);

    info!("Stage 1: Discovering and rasterising PDFs...");
    let pdf_files = pdf::discover_pdfs(input_dir, &pdf::normalize_path(output_dir))?;
    if pdf_files.is_empty() {
        bail!(InputError(format!(
            "No PDF files found in {}",
            input_dir.display()
        )));
    }
    let (mut pages, skipped) = pdf::ingest(&pdf_files, dpi, pdfium, reporter)?;
    if pages.is_empty() {
        bail!("No pages could be extracted from the PDF files.");
    }
    let pdf_count = pdf_files.len();
    let total_pages = pages.len();

    info!("Stage 2: Analysing {total_pages} page(s)...");
    let ocr = Ocr::new().context("Could not initialize embedded OCR models")?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .context("Cannot create analysis worker pool")?;
    let analysing = StageCounter::new(reporter, Stage::Analysing, total_pages);
    let analysis_results = pool.install(|| {
        use rayon::prelude::*;
        pages
            .par_iter_mut()
            .map(|page| {
                analysing.check_cancelled()?;
                let result = analysis::analyse_page(page, config.blank_threshold, title, &ocr, dpi);
                analysing.finished_one();
                result
            })
            .collect::<Vec<_>>()
    });
    for (index, result) in analysis_results.into_iter().enumerate() {
        result.with_context(|| format!("Analysis failed for page {}", index + 1))?;
    }
    let turned = analysis::harmonize_orientation(&mut pages);
    if !turned.is_empty() {
        use rayon::prelude::*;
        // These pages were read upside down, so read them again the right way up.
        let results = pool.install(|| {
            pages
                .par_iter_mut()
                .enumerate()
                .filter(|(index, _)| turned.contains(index))
                .map(|(_, page)| analysis::read_turned_page(page, title, &ocr, dpi))
                .collect::<Vec<_>>()
        });
        for result in results {
            result.context("Analysis failed for a turned page")?;
        }
    }
    let piece_titles = separate_pieces(
        &mut pages,
        title,
        config.single_piece,
        &ocr,
        &pool,
        dpi,
        reporter,
    )?;
    for (id, page) in pages.iter_mut().enumerate() {
        page.id = id;
    }
    if label_dpi > dpi {
        retry_labels(
            &mut pages, &ocr, pdfium, &pool, dpi, label_dpi, workers, reporter,
        )?;
    }
    for page in &pages {
        debug!(
            "Page {} ({} page {}) analysis: title={:?}, clef={:?}",
            page.global_order,
            page.source_file.display(),
            page.source_page_index + 1,
            page.analysis.music_title,
            page.analysis.clef
        );
    }
    let blank_count = pages.iter().filter(|page| page.analysis.is_blank).count();
    info!("Analysis complete. Blank pages: {blank_count}");

    info!("Stage 3: Grouping pages by instrument...");
    let mut groups = grouping::build_groups(&pages);
    grouping::validate_groups(&groups);
    let undetected_count: usize = groups
        .iter()
        .filter(|group| group.key.instrument_name.contains("UNDETECTED"))
        .map(|group| group.pages.len())
        .sum();
    let duplicated_count: usize = groups
        .iter()
        .filter(|group| group.key.instrument_name == grouping::DUPLICATED_NAME)
        .map(|group| group.pages.len())
        .sum();
    info!("Found {} instrument group(s).", groups.len());
    for group in &groups {
        info!("  {}: {} page(s)", group.key.label(), group.pages.len());
    }

    info!(
        "Stage 4: Writing output PDFs to {}...",
        output_dir.display()
    );
    let written = output::write_all(
        &mut groups,
        output_dir,
        title,
        config.dry_run,
        pdfium,
        reporter,
    )?;
    if !config.dry_run {
        let record = record::after_sort(config, &pages, &groups);
        if let Err(err) = record::write(output_dir, &record) {
            warn!("Cannot write the run record: {err:#}");
        }
    }
    Ok(SortOutcome {
        groups,
        pages,
        written,
        piece_titles,
        skipped,
        pdf_count,
        total_pages,
        blank_count,
        undetected_count,
        duplicated_count,
    })
}

/// Finds the other pieces in the folder, gives each scan with two parts one entry per part and
/// notes the piece of each page. Pages that now have a title or part to look for are read again.
fn separate_pieces(
    pages: &mut Vec<PageImage>,
    title: &str,
    single_piece: bool,
    ocr: &Ocr,
    pool: &rayon::ThreadPool,
    dpi: u32,
    reporter: &dyn Reporter,
) -> Result<Vec<String>> {
    use rayon::prelude::*;

    let piece_titles = if single_piece {
        vec![title.to_owned()]
    } else {
        pieces::discover(pages, title)
    };
    let looking = StageCounter::new(reporter, Stage::Pieces, pages.len());
    let results = pool.install(|| {
        pages
            .par_iter_mut()
            .map(|page| {
                looking.check_cancelled()?;
                let turned = analysis::turn_to_readable_text(page, &piece_titles, ocr)?;
                if turned {
                    analysis::read_again(page, title, ocr, dpi)?;
                }
                looking.finished_one();
                Ok(())
            })
            .collect::<Vec<Result<()>>>()
    });
    for result in results {
        result.context("Analysis failed for a page lying across")?;
    }
    let mut read_again = analysis::split_double_parts(pages, &piece_titles);
    let evidence = pieces::assign(pages, &piece_titles);
    let piece_of_page = pieces::fill_gaps(&evidence);
    for (index, page) in pages.iter_mut().enumerate() {
        let piece = piece_of_page[index];
        if piece_titles.len() > 1 {
            page.analysis.piece = (piece > 0).then(|| piece_titles[piece].clone());
        }
        if let Some(found) = evidence[index] {
            page.analysis.piece_evidence = !found.from_title;
            read_again[index] |=
                found.piece > 0 || (!found.from_title && !page.analysis.is_part_start);
        }
    }
    let results = pool.install(|| {
        pages
            .par_iter_mut()
            .zip(&piece_of_page)
            .zip(&read_again)
            .filter(|(_, again)| **again)
            .map(|((page, piece), _)| analysis::read_again(page, &piece_titles[*piece], ocr, dpi))
            .collect::<Vec<_>>()
    });
    for result in results {
        result.context("Analysis failed for a page of another piece")?;
    }
    Ok(piece_titles)
}

/// Reads the labels the first read left open again at `label_dpi`. PDFium renders on this thread,
/// and only one batch of the large renders is kept in memory. A failed retry keeps the first read.
#[allow(clippy::too_many_arguments)]
fn retry_labels(
    pages: &mut [PageImage],
    ocr: &Ocr,
    pdfium: &Pdfium,
    pool: &rayon::ThreadPool,
    dpi: u32,
    label_dpi: u32,
    workers: usize,
    reporter: &dyn Reporter,
) -> Result<()> {
    use rayon::prelude::*;

    // Open labels, and bar numbers of pages without page numbers (§6.2).
    let mut open: Vec<(&mut PageImage, bool, bool)> = pages
        .iter_mut()
        .map(|page| {
            let label = analysis::needs_label_retry(page);
            let bar = analysis::needs_bar_retry(page);
            (page, label, bar)
        })
        .filter(|(_, label, bar)| *label || *bar)
        .collect();
    info!(
        "Stage 2b: Reading {} unclear label(s) and {} bar number(s) again at {label_dpi} DPI...",
        open.iter().filter(|(_, label, _)| *label).count(),
        open.iter().filter(|(_, _, bar)| *bar).count()
    );
    let reading = StageCounter::new(reporter, Stage::Labels, open.len());
    let mut renderer = pdf::PageRenderer::new(pdfium);
    for batch in open.chunks_mut(workers) {
        reading.check_cancelled()?;
        let renders: Vec<_> = batch
            .iter()
            .map(|(page, _, _)| {
                renderer.render(&page.source_file, page.source_page_index, label_dpi)
            })
            .collect();
        pool.install(|| {
            batch
                .par_iter_mut()
                .zip(renders)
                .for_each(|((page, label, bar), render)| {
                    let result = render.and_then(|render| {
                        if *label {
                            analysis::retry_label(page, &render, ocr, dpi, label_dpi)?;
                        }
                        if *bar {
                            analysis::retry_last_bar(page, &render, ocr, dpi, label_dpi)?;
                        }
                        Ok(())
                    });
                    if let Err(err) = result {
                        warn!("Page {}: second read failed: {err:#}", page.global_order);
                    }
                    reading.finished_one();
                });
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_run_writes_to_the_next_numbered_out_folder() {
        let root = std::env::temp_dir().join(format!("musicscansort-runs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(next_run_dir(&root), root.join("out-1"));

        for name in ["out-1", "out-3", "out-final", "Out-2"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        std::fs::write(root.join("out-9"), b"not a folder").unwrap();
        assert_eq!(next_run_dir(&root), root.join("out-4"));
        assert_eq!(
            run_folders(&root),
            [root.join("out-3"), root.join("Out-2"), root.join("out-1")]
        );
        assert!(is_run_folder(&root.join("out-3")));
        assert!(!is_run_folder(&root.join("out-final")));
        assert!(!is_run_folder(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_cancelled_run_stops_with_an_error_the_caller_can_recognise() {
        struct Stopped;
        impl Reporter for Stopped {
            fn cancelled(&self) -> bool {
                true
            }
        }
        let counter = StageCounter::new(&Stopped, Stage::Reading, 1);
        let err = counter
            .check_cancelled()
            .context("Analysis failed for page 1")
            .unwrap_err();
        assert!(err.downcast_ref::<Cancelled>().is_some());
    }
}
