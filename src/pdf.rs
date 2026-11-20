use crate::PageImage;
use crate::pipeline::{Reporter, Stage, StageCounter};
use anyhow::{Context, Result};
use image::RgbImage;
use log::{error, info};
use pdfium_bundled::pdfium_render::prelude::*;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

pub fn discover_pdfs(root: &Path, excluded_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let root = normalize_path(root);
    let mut excluded_dirs = vec![normalize_path(excluded_dir), root.join("out")];
    excluded_dirs.sort();
    excluded_dirs.dedup();
    excluded_dirs.retain(|directory| directory != &root && directory.starts_with(&root));
    for entry in WalkDir::new(&root).into_iter().filter_entry(|entry| {
        if !entry.file_type().is_dir() {
            return true;
        }
        let path = normalize_path(entry.path());
        !excluded_dirs
            .iter()
            .any(|directory| path.starts_with(directory))
            && !is_conventional_output_dir(&root, &path)
    }) {
        let entry = entry.with_context(|| format!("Cannot scan {}", root.display()))?;
        if entry.file_type().is_file()
            && !excluded_dirs
                .iter()
                .any(|directory| normalize_path(entry.path()).starts_with(directory))
            && entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
        {
            files.push(entry.path().to_path_buf());
        }
    }
    files.sort();
    info!(
        "Discovered {} PDF file(s) in {}",
        files.len(),
        root.display()
    );
    Ok(files)
}

fn is_conventional_output_dir(root: &Path, path: &Path) -> bool {
    if path.parent() != Some(root) {
        return false;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.eq_ignore_ascii_case("out")
                || name
                    .get(..4)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("out-"))
        })
}

pub fn normalize_path(path: &Path) -> PathBuf {
    let path_without_verbatim_prefix = if cfg!(windows) {
        let text = path.to_string_lossy();
        if let Some(unc_path) = text.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{unc_path}"))
        } else if let Some(disk_path) = text.strip_prefix(r"\\?\") {
            PathBuf::from(disk_path)
        } else {
            path.to_path_buf()
        }
    } else {
        path.to_path_buf()
    };
    let mut normalized = PathBuf::new();
    for component in path_without_verbatim_prefix.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(windows)]
    #[test]
    fn normalize_path_matches_verbatim_and_regular_drive_paths() {
        assert_eq!(
            normalize_path(Path::new(r"\\?\D:\scores\set\..\out")),
            PathBuf::from(r"D:\scores\out")
        );
    }

    #[test]
    fn discovery_excludes_default_and_custom_output_directories() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "musicscansort-discovery-{}-{unique}",
            std::process::id()
        ));
        let default_output = root.join("out");
        let custom_output = root.join("out-rust");
        let previous_output = root.join("out-rust-previous");
        std::fs::create_dir_all(&default_output).unwrap();
        std::fs::create_dir_all(&custom_output).unwrap();
        std::fs::create_dir_all(&previous_output).unwrap();
        File::create(root.join("scan.pdf")).unwrap();
        File::create(default_output.join("old-result.pdf")).unwrap();
        File::create(custom_output.join("rust-result.pdf")).unwrap();
        File::create(previous_output.join("older-rust-result.pdf")).unwrap();

        let input_root = root.canonicalize().unwrap();
        let pdfs = discover_pdfs(&input_root, &custom_output).unwrap();
        assert_eq!(
            pdfs,
            vec![root.join("scan.pdf")],
            "output PDFs must not become inputs on the next run"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// The PDF files directly in a folder, sorted by name. Unlike the scan folder of a sorting run, its
/// subfolders stay out: they are other runs or other pieces.
pub fn list_folder_pdfs(folder: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder)
        .with_context(|| format!("Cannot read folder {}", folder.display()))?
    {
        let path = entry
            .with_context(|| format!("Cannot read folder {}", folder.display()))?
            .path();
        if path.is_file()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// The size in points of each page of a PDF, as a viewer shows it.
pub fn page_sizes(path: &Path, pdfium: &Pdfium) -> Result<Vec<(f32, f32)>> {
    let document = pdfium
        .load_pdf_from_file(path, None)
        .with_context(|| format!("Cannot open PDF {}", path.display()))?;
    Ok(document
        .pages()
        .iter()
        .map(|page| (page.width().value, page.height().value))
        .collect())
}

/// Renders all pages. Also returns what could not be read: whole files, or single pages of them.
pub fn ingest(
    pdf_files: &[PathBuf],
    dpi: u32,
    pdfium: &Pdfium,
    reporter: &dyn Reporter,
) -> Result<(Vec<PageImage>, Vec<String>)> {
    let name_of = |path: &Path| {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    let mut skipped = Vec::new();
    let total_pages = pdf_files
        .iter()
        .filter_map(|path| pdfium.load_pdf_from_file(path, None).ok())
        .map(|document| document.pages().len() as usize)
        .sum();
    let reading = StageCounter::new(reporter, Stage::Reading, total_pages);
    let mut all_pages = Vec::new();
    for path in pdf_files {
        info!(
            "Ingesting {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        let document = match pdfium.load_pdf_from_file(path, None) {
            Ok(document) => document,
            Err(err) => {
                error!("Cannot open PDF {}: {err} — skipping.", path.display());
                skipped.push(name_of(path));
                continue;
            }
        };
        for (index, page) in document.pages().iter().enumerate() {
            reading.check_cancelled()?;
            let rendered = render_page(&page, dpi);
            reading.finished_one();
            let rendered = match rendered {
                Ok(image) => image,
                Err(err) => {
                    error!(
                        "Cannot render page {index} of {}: {err:#} — skipping page.",
                        path.display()
                    );
                    skipped.push(format!("{}, S. {}", name_of(path), index + 1));
                    continue;
                }
            };
            let global_order = all_pages.len();
            let rendered = Arc::new(rendered);
            all_pages.push(PageImage {
                source_file: path.to_path_buf(),
                source_page_index: index,
                global_order,
                id: global_order,
                image: Arc::clone(&rendered),
                corrected_image: rendered,
                orientation: 0,
                analysis: Default::default(),
            });
        }
    }
    info!("Total pages rasterised: {}", all_pages.len());
    Ok((all_pages, skipped))
}

fn render_page(page: &PdfPage, dpi: u32) -> Result<RgbImage> {
    let bitmap = page
        .render_with_config(&PdfRenderConfig::new().scale_page_by_factor(dpi as f32 / 72.0))
        .context("Rendering failed")?;
    let image = bitmap.as_image().context("Decoding failed")?;
    Ok(image.into_rgb8())
}

/// Renders single pages again, and keeps the last PDF open for the next page.
pub struct PageRenderer<'a> {
    pdfium: &'a Pdfium,
    open: Option<(PathBuf, PdfDocument<'a>)>,
}

impl<'a> PageRenderer<'a> {
    pub fn new(pdfium: &'a Pdfium) -> Self {
        Self { pdfium, open: None }
    }

    pub fn render(&mut self, path: &Path, index: usize, dpi: u32) -> Result<RgbImage> {
        if self.open.as_ref().is_none_or(|(open, _)| open != path) {
            let document = self
                .pdfium
                .load_pdf_from_file(path, None)
                .with_context(|| format!("Cannot open PDF {}", path.display()))?;
            self.open = Some((path.to_path_buf(), document));
        }
        let (_, document) = self.open.as_ref().context("No PDF is open")?;
        let page = document
            .pages()
            .get(index as PdfPageIndex)
            .with_context(|| format!("Cannot open page {index} of {}", path.display()))?;
        render_page(&page, dpi)
    }
}
