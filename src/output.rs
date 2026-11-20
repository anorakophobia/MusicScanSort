use crate::InstrumentGroup;
use crate::pipeline::{InputError, Reporter, Stage, StageCounter};
use crate::session::{Crop, SavePlan, unturn};
use anyhow::{Context, Result, bail};
use log::{info, warn};
use pdfium_bundled::pdfium_render::prelude::*;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn write_all(
    groups: &mut [InstrumentGroup],
    out_dir: &Path,
    title: &str,
    dry_run: bool,
    pdfium: &Pdfium,
    reporter: &dyn Reporter,
) -> Result<Vec<String>> {
    if !dry_run {
        fs::create_dir_all(out_dir).with_context(|| {
            InputError(format!(
                "Cannot create output directory {}",
                out_dir.display()
            ))
        })?;
    }

    let file_count = groups
        .iter()
        .filter(|group| !group.pages.is_empty())
        .count();
    let writing = StageCounter::new(reporter, Stage::Writing, file_count);
    let mut existing_names = HashSet::new();
    let mut written = Vec::new();
    for group in groups.iter_mut().filter(|group| !group.pages.is_empty()) {
        writing.check_cancelled()?;
        let filename = compute_output_filename(title, group, &existing_names);
        existing_names.insert(filename.to_lowercase());
        group.output_filename = filename.clone();
        if dry_run {
            info!(
                "[DRY RUN] Would write: {filename} ({} pages)",
                group.pages.len()
            );
        } else {
            write_group_pdf(group, out_dir, &filename, pdfium)?;
            reporter.file_written(group);
        }
        writing.finished_one();
        written.push(filename);
    }
    Ok(written)
}

/// Why a save stopped.
#[derive(Debug)]
pub struct SaveFailure {
    pub message: String,
    /// The folder is no longer as it was, and the session no longer describes it.
    pub folder_changed: bool,
}

impl SaveFailure {
    fn nothing_changed(err: impl fmt::Display) -> Self {
        Self {
            message: format!("{err:#}. Nichts wurde geändert."),
            folder_changed: false,
        }
    }

    /// The temporary files stay, because they can be the only copy of a page. Their names show
    /// which file each one should have become.
    fn folder_changed(err: impl fmt::Display) -> Self {
        Self {
            message: format!(
                "{err:#}. Der Ordner ist nur teilweise gespeichert: Fehlende Dateien liegen dort \
                 noch als ~musicscansort-….tmp, alte Fassungen im Papierkorb."
            ),
            folder_changed: true,
        }
    }
}

/// Numbers the temporary files of all saves, so that no save can touch those of an earlier one.
static TEMPORARY_FILES: AtomicUsize = AtomicUsize::new(0);

