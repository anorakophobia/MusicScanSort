//! The run record in a result folder: what the program read and decided for each scan page, and
//! which pages the files hold now. Each save compares both and writes the corrections made by hand
//! to `Corrections.md`, so that they can be studied to improve the detection.

use crate::ocr::OcrLine;
use crate::pipeline::SortConfig;
use crate::session::{Crop, PageRef, ScanRef};
use crate::{InstrumentGroup, PageImage, analysis, grouping, pdf};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const RECORD_FILE: &str = "musicscansort.json";
pub const CORRECTIONS_FILE: &str = "Corrections.md";
/// The name earlier versions gave the corrections file. A save moves it to the Recycle Bin, so
/// that no outdated list stays next to the current one.
const OLD_CORRECTIONS_FILE: &str = "Korrekturen.md";
const FORMAT: u32 = 1;
/// OCR lines shown per page in the corrections.
const SHOWN_TEXT_LINES: usize = 12;
/// Other text is marked above the first staff, at most down to this share of the page height, and
/// below the next one, where the footer label of a continuation page is.
const MARGIN_TOP: f32 = 0.35;
const MARGIN_BOTTOM: f32 = 0.76;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Record {
    pub format: u32,
    pub program: String,
    /// Seconds since 1970, UTC.
    pub sorted_at: u64,
    pub saved_at: Option<u64>,
    pub scan_dir: PathBuf,
    pub title: String,
    pub settings: Settings,
    pub pages: Vec<ScanPage>,
    /// The files as the run wrote them.
    pub sorted_files: Vec<RecordFile>,
    /// The files as the folder holds them since the last save.
    pub files: Vec<RecordFile>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Settings {
    pub dpi: u32,
    pub label_dpi: u32,
    pub blank_threshold: f32,
    pub single_piece: bool,
}

/// What the analysis read on one scan page, before grouping.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ScanPage {
    /// The scan file, relative to `scan_dir`.
    pub scan: String,
    /// Page in the scan, from 1.
    pub page: usize,
    /// 1 for the upper, 2 for the lower part of a scan that holds two parts.
    pub part: Option<u8>,
    /// The number of the page in the log, `Page N`.
    pub log_page: usize,
    pub blank: bool,
    pub first_page: bool,
    pub final_page: bool,
    pub score_page: bool,
    /// Clockwise turn that makes the scan upright.
    pub turned: u16,
    pub turned_from_text: bool,
    pub skew: f32,
    pub title: Option<String>,
    pub piece: Option<String>,
    pub label: Option<String>,
    pub instrument: Option<String>,
    pub voice: Option<u32>,
    pub page_number: Option<u32>,
    pub clef: Option<String>,
    pub text: Vec<String>,
    /// The run put the page into the file of pages it could not place.
    #[serde(default)]
    pub undetected: bool,
    /// Where the page shows what was read, for the window.
    #[serde(default)]
    pub marks: Vec<Mark>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Mark {
    pub kind: MarkKind,
    /// What the OCR read there.
    pub text: String,
    /// Left, top, width and height as shares of the scan page before it is turned.
    pub area: [f32; 4],
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkKind {
    Title,
    Label,
    PageNumber,
    /// Other text in the top or bottom margin.
    Text,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RecordFile {
    pub name: String,
    pub pages: Vec<FilePage>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct FilePage {
    /// Index in `Record::pages`, or none for a page the record does not know.
    pub page: Option<usize>,
    /// Clockwise turn against the scan page.
    pub turned: u16,
    /// The part of the scan page, before it is turned, of a page that was split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<Crop>,
}

pub fn after_sort(config: &SortConfig, pages: &[PageImage], groups: &[InstrumentGroup]) -> Record {
    let sorted_files: Vec<RecordFile> = groups
        .iter()
        .filter(|group| !group.pages.is_empty())
        .map(|group| RecordFile {
            name: group.output_filename.clone(),
            pages: group
                .pages
                .iter()
                .map(|page| FilePage {
                    page: Some(page.id),
                    turned: page.orientation,
                    area: None,
                })
                .collect(),
        })
        .collect();
    let undetected: HashSet<usize> = groups
        .iter()
        .filter(|group| group.key.instrument_name == grouping::UNKNOWN_NAME)
        .flat_map(|group| group.pages.iter().map(|page| page.id))
        .collect();
    Record {
        format: FORMAT,
        program: env!("CARGO_PKG_VERSION").to_owned(),
        sorted_at: now(),
        saved_at: None,
        scan_dir: config.input_dir.clone(),
        title: config.title.clone(),
        settings: Settings {
            dpi: config.dpi,
            label_dpi: config.label_dpi,
            blank_threshold: config.blank_threshold,
            single_piece: config.single_piece,
        },
        pages: pages
            .iter()
            .map(|page| ScanPage {
                undetected: undetected.contains(&page.id),
                ..scan_page(page, &config.input_dir)
            })
            .collect(),
        files: sorted_files.clone(),
        sorted_files,
    }
}

/// What the run read on the page. Before the record is written it is not known whether the page is
/// undetected.
pub fn scan_page(page: &PageImage, scan_dir: &Path) -> ScanPage {
    let analysis = &page.analysis;
    let normalized = pdf::normalize_path(scan_dir);
    let scan = page
        .source_file
        .strip_prefix(scan_dir)
        .or_else(|_| page.source_file.strip_prefix(&normalized))
        .unwrap_or(&page.source_file);
    ScanPage {
        scan: scan.display().to_string(),
        page: page.source_page_index + 1,
        part: analysis
            .part_region
            .map(|(top, _)| if top == 0 { 1 } else { 2 }),
        log_page: page.global_order,
        blank: analysis.is_blank,
        first_page: analysis.is_part_start,
        final_page: analysis.is_final_page,
        score_page: analysis.is_score_page,
        turned: page.orientation,
        turned_from_text: analysis.orientation_from_text,
        skew: (analysis.skew_degrees * 10.0).round() / 10.0,
        title: analysis.music_title.clone(),
        piece: analysis.piece.clone(),
        label: analysis.instrument_raw.clone(),
        instrument: analysis.instrument_normalized.clone(),
        voice: analysis.voice_number,
        page_number: analysis.page_number,
        clef: analysis.clef.clone(),
        text: analysis
            .text_lines
            .iter()
            .map(|line| line.text.clone())
            .collect(),
        undetected: false,
        marks: marks(page),
    }
}

/// The lines the analysis used, and the other text in the margins where labels are.
fn marks(page: &PageImage) -> Vec<Mark> {
    let analysis = &page.analysis;
    if analysis.is_blank {
        return Vec::new();
    }
    let mark = |kind, line: &OcrLine| Mark {
        kind,
        text: line.text.clone(),
        area: analysis::on_scan(page, line).map(|share| (share * 10_000.0).round() / 10_000.0),
    };
    let used = [
        (MarkKind::Title, &analysis.marks.title),
        (MarkKind::Label, &analysis.marks.label),
        (MarkKind::PageNumber, &analysis.marks.page_number),
    ];
    let mut marks: Vec<Mark> = used
        .iter()
        .filter_map(|(kind, line)| line.as_ref().map(|line| mark(*kind, line)))
        .collect();
    let height = page.corrected_image.height() as f32;
    let header_end = analysis.staff_top.map_or(MARGIN_TOP * height, |top| {
        (top as f32).min(MARGIN_TOP * height)
    });
    let in_margin = |line: &&OcrLine| {
        line.top + line.height <= header_end || line.top >= MARGIN_BOTTOM * height
    };
    // Single letters are mostly notes read as text.
    let is_text = |line: &&OcrLine| {
        line.text.chars().filter(|c| c.is_alphanumeric()).count() >= 2
            && !analysis::crosses_staff_lines(&page.corrected_image, line)
    };
    let others = analysis.text_lines.iter().chain(&analysis.marks.extra);
    for line in others.filter(in_margin).filter(is_text) {
        let other = mark(MarkKind::Text, line);
        // The corner reads see the same text again, so a line already marked is left out.
        if !marks.iter().any(|mark| overlap(mark, &other) > 0.5) {
            marks.push(other);
        }
    }
    marks
}

/// The shared area of two marks as a share of the smaller one.
fn overlap(a: &Mark, b: &Mark) -> f32 {
    let [a_left, a_top, a_width, a_height] = a.area;
    let [b_left, b_top, b_width, b_height] = b.area;
    let width = (a_left + a_width).min(b_left + b_width) - a_left.max(b_left);
    let height = (a_top + a_height).min(b_top + b_height) - a_top.max(b_top);
    let smaller = (a_width * a_height).min(b_width * b_height);
    if width <= 0.0 || height <= 0.0 || smaller <= 0.0 {
        return 0.0;
    }
    width * height / smaller
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

/// Writes the record over the one in the folder, without leaving half a file.
pub fn write(dir: &Path, record: &Record) -> Result<()> {
    let text = serde_json::to_string_pretty(record).context("Cannot encode the run record")?;
    let temporary = dir.join(format!("~{RECORD_FILE}.tmp"));
    fs::write(&temporary, text).with_context(|| format!("Cannot write {}", temporary.display()))?;
    fs::rename(&temporary, dir.join(RECORD_FILE))
        .with_context(|| format!("Cannot replace {RECORD_FILE} in {}", dir.display()))
}

/// The record of the folder, if it is the result of a run.
pub fn read(dir: &Path) -> Result<Option<Record>> {
    let path = dir.join(RECORD_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("Cannot read {}", path.display()))?;
    let record: Record = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a run record", path.display()))?;
    if record.format != FORMAT {
        bail!(
            "{} has the unknown format {}",
            path.display(),
            record.format
        );
    }
    Ok(Some(record))
}

impl Record {
    /// The scan pages of a file in the folder, if the record knows it with this many pages. A file
    /// changed by another program keeps no scan pages.
    pub fn scan_refs(&self, file_name: &str, page_count: usize) -> Option<Vec<Option<ScanRef>>> {
        let file = self
            .files
            .iter()
            .find(|file| file.name.to_lowercase() == file_name.to_lowercase())?;
        (file.pages.len() == page_count).then(|| {
            file.pages
                .iter()
                .map(|page| {
                    page.page
                        .filter(|&index| index < self.pages.len())
                        .map(|index| ScanRef {
                            page: index,
                            turned: page.turned,
                            area: page.area,
                        })
                })
                .collect()
        })
    }
}

/// Notes the files of a save in the record of the folder and writes the corrections. A folder
/// without a record is left alone.
pub fn after_save(dir: &Path, files: &[(String, Vec<PageRef>)]) -> Result<()> {
    let Some(mut record) = read(dir)? else {
        return Ok(());
    };
    record.files = files
        .iter()
        .map(|(name, pages)| RecordFile {
            name: name.clone(),
            pages: pages
                .iter()
                .map(|page| match page.turned_from_scan() {
                    Some((index, turned)) => FilePage {
                        page: Some(index),
                        turned,
                        area: page.scan.and_then(|scan| scan.area),
                    },
                    None => FilePage {
                        page: None,
                        turned: page.rotation,
                        area: None,
                    },
                })
                .collect(),
        })
        .collect();
    record.saved_at = Some(now());
    write(dir, &record)?;
    let path = dir.join(CORRECTIONS_FILE);
    fs::write(&path, markdown(&record))
        .with_context(|| format!("Cannot write {}", path.display()))?;
    let old = dir.join(OLD_CORRECTIONS_FILE);
    if old.exists() {
        trash::delete(&old)
            .map_err(|err| anyhow::anyhow!("{err}"))
            .with_context(|| format!("Cannot move {} to the Recycle Bin", old.display()))?;
    }
    Ok(())
}

#[derive(Debug, Default, PartialEq)]
pub struct Corrections {
    /// Name the run gave, name now.
    pub renamed: Vec<(String, String)>,
    /// Files none of the run's files became.
    pub new_files: Vec<String>,
    /// Pages in another file than the run put them, by page.
    pub moved: Vec<Moved>,
    pub turned: Vec<Turned>,
    pub reordered: Vec<Reordered>,
    /// Pages split into parts, and the file of each part.
    pub split: Vec<(usize, Vec<String>)>,
    /// Files with pages the record does not know, and how many.
    pub unknown: Vec<(String, usize)>,
}

#[derive(Debug, PartialEq)]
pub struct Moved {
    pub page: usize,
    /// None: the run left the page out.
    pub from: Option<String>,
    /// None: no file holds it now.
    pub to: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct Turned {
    pub page: usize,
    pub file: String,
    pub sorted: u16,
    pub now: u16,
}

/// A file whose pages from the run's file are in another order now.
#[derive(Debug, PartialEq)]
pub struct Reordered {
    pub file: String,
    pub sorted: Vec<usize>,
    pub now: Vec<usize>,
}

impl Corrections {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Compares the files now with the files of the run.
pub fn corrections(record: &Record) -> Corrections {
    let known = |page: &FilePage| page.page.filter(|&index| index < record.pages.len());
    // Where the run put each page: its file and position.
    let mut sorted_at: HashMap<usize, (usize, usize)> = HashMap::new();
    for (file_index, file) in record.sorted_files.iter().enumerate() {
        for (position, page) in file.pages.iter().enumerate() {
            if let Some(index) = known(page) {
                sorted_at.insert(index, (file_index, position));
            }
        }
    }
    let origins = file_origins(record, &sorted_at);
    let mut corrections = Corrections::default();
    let mut placed = HashSet::new();
    let mut split: std::collections::BTreeMap<usize, Vec<String>> = Default::default();
    for (file, origin) in record.files.iter().zip(&origins) {
        match origin {
            Some(origin) if record.sorted_files[*origin].name != file.name => corrections
                .renamed
                .push((record.sorted_files[*origin].name.clone(), file.name.clone())),
            Some(_) => {}
            None => corrections.new_files.push(file.name.clone()),
        }
        let mut unknown = 0;
        // Sorted position and page of the pages that stayed in their file.
        let mut kept = Vec::new();
        for page in &file.pages {
            let Some(index) = known(page) else {
                unknown += 1;
                continue;
            };
            if page.area.is_some() {
                split.entry(index).or_default().push(file.name.clone());
            }
            if !placed.insert(index) && page.area.is_some() {
                // Another part of the same page: it was moved, turned or placed already.
                continue;
            }
            let sorted_turn = record.pages[index].turned;
            if sorted_turn != page.turned {
                corrections.turned.push(Turned {
                    page: index,
                    file: file.name.clone(),
                    sorted: sorted_turn,
                    now: page.turned,
                });
            }
            match sorted_at.get(&index) {
                Some(&(from, position)) if Some(from) == *origin => kept.push((position, index)),
                sorted => corrections.moved.push(Moved {
                    page: index,
                    from: sorted.map(|&(from, _)| record.sorted_files[from].name.clone()),
                    to: Some(file.name.clone()),
                }),
            }
        }
        if unknown > 0 {
            corrections.unknown.push((file.name.clone(), unknown));
        }
        if kept.windows(2).any(|pair| pair[0].0 > pair[1].0) {
            let now = kept.iter().map(|&(_, index)| index).collect();
            kept.sort();
            corrections.reordered.push(Reordered {
                file: file.name.clone(),
                sorted: kept.iter().map(|&(_, index)| index).collect(),
                now,
            });
        }
    }
    for file in &record.sorted_files {
        for index in file.pages.iter().filter_map(known) {
            if !placed.contains(&index) {
                corrections.moved.push(Moved {
                    page: index,
                    from: Some(file.name.clone()),
                    to: None,
                });
            }
        }
    }
    corrections.moved.sort_by_key(|moved| moved.page);
    corrections.split = split.into_iter().collect();
    corrections
}

/// Which of the run's files each file now is: the one it holds the most pages of, so that a
/// renamed file is still found, else the one with its name.
fn file_origins(record: &Record, sorted_at: &HashMap<usize, (usize, usize)>) -> Vec<Option<usize>> {
    let same_name = |file: usize, sorted: usize| {
        record.files[file].name.to_lowercase() == record.sorted_files[sorted].name.to_lowercase()
    };
    let mut pairs = Vec::new();
    for (file_index, file) in record.files.iter().enumerate() {
        let mut counts: HashMap<usize, usize> = HashMap::new();
        for page in &file.pages {
            if let Some(&(sorted, _)) = page.page.and_then(|index| sorted_at.get(&index)) {
                *counts.entry(sorted).or_default() += 1;
            }
        }
        pairs.extend(counts.into_iter().map(|(sorted, count)| {
            let named = same_name(file_index, sorted);
            (std::cmp::Reverse(count), !named, file_index, sorted)
        }));
    }
    pairs.sort();
    let mut origins = vec![None; record.files.len()];
    let mut taken = vec![false; record.sorted_files.len()];
    for (_, _, file, sorted) in pairs {
        if origins[file].is_none() && !taken[sorted] {
            origins[file] = Some(sorted);
            taken[sorted] = true;
        }
    }
    for (file, origin) in origins.iter_mut().enumerate() {
        if origin.is_none()
            && let Some(sorted) =
                (0..taken.len()).find(|&sorted| !taken[sorted] && same_name(file, sorted))
        {
            *origin = Some(sorted);
            taken[sorted] = true;
        }
    }
    origins
}

/// The corrections as a page to read.
pub fn markdown(record: &Record) -> String {
    let corrections = corrections(record);
    let mut text = String::new();
    let _ = writeln!(text, "# Korrekturen: {}\n", record.title);
    let _ = writeln!(
        text,
        "Sortiert am {} mit MusicScanSort {}, zuletzt gespeichert am {}.  ",
        date(record.sorted_at),
        record.program,
        record.saved_at.map_or_else(|| "–".to_owned(), date)
    );
    let _ = writeln!(
        text,
        "Scans: `{}`, {} Seiten. Einstellungen: {} DPI, Labels mit {} DPI, Leerseiten-Schwelle {}, \
         nur ein Stück: {}.\n",
        record.scan_dir.display(),
        record.pages.len(),
        record.settings.dpi,
        record.settings.label_dpi,
        record.settings.blank_threshold,
        yes_no(record.settings.single_piece)
    );
    let _ = writeln!(
        text,
        "Vergleicht, wie das Programm sortiert hat, mit dem Stand nach dem letzten Speichern. \
         Alle Angaben je Seite stehen in `{RECORD_FILE}`.\n"
    );
    if corrections.is_empty() {
        text.push_str(
            "Keine Korrekturen: Die Dateien sind so, wie das Programm sie sortiert hat.\n",
        );
        return text;
    }
    let _ = writeln!(text, "{}\n", summary(&corrections));

    let page = |index: usize| page_name(&record.pages[index]);
    let read = |index: usize| cell(&reading(&record.pages[index]));
    let moves = |wanted: fn(&Moved) -> bool| -> Vec<&Moved> {
        corrections
            .moved
            .iter()
            .filter(|moved| wanted(moved))
            .collect()
    };

    if !corrections.renamed.is_empty() {
        text.push_str("## Dateien umbenannt\n\n| Programm | Jetzt | Art |\n|---|---|---|\n");
        for (from, to) in &corrections.renamed {
            let _ = writeln!(
                text,
                "| {} | {} | {} |",
                cell(from),
                cell(to),
                rename_kind(from, to)
            );
        }
        text.push('\n');
    }
    if !corrections.new_files.is_empty() {
        text.push_str("## Neue Dateien\n\n");
        for name in &corrections.new_files {
            let _ = writeln!(text, "- {name}");
        }
        text.push('\n');
    }
    let elsewhere = moves(|moved| moved.from.is_some() && moved.to.is_some());
    if !elsewhere.is_empty() {
        text.push_str(
            "## Seiten in eine andere Datei verschoben\n\n\
             | Seite | Programm | Jetzt | Gelesen |\n|---|---|---|---|\n",
        );
        for moved in elsewhere {
            let _ = writeln!(
                text,
                "| {} | {} | {} | {} |",
                page(moved.page),
                cell(moved.from.as_deref().unwrap_or_default()),
                cell(moved.to.as_deref().unwrap_or_default()),
                read(moved.page)
            );
        }
        text.push('\n');
    }
    let added = moves(|moved| moved.from.is_none());
    if !added.is_empty() {
        text.push_str(
            "## Seiten ergänzt, die das Programm weggelassen hat\n\n\
             | Seite | Jetzt | Gelesen |\n|---|---|---|\n",
        );
        for moved in added {
            let _ = writeln!(
                text,
                "| {} | {} | {} |",
                page(moved.page),
                cell(moved.to.as_deref().unwrap_or_default()),
                read(moved.page)
            );
        }
        text.push('\n');
    }
    let removed = moves(|moved| moved.to.is_none());
    if !removed.is_empty() {
        text.push_str("## Seiten entfernt\n\n| Seite | Programm | Gelesen |\n|---|---|---|\n");
        for moved in removed {
            let _ = writeln!(
                text,
                "| {} | {} | {} |",
                page(moved.page),
                cell(moved.from.as_deref().unwrap_or_default()),
                read(moved.page)
            );
        }
        text.push('\n');
    }
    if !corrections.turned.is_empty() {
        text.push_str(
            "## Seiten gedreht\n\nDrehung im Uhrzeigersinn gegenüber dem Scan.\n\n\
             | Seite | Datei | Programm | Jetzt | Gelesen |\n|---|---|---|---|---|\n",
        );
        for turned in &corrections.turned {
            let _ = writeln!(
                text,
                "| {} | {} | {}° | {}° | {} |",
                page(turned.page),
                cell(&turned.file),
                turned.sorted,
                turned.now,
                read(turned.page)
            );
        }
        text.push('\n');
    }
    if !corrections.reordered.is_empty() {
        text.push_str("## Reihenfolge geändert\n\n");
        for reordered in &corrections.reordered {
            let list = |pages: &[usize]| {
                pages
                    .iter()
                    .map(|&index| page(index))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let _ = writeln!(
                text,
                "- {}\n  - Programm: {}\n  - Jetzt: {}",
                reordered.file,
                list(&reordered.sorted),
                list(&reordered.now)
            );
        }
        text.push('\n');
    }
    if !corrections.split.is_empty() {
        text.push_str(
            "## Seiten geteilt\n\n| Seite | Teile | Dateien | Gelesen |\n|---|---|---|---|\n",
        );
        for (index, files) in &corrections.split {
            let mut names = files.clone();
            names.dedup();
            let _ = writeln!(
                text,
                "| {} | {} | {} | {} |",
                page(*index),
                files.len(),
                cell(&names.join(", ")),
                read(*index)
            );
        }
        text.push('\n');
    }
    if !corrections.unknown.is_empty() {
        text.push_str(
            "## Seiten unbekannter Herkunft\n\n\
             Diese Dateien wurden außerhalb des Programms geändert, ihre Seiten sind nicht \
             zuzuordnen. Seiten unter „entfernt“ können deshalb auch dort liegen.\n\n",
        );
        for (name, count) in &corrections.unknown {
            let _ = writeln!(text, "- {name}: {count} Seiten");
        }
        text.push('\n');
    }

    text.push_str("## Was das Programm auf diesen Seiten gelesen hat\n\n");
    let mut involved: Vec<usize> = corrections
        .moved
        .iter()
        .map(|moved| moved.page)
        .chain(corrections.turned.iter().map(|turned| turned.page))
        .chain(corrections.split.iter().map(|(index, _)| *index))
        .chain(
            corrections
                .reordered
                .iter()
                .flat_map(|reordered| reordered.now.clone()),
        )
        .collect();
    involved.sort();
    involved.dedup();
    for index in involved {
        details(&mut text, &record.pages[index]);
    }
    text
}

fn summary(corrections: &Corrections) -> String {
    let count = |wanted: fn(&Moved) -> bool| corrections.moved.iter().filter(|m| wanted(m)).count();
    let parts = [
        (corrections.renamed.len(), "Datei", "Dateien", "umbenannt"),
        (
            corrections.new_files.len(),
            "neue Datei",
            "neue Dateien",
            "",
        ),
        (
            count(|moved| moved.from.is_some() && moved.to.is_some()),
            "Seite",
            "Seiten",
            "verschoben",
        ),
        (
            count(|moved| moved.from.is_none()),
            "Seite",
            "Seiten",
            "ergänzt",
        ),
        (
            count(|moved| moved.to.is_none()),
            "Seite",
            "Seiten",
            "entfernt",
        ),
        (corrections.turned.len(), "Seite", "Seiten", "gedreht"),
        (corrections.split.len(), "Seite", "Seiten", "geteilt"),
        (
            corrections.reordered.len(),
            "Datei",
            "Dateien",
            "umsortiert",
        ),
    ];
    let listed: Vec<String> = parts
        .iter()
        .filter(|(count, ..)| *count > 0)
        .map(|(count, one, many, what)| {
            let noun = if *count == 1 { one } else { many };
            format!("{count} {noun} {what}").trim_end().to_owned()
        })
        .collect();
    format!("**Zusammenfassung:** {}.", listed.join(", "))
}

fn rename_kind(from: &str, to: &str) -> &'static str {
    if without_copy(from) == without_copy(to) {
        "andere Kopie als Hauptdatei"
    } else if from.contains("UNDETECTED") {
        "nicht erkannte Seiten benannt"
    } else {
        "Instrument oder Stimme anders"
    }
}

/// The name without `.pdf` and without a ` - Copy N` ending.
fn without_copy(name: &str) -> String {
    let name = name.to_lowercase();
    let name = name.strip_suffix(".pdf").unwrap_or(&name);
    match name.rsplit_once(" - copy ") {
        Some((base, number)) if number.chars().all(|c| c.is_ascii_digit()) => base.to_owned(),
        _ => name.to_owned(),
    }
}

fn page_name(page: &ScanPage) -> String {
    let part = match page.part {
        Some(1) => " oben",
        Some(_) => " unten",
        None => "",
    };
    format!("{} S. {}{part}", page.scan, page.page)
}

/// What decided where the page went, in one line.
fn reading(page: &ScanPage) -> String {
    if page.blank {
        return "leer".to_owned();
    }
    let mut parts = vec![
        if page.score_page {
            "Partiturseite"
        } else if page.first_page {
            "erste Seite"
        } else {
            "Folgeseite"
        }
        .to_owned(),
    ];
    let voice = page
        .voice
        .map(|voice| format!(" {voice}"))
        .unwrap_or_default();
    parts.push(match (&page.label, &page.instrument) {
        (Some(label), Some(instrument)) => format!("„{label}“ → {instrument}{voice}"),
        (Some(label), None) => format!("„{label}“ → kein Instrument"),
        (None, Some(instrument)) => format!("ohne Label → {instrument}{voice}"),
        (None, None) => "kein Label".to_owned(),
    });
    if let Some(number) = page.page_number {
        parts.push(format!("Seitenzahl {number}"));
    }
    if page.turned != 0 {
        let reason = if page.turned_from_text {
            "nach Text"
        } else {
            "ohne Text"
        };
        parts.push(format!("gedreht {}° ({reason})", page.turned));
    }
    parts.join(", ")
}

/// What the run read on the page and why it could not place it, for the window.
pub fn page_summary(page: &ScanPage) -> String {
    // The font of the window has no arrow.
    let mut lines = vec![reading(page).replace(" → ", ": ")];
    if page.undetected {
        lines.push(match (&page.label, &page.instrument) {
            (None, _) => "Nicht zugeordnet: kein Label gefunden".to_owned(),
            (Some(label), None) => {
                format!("Nicht zugeordnet: „{label}“ ist kein bekanntes Instrument")
            }
            (Some(_), Some(_)) => {
                "Nicht zugeordnet: Instrument erkannt, aber keiner Stimme sicher zuzuordnen"
                    .to_owned()
            }
        });
    }
    let mut facts = Vec::new();
    if let Some(title) = &page.title {
        facts.push(format!("Titel „{title}“"));
    }
    if let Some(piece) = &page.piece {
        facts.push(format!("Stück „{piece}“"));
    }
    if let Some(clef) = &page.clef {
        facts.push(format!("Schlüssel {clef}"));
    }
    if !facts.is_empty() {
        lines.push(facts.join(", "));
    }
    lines.join("\n")
}

fn details(text: &mut String, page: &ScanPage) {
    let _ = writeln!(
        text,
        "### {} (Log: Page {})\n",
        page_name(page),
        page.log_page
    );
    let _ = writeln!(text, "- {}", reading(page));
    let mut facts = Vec::new();
    if let Some(title) = &page.title {
        facts.push(format!("Titel „{title}“"));
    }
    if let Some(piece) = &page.piece {
        facts.push(format!("Stück „{piece}“"));
    }
    if let Some(clef) = &page.clef {
        facts.push(format!("Schlüssel {clef}"));
    }
    facts.push(format!("Schräglage {}°", page.skew));
    if page.final_page {
        facts.push("Schlussstrich".to_owned());
    }
    let _ = writeln!(text, "- {}", facts.join(", "));
    if !page.text.is_empty() {
        let shown: Vec<String> = page
            .text
            .iter()
            .take(SHOWN_TEXT_LINES)
            .map(|line| format!("„{}“", line.chars().take(80).collect::<String>()))
            .collect();
        let more = page.text.len().saturating_sub(SHOWN_TEXT_LINES);
        let more = if more > 0 {
            format!(" (+{more} Zeilen)")
        } else {
            String::new()
        };
        let _ = writeln!(text, "- Text: {}{more}", shown.join(" · "));
    }
    text.push('\n');
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn yes_no(value: bool) -> &'static str {
    if value { "ja" } else { "nein" }
}

/// Day and time in the local time zone of this computer, as `03.11.2026 14:02`.
fn date(seconds: u64) -> String {
    date_in(seconds, &Local)
}

fn date_in<Tz: TimeZone>(seconds: u64, zone: &Tz) -> String {
    let utc = DateTime::from_timestamp(seconds as i64, 0).unwrap_or_default();
    utc.with_timezone(zone)
        .naive_local()
        .format("%d.%m.%Y %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;
    use std::sync::Arc;

    fn scan(page: usize, label: &str) -> ScanPage {
        ScanPage {
            scan: "Scan.pdf".to_owned(),
            page: page + 1,
            log_page: page,
            first_page: !label.is_empty(),
            label: (!label.is_empty()).then(|| label.to_owned()),
            instrument: (!label.is_empty()).then(|| label.to_owned()),
            text: vec![format!("Text {page}")],
            ..ScanPage::default()
        }
    }

    fn file(name: &str, pages: &[usize]) -> RecordFile {
        RecordFile {
            name: name.to_owned(),
            pages: pages
                .iter()
                .map(|&page| FilePage {
                    page: Some(page),
                    turned: 0,
                    area: None,
                })
                .collect(),
        }
    }

    /// Six pages: Trompete on 0 and 1, its second scan on 2 and 3, Horn on 4, a blank page 5.
    fn record() -> Record {
        let mut pages: Vec<ScanPage> = ["Trompete", "", "Trompete", "", "Horn", ""]
            .iter()
            .enumerate()
            .map(|(page, label)| scan(page, label))
            .collect();
        pages[5].blank = true;
        let sorted_files = vec![
            file("Stück - Trompete.pdf", &[0, 1]),
            file("Stück - Trompete - Copy 1.pdf", &[2, 3]),
            file("Stück - Horn.pdf", &[4]),
        ];
        Record {
            format: FORMAT,
            program: "0.0.0".to_owned(),
            sorted_at: 0,
            saved_at: None,
            scan_dir: PathBuf::from("scans"),
            title: "Stück".to_owned(),
            settings: Settings {
                dpi: 150,
                label_dpi: 300,
                blank_threshold: 0.5,
                single_piece: false,
            },
            pages,
            files: sorted_files.clone(),
            sorted_files,
        }
    }

    #[test]
    fn files_as_the_run_wrote_them_have_no_corrections() {
        let record = record();
        assert!(corrections(&record).is_empty());
        assert!(markdown(&record).contains("Keine Korrekturen"));
    }

    #[test]
    fn swapped_copies_are_renames_and_not_moved_pages() {
        let mut record = record();
        record.files = vec![
            file("Stück - Trompete - Copy 1.pdf", &[0, 1]),
            file("Stück - Trompete.pdf", &[2, 3]),
            file("Stück - Horn.pdf", &[4]),
        ];
        let corrections = corrections(&record);
        assert_eq!(
            corrections.renamed,
            [
                (
                    "Stück - Trompete.pdf".to_owned(),
                    "Stück - Trompete - Copy 1.pdf".to_owned()
                ),
                (
                    "Stück - Trompete - Copy 1.pdf".to_owned(),
                    "Stück - Trompete.pdf".to_owned()
                ),
            ]
        );
        assert!(corrections.moved.is_empty());
        assert!(markdown(&record).contains("andere Kopie als Hauptdatei"));
    }

    #[test]
    fn moved_added_removed_turned_and_reordered_pages_are_told_apart() {
        let mut record = record();
        let mut trumpet = file("Stück - Trompete.pdf", &[1, 0, 5]);
        trumpet.pages[0].turned = 180;
        record.files = vec![
            trumpet,
            file("Stück - Trompete - Copy 1.pdf", &[2]),
            file("Stück - Horn.pdf", &[4]),
            file("Stück - Horn 2.pdf", &[3]),
        ];
        let corrections = corrections(&record);
        assert!(corrections.renamed.is_empty());
        assert_eq!(corrections.new_files, ["Stück - Horn 2.pdf"]);
        assert_eq!(
            corrections.moved,
            [
                Moved {
                    page: 3,
                    from: Some("Stück - Trompete - Copy 1.pdf".to_owned()),
                    to: Some("Stück - Horn 2.pdf".to_owned()),
                },
                Moved {
                    page: 5,
                    from: None,
                    to: Some("Stück - Trompete.pdf".to_owned()),
                },
            ]
        );
        assert_eq!(
            corrections.turned,
            [Turned {
                page: 1,
                file: "Stück - Trompete.pdf".to_owned(),
                sorted: 0,
                now: 180,
            }]
        );
        assert_eq!(
            corrections.reordered,
            [Reordered {
                file: "Stück - Trompete.pdf".to_owned(),
                sorted: vec![0, 1],
                now: vec![1, 0],
            }]
        );

        let text = markdown(&record);
        for expected in [
            "1 neue Datei, 1 Seite verschoben, 1 Seite ergänzt, 1 Seite gedreht, 1 Datei umsortiert.",
            "| Scan.pdf S. 4 | Stück - Trompete - Copy 1.pdf | Stück - Horn 2.pdf | Folgeseite, kein Label |",
            "| Scan.pdf S. 6 | Stück - Trompete.pdf | leer |",
            "| Scan.pdf S. 2 | Stück - Trompete.pdf | 0° | 180° |",
            "  - Programm: Scan.pdf S. 1, Scan.pdf S. 2\n  - Jetzt: Scan.pdf S. 2, Scan.pdf S. 1",
            "### Scan.pdf S. 4 (Log: Page 3)",
            "- Text: „Text 3“",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
    }

    #[test]
    fn a_renamed_file_and_removed_pages_are_found() {
        let mut record = record();
        record.files = vec![
            file("Stück - Flügelhorn.pdf", &[0, 1]),
            file("Stück - Trompete - Copy 1.pdf", &[2, 3]),
        ];
        let corrections = corrections(&record);
        assert_eq!(
            corrections.renamed,
            [(
                "Stück - Trompete.pdf".to_owned(),
                "Stück - Flügelhorn.pdf".to_owned()
            )]
        );
        assert_eq!(
            corrections.moved,
            [Moved {
                page: 4,
                from: Some("Stück - Horn.pdf".to_owned()),
                to: None,
            }]
        );
        let text = markdown(&record);
        assert!(text.contains("Instrument oder Stimme anders"));
        assert!(text.contains("## Seiten entfernt"));
    }

    #[test]
    fn a_split_page_is_listed_with_the_files_of_its_parts() {
        let mut record = record();
        let part = |top: u16, bottom: u16| FilePage {
            page: Some(4),
            turned: 0,
            area: Some(Crop {
                left: 0,
                top,
                right: Crop::FULL,
                bottom,
            }),
        };
        record.files[2].pages = vec![part(0, 5000)];
        record.files.push(RecordFile {
            name: "Stück - Tuba.pdf".to_owned(),
            pages: vec![part(5000, Crop::FULL)],
        });
        let corrections = corrections(&record);
        assert_eq!(
            corrections.split,
            [(
                4,
                vec!["Stück - Horn.pdf".to_owned(), "Stück - Tuba.pdf".to_owned()]
            )]
        );
        assert!(corrections.moved.is_empty(), "the page is still where the run put it");
        let text = markdown(&record);
        assert!(text.contains("1 Seite geteilt"));
        assert!(text.contains("| Scan.pdf S. 5 | 2 | Stück - Horn.pdf, Stück - Tuba.pdf |"));

        // The record keeps the part, so that a folder opened again still knows it.
        let refs = record.scan_refs("Stück - Tuba.pdf", 1).unwrap();
        assert_eq!(refs[0].unwrap().area, part(5000, Crop::FULL).area);
    }

    #[test]
    fn pages_of_files_changed_elsewhere_are_unknown() {
        let mut record = record();
        record.files[2].pages[0].page = None;
        let corrections = corrections(&record);
        assert_eq!(corrections.unknown, [("Stück - Horn.pdf".to_owned(), 1)]);
        assert!(markdown(&record).contains("## Seiten unbekannter Herkunft"));

        let record = super::tests::record();
        assert_eq!(
            record.scan_refs("stück - horn.pdf", 1),
            Some(vec![Some(ScanRef {
                page: 4,
                turned: 0,
                area: None
            })])
        );
        assert_eq!(
            record.scan_refs("Stück - Horn.pdf", 2),
            None,
            "another page count"
        );
        assert_eq!(record.scan_refs("Anderes.pdf", 1), None);
    }

    #[test]
    fn a_save_updates_the_record_and_writes_the_corrections() {
        let dir = std::env::temp_dir().join(format!("musicscansort-record-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let source = Arc::new(dir.join("Stück - Horn.pdf"));
        let horn_page = |turned| {
            PageRef::new(Arc::clone(&source), 0, 90, (595.0, 842.0))
                .with_scan(Some(ScanRef {
                    page: 4,
                    turned,
                    area: None,
                }))
        };

        after_save(&dir, &[("X.pdf".to_owned(), vec![horn_page(0)])]).unwrap();
        assert!(
            !dir.join(RECORD_FILE).exists(),
            "a folder without a record stays as it is"
        );
        assert!(!dir.join(CORRECTIONS_FILE).exists());

        write(&dir, &record()).unwrap();
        let mut files: Vec<(String, Vec<PageRef>)> = record()
            .files
            .iter()
            .take(2)
            .map(|file| {
                let pages = file
                    .pages
                    .iter()
                    .map(|page| {
                        PageRef::new(Arc::clone(&source), 0, 0, (595.0, 842.0)).with_scan(Some(
                            ScanRef {
                                page: page.page.unwrap(),
                                turned: 0,
                                area: None,
                            },
                        ))
                    })
                    .collect();
                (file.name.clone(), pages)
            })
            .collect();
        files.push(("Stück - Horn.pdf".to_owned(), vec![horn_page(180)]));
        after_save(&dir, &files).unwrap();

        let saved = read(&dir).unwrap().unwrap();
        assert!(saved.saved_at.is_some());
        assert_eq!(
            saved.files[2].pages,
            [FilePage {
                page: Some(4),
                turned: 270,
                area: None,
            }],
            "the turn on disk plus the turn in the window"
        );
        let text = fs::read_to_string(dir.join(CORRECTIONS_FILE)).unwrap();
        assert!(text.contains("| Scan.pdf S. 5 | Stück - Horn.pdf | 0° | 270° |"));

        fs::write(dir.join(RECORD_FILE), "{").unwrap();
        assert!(read(&dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dates_are_written_in_the_local_time_zone() {
        let utc = FixedOffset::east_opt(0).unwrap();
        let winter = FixedOffset::east_opt(3_600).unwrap();
        let summer = FixedOffset::east_opt(7_200).unwrap();
        assert_eq!(date_in(0, &utc), "01.01.1970 00:00");
        assert_eq!(date_in(1_700_000_000, &utc), "14.11.2023 22:13");
        assert_eq!(date_in(1_700_000_000, &winter), "14.11.2023 23:13");
        assert_eq!(date_in(951_782_400, &utc), "29.02.2000 00:00");
        // 31.12.2025 23:30 UTC is already the new year one hour east.
        assert_eq!(date_in(1_767_223_800, &utc), "31.12.2025 23:30");
        assert_eq!(date_in(1_767_223_800, &winter), "01.01.2026 00:30");
        // 15.07.2026 12:00 UTC.
        assert_eq!(date_in(1_784_116_800, &summer), "15.07.2026 14:00");
        assert_eq!(date(0).len(), "01.01.1970 00:00".len());
    }

    fn ocr_line(text: &str, left: f32, top: f32) -> OcrLine {
        OcrLine {
            text: text.to_owned(),
            confidence: 0.9,
            left,
            top,
            width: 100.0,
            height: 20.0,
        }
    }

    #[test]
    fn marks_frame_the_lines_used_and_the_other_margin_text_once() {
        let image = Arc::new(image::RgbImage::from_pixel(
            1000,
            1400,
            image::Rgb([255; 3]),
        ));
        let title = ocr_line("Ein Stück", 400.0, 50.0);
        let label = ocr_line("Horn 2", 50.0, 120.0);
        let mut page = PageImage {
            source_file: "Scan.pdf".into(),
            source_page_index: 0,
            global_order: 0,
            id: 0,
            image: Arc::clone(&image),
            corrected_image: image,
            orientation: 0,
            analysis: crate::PageAnalysis {
                scan_size: (1000, 1400),
                text_lines: vec![
                    title.clone(),
                    label.clone(),
                    ocr_line("Marsch", 800.0, 60.0),
                    ocr_line("F", 800.0, 160.0),
                    ocr_line("music read as text", 300.0, 700.0),
                    ocr_line("Horn 2", 450.0, 1350.0),
                ],
                ..Default::default()
            },
        };
        page.analysis.marks = crate::MarkLines {
            title: Some(title),
            label: Some(label.clone()),
            page_number: None,
            // The corner read sees the label again, a little shifted.
            extra: vec![ocr_line("Horn 2", 52.0, 121.0)],
        };
        let marks = marks(&page);
        let kinds: Vec<(MarkKind, &str)> = marks
            .iter()
            .map(|mark| (mark.kind, mark.text.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (MarkKind::Title, "Ein Stück"),
                (MarkKind::Label, "Horn 2"),
                (MarkKind::Text, "Marsch"),
                (MarkKind::Text, "Horn 2"),
            ]
        );
        assert_eq!(marks[1].area, [0.05, 0.0857, 0.1, 0.0143]);
        assert_eq!(marks[3].area[1], 0.9643, "the footer label");
    }

    #[test]
    fn records_without_marks_can_still_be_read() {
        let mut value = serde_json::to_value(scan(0, "Horn")).unwrap();
        let fields = value.as_object_mut().unwrap();
        fields.remove("marks");
        fields.remove("undetected");
        let page: ScanPage = serde_json::from_value(value).unwrap();
        assert!(page.marks.is_empty() && !page.undetected);
    }

    #[test]
    fn the_summary_says_why_a_page_was_not_placed() {
        let unknown = ScanPage {
            undetected: true,
            instrument: None,
            ..scan(0, "Hörnchen")
        };
        let summary = page_summary(&unknown);
        assert!(summary.starts_with("erste Seite, „Hörnchen“: kein Instrument"));
        assert!(summary.contains("Nicht zugeordnet: „Hörnchen“ ist kein bekanntes Instrument"));
        let continuation = ScanPage {
            undetected: true,
            ..scan(1, "")
        };
        assert!(page_summary(&continuation).contains("kein Label gefunden"));
        assert!(!page_summary(&scan(2, "Horn")).contains("Nicht zugeordnet"));
    }
}