/// Carries out a save, so that a failure leaves the files as they were as far as possible: all new
/// files are complete and no file is in use before the first old file goes, and a Recycle Bin that
/// refuses is undone. `remove` takes the replaced and emptied files, which may be the very files
/// the new ones copy pages from.
pub fn apply_save(
    plan: &SavePlan,
    pdfium: &Pdfium,
    remove: &dyn Fn(&[PathBuf]) -> Result<()>,
) -> std::result::Result<(), SaveFailure> {
    let mut prepared: Vec<(&str, PathBuf)> = Vec::new();
    let checks = prepare_save(plan, pdfium, &mut prepared);
    let discard = |prepared: &[(&str, PathBuf)]| {
        for (_, path) in prepared {
            let _ = fs::remove_file(path);
        }
    };
    if let Err(err) = checks {
        discard(&prepared);
        return Err(SaveFailure::nothing_changed(err));
    }

    let written = prepared.len();
    let mut moved: Vec<(&str, PathBuf)> = Vec::new();
    let undo_moves = |moved: &[(&str, PathBuf)]| {
        for (from, path) in moved {
            let _ = fs::rename(path, plan.dir.join(from));
        }
    };
    for (from, to) in &plan.renames {
        let path = temporary_file(&plan.dir, to);
        if let Err(err) = fs::rename(plan.dir.join(from), &path) {
            undo_moves(&moved);
            discard(&prepared);
            return Err(SaveFailure::nothing_changed(format!(
                "{from} lässt sich nicht umbenennen: {err}"
            )));
        }
        moved.push((from.as_str(), path.clone()));
        prepared.push((to.as_str(), path));
    }

    let old_files: Vec<PathBuf> = plan
        .removes
        .iter()
        .map(|name| plan.dir.join(name))
        .filter(|path| path.exists())
        .collect();
    if !old_files.is_empty()
        && let Err(err) = remove(&old_files)
    {
        let message = format!("Alte Dateien lassen sich nicht in den Papierkorb legen: {err:#}");
        if old_files.iter().all(|path| path.exists()) {
            undo_moves(&moved);
            discard(&prepared[..written]);
            return Err(SaveFailure::nothing_changed(message));
        }
        return Err(SaveFailure::folder_changed(message));
    }
    for (file_name, path) in &prepared {
        if let Err(err) = fs::rename(path, plan.dir.join(file_name)) {
            return Err(SaveFailure::folder_changed(format!(
                "{file_name} lässt sich nicht schreiben: {err}"
            )));
        }
        info!("Saved: {file_name}");
    }
    Ok(())
}

/// Everything before the first old file changes: the checks, and the new files as temporary files.
fn prepare_save<'a>(
    plan: &'a SavePlan,
    pdfium: &Pdfium,
    prepared: &mut Vec<(&'a str, PathBuf)>,
) -> Result<()> {
    fs::create_dir_all(&plan.dir)
        .with_context(|| format!("Der Ordner {} fehlt", plan.dir.display()))?;
    let goes_away: HashSet<String> = plan
        .removes
        .iter()
        .chain(plan.renames.iter().map(|(from, _)| from))
        .map(|name| name.to_lowercase())
        .collect();
    let targets = plan
        .writes
        .iter()
        .map(|(name, _)| name)
        .chain(plan.renames.iter().map(|(_, to)| to));
    for name in targets {
        if !goes_away.contains(&name.to_lowercase()) && plan.dir.join(name).exists() {
            bail!("Im Ordner gibt es schon eine andere Datei {name}");
        }
    }
    for (file_name, pages) in &plan.writes {
        let path = temporary_file(&plan.dir, file_name);
        prepared.push((file_name.as_str(), path.clone()));
        let pages = pages
            .iter()
            .map(|page| (page.source.as_path(), page.index, page.rotation, page.crop));
        write_pdf(pages, &path, pdfium)?;
    }
    let touched = plan
        .removes
        .iter()
        .chain(plan.renames.iter().map(|(from, _)| from));
    if let Some(name) = touched
        .into_iter()
        .find(|name| is_in_use(&plan.dir.join(name)))
    {
        bail!("{name} ist in einem anderen Programm geöffnet");
    }
    Ok(())
}

fn temporary_file(dir: &Path, file_name: &str) -> PathBuf {
    let number = TEMPORARY_FILES.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(
        "~musicscansort-{}-{number} {file_name}.tmp",
        std::process::id()
    ))
}

/// Another program holds the file open against writing, as PDF viewers do.
fn is_in_use(path: &Path) -> bool {
    path.exists() && fs::OpenOptions::new().write(true).open(path).is_err()
}

fn compute_output_filename(
    title: &str,
    group: &InstrumentGroup,
    existing_names: &HashSet<String>,
) -> String {
    let piece = group.key.piece.as_deref().unwrap_or(title);
    let instrument = group.key.label();
    let base = sanitise_filename(&format!("{piece} - {instrument}"));
    let mut candidate = format!("{base}.pdf");
    let mut suffix = 2;
    while existing_names.contains(&candidate.to_lowercase()) {
        candidate = format!("{base}-{suffix}.pdf");
        suffix += 1;
    }
    candidate
}

pub fn sanitise_filename(name: &str) -> String {
    let mut safe = name
        .chars()
        .map(|character| {
            if matches!(
                character,
                '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            ) || character.is_control()
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    while matches!(safe.chars().last(), Some(' ' | '.')) {
        safe.pop();
    }
    if safe.is_empty() {
        "Music".to_owned()
    } else {
        safe
    }
}

fn write_group_pdf(
    group: &InstrumentGroup,
    out_dir: &Path,
    filename: &str,
    pdfium: &Pdfium,
) -> Result<()> {
    let output_path = out_dir.join(filename);
    if output_path.exists() {
        warn!("Overwriting existing file: {}", output_path.display());
    }
    let pages = group.pages.iter().map(|page| {
        (
            page.source_file.as_path(),
            page.source_page_index,
            page.orientation,
            None,
        )
    });
    write_pdf(pages, &output_path, pdfium)?;
    info!(
        "Written: {} ({} pages)",
        output_path.display(),
        group.pages.len()
    );
    Ok(())
}

/// Writes a PDF from pages of other PDFs: source file, page index, rotation to add, and the part
/// of the page to keep.
fn write_pdf<'a>(
    pages: impl Iterator<Item = (&'a Path, usize, u16, Option<Crop>)>,
    output_path: &Path,
    pdfium: &Pdfium,
) -> Result<()> {
    let mut document = pdfium
        .create_new_pdf()
        .context("Cannot create output PDF document")?;
    let mut source_documents = HashMap::new();
    for (source_file, source_page_index, rotation, crop) in pages {
        if !source_documents.contains_key(source_file) {
            let source = pdfium
                .load_pdf_from_file(source_file, None)
                .with_context(|| format!("Cannot reopen source PDF {}", source_file.display()))?;
            source_documents.insert(source_file, source);
        }
        let source = source_documents
            .get(source_file)
            .context("Source PDF cache entry is missing")?;
        let destination_index = document.pages().len();
        let source_index = i32::try_from(source_page_index).with_context(|| {
            format!(
                "Source page index is too large for {}",
                source_file.display()
            )
        })?;
        document
            .pages_mut()
            .copy_page_from_document(source, source_index, destination_index)
            .with_context(|| {
                format!(
                    "Cannot copy page {} from {}",
                    source_page_index + 1,
                    source_file.display()
                )
            })?;
        if let Some(crop) = crop {
            let mut copied_page = document
                .pages_mut()
                .get(destination_index)
                .context("Cannot load copied output page")?;
            crop_page(&mut copied_page, crop)?;
        }
        if rotation != 0 {
            let mut copied_page = document
                .pages_mut()
                .get(destination_index)
                .context("Cannot load copied output page")?;
            let source_rotation = copied_page
                .rotation()
                .context("Cannot read copied page rotation")?
                .as_degrees() as u16;
            copied_page.set_rotation(rotation_from_degrees((source_rotation + rotation) % 360));
        }
    }
    document
        .save_to_file(output_path)
        .with_context(|| format!("Cannot save output PDF {}", output_path.display()))?;
    Ok(())
}

/// Keeps only the part of the page, as it is shown before any rotation is added. The page keeps
/// all its content, but both its media box and its crop box shrink to the part, so that every
/// viewer and printer shows only the part.
fn crop_page(page: &mut PdfPage, crop: Crop) -> Result<()> {
    let shown_turn = page
        .rotation()
        .context("Cannot read page rotation")?
        .as_degrees() as u16;
    let boundaries = page.boundaries();
    let bounds = boundaries
        .crop()
        .or_else(|_| boundaries.media())
        .context("Cannot read page size")?
        .bounds;
    let (x0, y0, x1, y1) = (
        bounds.left().value,
        bounds.bottom().value,
        bounds.right().value,
        bounds.top().value,
    );
    // A point of the shown page, as shares from its top left, in PDF user space, where y grows
    // upwards. The shown page is user space turned clockwise by `shown_turn`.
    let to_user = |point: (f32, f32)| {
        let (x, y) = unturn(point, shown_turn);
        (x0 + x * (x1 - x0), y1 - y * (y1 - y0))
    };
    let [left, top, right, bottom] = crop.shares();
    let (a, b) = (to_user((left, top)), to_user((right, bottom)));
    let rect = PdfRect::new_from_values(a.1.min(b.1), a.0.min(b.0), a.1.max(b.1), a.0.max(b.0));
    let boundaries = page.boundaries_mut();
    boundaries.set_media(rect).context("Cannot crop page")?;
    boundaries.set_crop(rect).context("Cannot crop page")?;
    Ok(())
}

fn rotation_from_degrees(degrees: u16) -> PdfPageRenderRotation {
    match degrees {
        0 => PdfPageRenderRotation::None,
        90 => PdfPageRenderRotation::Degrees90,
        180 => PdfPageRenderRotation::Degrees180,
        270 => PdfPageRenderRotation::Degrees270,
        _ => unreachable!("page rotation must be a multiple of 90 degrees"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GroupKey, PageAnalysis, PageImage};
    use image::{Rgb, RgbImage};
    use std::sync::Arc;

    fn test_group(name: &str, voice: Option<u32>) -> InstrumentGroup {
        let image = RgbImage::from_pixel(10, 10, Rgb([255, 255, 255]));
        InstrumentGroup {
            key: GroupKey {
                piece: None,
                instrument_name: name.to_owned(),
                voice_number: voice,
                copy: 0,
            },
            pages: vec![PageImage {
                source_file: "test.pdf".into(),
                source_page_index: 0,
                global_order: 0,
                id: 0,
                image: Arc::new(image.clone()),
                corrected_image: Arc::new(image),
                orientation: 0,
                analysis: PageAnalysis::default(),
            }],
            output_filename: String::new(),
        }
    }

    #[test]
    fn output_names_are_safe_and_collisions_are_resolved_case_insensitively() {
        let group = test_group("Flöte/Oboe", Some(1));
        let existing = HashSet::from(["mein - flöte_oboe 1.pdf".to_owned()]);
        assert_eq!(
            compute_output_filename("Mein", &group, &existing),
            "Mein - Flöte_Oboe 1-2.pdf"
        );
    }

    #[test]
    fn a_part_of_another_piece_is_named_after_that_piece() {
        let mut group = test_group("Flöte", None);
        group.key.piece = Some("Andere Weise".to_owned());
        assert_eq!(
            compute_output_filename("Ordner", &group, &HashSet::new()),
            "Andere Weise - Flöte.pdf"
        );
    }

    #[test]
    fn the_voice_of_a_melody_part_stands_before_melodie() {
        let group = test_group("Horn F Melodie", Some(2));
        assert_eq!(
            compute_output_filename("Stück", &group, &HashSet::new()),
            "Stück - Horn F 2 Melodie.pdf"
        );
    }
}

#[cfg(test)]
mod save_tests {
    use super::*;
    use crate::session::PageRef;
    use std::cell::RefCell;
    use std::sync::Arc;

    /// One test, because PDFium can only be loaded once per process.
    #[test]
    fn saving() {
        let pdfium = pdfium_bundled::bind_bundled().unwrap();
        pages_can_move_between_files_that_are_their_own_sources(&pdfium);
        renames_keep_the_file_as_it_is_even_when_two_files_swap_names(&pdfium);
        a_file_the_session_does_not_know_is_not_replaced(&pdfium);
        a_recycle_bin_that_refuses_leaves_everything_as_it_was(&pdfium);
        a_failure_after_old_files_went_keeps_the_new_ones_and_says_so(&pdfium);
        parts_of_split_pages_keep_only_their_part(&pdfium);
        #[cfg(windows)]
        a_file_open_in_a_viewer_stops_the_save_before_anything_changes(&pdfium);
    }

    fn folder(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("musicscansort-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// A PDF whose pages can be told apart by their width.
    fn write_test_pdf(path: &Path, widths: &[f32], pdfium: &Pdfium) {
        let mut document = pdfium.create_new_pdf().unwrap();
        for width in widths {
            let size = PdfPagePaperSize::Custom(PdfPoints::new(*width), PdfPoints::new(800.0));
            document.pages_mut().create_page_at_end(size).unwrap();
        }
        document.save_to_file(path).unwrap();
    }

    /// Width and rotation of each page in the file.
    fn read_pages(path: &Path, pdfium: &Pdfium) -> Vec<(u32, u16)> {
        let document = pdfium.load_pdf_from_file(path, None).unwrap();
        document
            .pages()
            .iter()
            .map(|page| {
                let rotation = page.rotation().unwrap().as_degrees() as u16;
                let width = if rotation % 180 == 90 {
                    page.height()
                } else {
                    page.width()
                };
                (width.value.round() as u32, rotation)
            })
            .collect()
    }

    fn page_of(path: &Path, index: usize, rotation: u16) -> PageRef {
        PageRef::new(
            Arc::new(path.to_path_buf()),
            index,
            rotation,
            (100.0, 800.0),
        )
    }

    fn pages_can_move_between_files_that_are_their_own_sources(pdfium: &Pdfium) {
        let dir = folder("move");
        let (a, b) = (dir.join("A.pdf"), dir.join("B.pdf"));
        write_test_pdf(&a, &[101.0, 102.0], pdfium);
        write_test_pdf(&b, &[201.0], pdfium);

        let plan = SavePlan {
            dir: dir.clone(),
            writes: vec![
                ("A.pdf".to_owned(), vec![page_of(&a, 0, 0)]),
                (
                    "B.pdf".to_owned(),
                    vec![page_of(&b, 0, 0), page_of(&a, 1, 90)],
                ),
            ],
            renames: Vec::new(),
            removes: vec!["A.pdf".to_owned(), "B.pdf".to_owned()],
            ..SavePlan::default()
        };
        let removed = RefCell::new(Vec::new());
        apply_save(&plan, pdfium, &|paths| {
            for path in paths {
                let kept = path.with_extension("old");
                fs::rename(path, kept)?;
                removed.borrow_mut().push(path.clone());
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(read_pages(&a, pdfium), [(101, 0)]);
        assert_eq!(read_pages(&b, pdfium), [(201, 0), (102, 90)]);
        assert_eq!(*removed.borrow(), [a.clone(), b.clone()]);
        assert_eq!(read_pages(&a.with_extension("old"), pdfium).len(), 2);
        let left_over: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(left_over.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    fn parts_of_split_pages_keep_only_their_part(pdfium: &Pdfium) {
        let dir = folder("split");
        let source = dir.join("Scan.pdf");
        // Page 1 upright, 400 x 800. Page 2 the same, but turned by 90° in the file, so that it
        // shows 800 wide and 400 high.
        let mut document = pdfium.create_new_pdf().unwrap();
        for _ in 0..2 {
            let size = PdfPagePaperSize::Custom(PdfPoints::new(400.0), PdfPoints::new(800.0));
            document.pages_mut().create_page_at_end(size).unwrap();
        }
        document
            .pages_mut()
            .get(1)
            .unwrap()
            .set_rotation(PdfPageRenderRotation::Degrees90);
        document.save_to_file(&source).unwrap();

        let page = |index| PageRef::new(Arc::new(source.clone()), index, 0, (400.0, 800.0));
        let upper_quarter = page(0).part([0.0, 0.0, 1.0, 0.25]);
        let left_quarter = page(1).part([0.0, 0.0, 0.25, 1.0]);
        let turned_part = PageRef {
            rotation: 90,
            ..page(0).part([0.0, 0.5, 1.0, 1.0])
        };
        let out = dir.join("Teile.pdf");
        let pages = [&upper_quarter, &left_quarter, &turned_part];
        write_pdf(
            pages
                .iter()
                .map(|page| (page.source.as_path(), page.index, page.rotation, page.crop)),
            &out,
            pdfium,
        )
        .unwrap();

        let document = pdfium.load_pdf_from_file(&out, None).unwrap();
        let shown: Vec<(u32, u32)> = document
            .pages()
            .iter()
            .map(|page| (page.width().value.round() as u32, page.height().value.round() as u32))
            .collect();
        assert_eq!(shown, [(400, 200), (200, 400), (400, 400)]);
        let media = |index| {
            let bounds = document
                .pages()
                .get(index)
                .unwrap()
                .boundaries()
                .media()
                .unwrap()
                .bounds;
            [bounds.left(), bounds.bottom(), bounds.right(), bounds.top()]
                .map(|value| value.value.round() as u32)
        };
        assert_eq!(media(0), [0, 600, 400, 800], "the top of the page");
        // Shown turned clockwise, the left of the page is the bottom of the file's page.
        assert_eq!(media(1), [0, 0, 400, 200]);
        assert_eq!(media(2), [0, 0, 400, 400], "the lower half, then turned");
        drop(document);
        fs::remove_dir_all(dir).unwrap();
    }

    fn renames_keep_the_file_as_it_is_even_when_two_files_swap_names(pdfium: &Pdfium) {
        let dir = folder("swap");
        let (a, b) = (dir.join("A.pdf"), dir.join("B.pdf"));
        write_test_pdf(&a, &[101.0], pdfium);
        write_test_pdf(&b, &[201.0], pdfium);
        let bytes_of_a = fs::read(&a).unwrap();

        let plan = SavePlan {
            dir: dir.clone(),
            writes: Vec::new(),
            renames: vec![
                ("A.pdf".to_owned(), "B.pdf".to_owned()),
                ("B.pdf".to_owned(), "A.pdf".to_owned()),
            ],
            removes: Vec::new(),
            ..SavePlan::default()
        };
        apply_save(&plan, pdfium, &|paths| {
            panic!("nothing is replaced, but {paths:?} would go")
        })
        .unwrap();
        assert_eq!(fs::read(&b).unwrap(), bytes_of_a);
        assert_eq!(read_pages(&a, pdfium), [(201, 0)]);
        fs::remove_dir_all(dir).unwrap();
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn a_file_the_session_does_not_know_is_not_replaced(pdfium: &Pdfium) {
        let dir = folder("unknown");
        let (a, other) = (dir.join("A.pdf"), dir.join("Andere.pdf"));
        write_test_pdf(&a, &[101.0], pdfium);
        write_test_pdf(&other, &[301.0], pdfium);
        let plan = SavePlan {
            dir: dir.clone(),
            writes: Vec::new(),
            renames: vec![("A.pdf".to_owned(), "andere.pdf".to_owned())],
            removes: Vec::new(),
            ..SavePlan::default()
        };
        let err = apply_save(&plan, pdfium, &|_| panic!("nothing may be removed")).unwrap_err();
        assert!(!err.folder_changed);
        assert_eq!(names_in(&dir), ["A.pdf", "Andere.pdf"]);
        assert_eq!(read_pages(&other, pdfium), [(301, 0)]);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Files A and B to replace and C to rename, as in all saves that change everything.
    fn plan_for_failures(dir: &Path, pdfium: &Pdfium) -> SavePlan {
        let (a, b, c) = (dir.join("A.pdf"), dir.join("B.pdf"), dir.join("C.pdf"));
        write_test_pdf(&a, &[101.0, 102.0], pdfium);
        write_test_pdf(&b, &[201.0], pdfium);
        write_test_pdf(&c, &[301.0], pdfium);
        SavePlan {
            dir: dir.to_path_buf(),
            writes: vec![
                ("A.pdf".to_owned(), vec![page_of(&a, 0, 0)]),
                (
                    "B.pdf".to_owned(),
                    vec![page_of(&b, 0, 0), page_of(&a, 1, 0)],
                ),
            ],
            renames: vec![("C.pdf".to_owned(), "D.pdf".to_owned())],
            removes: vec!["A.pdf".to_owned(), "B.pdf".to_owned()],
            ..SavePlan::default()
        }
    }

    fn a_recycle_bin_that_refuses_leaves_everything_as_it_was(pdfium: &Pdfium) {
        let dir = folder("refused");
        let plan = plan_for_failures(&dir, pdfium);
        let err = apply_save(&plan, pdfium, &|_| anyhow::bail!("no Recycle Bin")).unwrap_err();
        assert!(!err.folder_changed, "{err:?}");
        assert_eq!(names_in(&dir), ["A.pdf", "B.pdf", "C.pdf"]);
        assert_eq!(read_pages(&dir.join("A.pdf"), pdfium).len(), 2);

        // So the same save can simply run again.
        apply_save(&plan, pdfium, &|paths| {
            for path in paths {
                fs::remove_file(path)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(names_in(&dir), ["A.pdf", "B.pdf", "D.pdf"]);
        assert_eq!(read_pages(&dir.join("B.pdf"), pdfium), [(201, 0), (102, 0)]);
        fs::remove_dir_all(dir).unwrap();
    }

    fn a_failure_after_old_files_went_keeps_the_new_ones_and_says_so(pdfium: &Pdfium) {
        let dir = folder("partial");
        let plan = plan_for_failures(&dir, pdfium);
        let err = apply_save(&plan, pdfium, &|paths| {
            fs::remove_file(&paths[0])?;
            anyhow::bail!("stopped after the first file")
        })
        .unwrap_err();
        assert!(err.folder_changed, "{err:?}");
        let names = names_in(&dir);
        for target in ["A.pdf", "B.pdf", "D.pdf"] {
            assert!(
                names
                    .iter()
                    .any(|name| name.ends_with(&format!(" {target}.tmp"))),
                "{target} is kept: {names:?}"
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    fn a_file_open_in_a_viewer_stops_the_save_before_anything_changes(pdfium: &Pdfium) {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = folder("locked");
        let (a, b) = (dir.join("A.pdf"), dir.join("B.pdf"));
        write_test_pdf(&a, &[101.0, 102.0], pdfium);
        write_test_pdf(&b, &[201.0], pdfium);
        const FILE_SHARE_READ: u32 = 1;
        let viewer = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&b)
            .unwrap();

        let plan = SavePlan {
            dir: dir.clone(),
            writes: vec![
                ("A.pdf".to_owned(), vec![page_of(&a, 0, 0)]),
                (
                    "B.pdf".to_owned(),
                    vec![page_of(&b, 0, 0), page_of(&a, 1, 0)],
                ),
            ],
            renames: Vec::new(),
            removes: vec!["A.pdf".to_owned(), "B.pdf".to_owned()],
            ..SavePlan::default()
        };
        let err = apply_save(&plan, pdfium, &|_| panic!("nothing may be removed")).unwrap_err();
        assert!(err.message.contains("B.pdf"), "{err:?}");
        assert!(!err.folder_changed);
        drop(viewer);

        assert_eq!(read_pages(&a, pdfium), [(101, 0), (102, 0)]);
        assert_eq!(read_pages(&b, pdfium), [(201, 0)]);
        let names: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(names.len(), 2, "no temporary files stay behind");
        fs::remove_dir_all(dir).unwrap();
    }
}
