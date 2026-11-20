//! The thread that does all PDF work for the window: it owns PDFium, so sorting, showing pages and
//! saving run one after the other and the window never waits.

use crate::pipeline::{self, Cancelled, Reporter, SortConfig, Stage};
use crate::session::{Crop, PageRef, SavePlan, ScanRef};
use crate::{InstrumentGroup, grouping, output, pdf, record};
use anyhow::{Result, anyhow};
use eframe::egui;
use image::imageops;
use log::error;
use pdfium_bundled::pdfium_render::prelude::Pdfium;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

/// Sharp enough to read at the full height of a large screen.
const SHOW_DPI: u32 = 150;
const MAX_IMAGE_SIDE: u32 = 4096;

/// A page as it is shown.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PageKey {
    pub source: Arc<PathBuf>,
    pub index: usize,
    pub rotation: u16,
    pub crop: Option<Crop>,
}

impl From<&PageRef> for PageKey {
    fn from(page: &PageRef) -> Self {
        Self {
            source: Arc::clone(&page.source),
            index: page.index,
            rotation: page.rotation,
            crop: page.crop,
        }
    }
}

pub enum Command {
    Sort(SortConfig),
    Open(PathBuf),
    /// The pages to show, most important first. Replaces the previous list.
    Render(u64, Vec<PageKey>),
    Save(SavePlan),
}

pub struct SortSummary {
    pub other_pieces: Vec<String>,
    /// Scans that could not be read, so no file holds them.
    pub skipped: Vec<String>,
    pub file_count: usize,
    pub undetected_count: usize,
    pub duplicated_count: usize,
    pub blank_count: usize,
}

pub enum Event {
    Progress {
        stage: Stage,
        done: usize,
        total: usize,
    },
    FileWritten {
        file_name: String,
        pages: Vec<PageRef>,
    },
    SortFinished {
        unused: Vec<PageRef>,
        summary: SortSummary,
    },
    SortCancelled,
    Opened {
        dir: PathBuf,
        files: Vec<(String, Vec<PageRef>)>,
        unreadable: Vec<String>,
    },
    Rendered {
        generation: u64,
        key: PageKey,
        image: Result<egui::ColorImage, String>,
    },
    Saved,
    SaveFailed {
        message: String,
        folder_changed: bool,
    },
    Failed(String),
}

pub struct Worker {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
    pub cancel: Arc<AtomicBool>,
}

pub fn spawn(ctx: egui::Context) -> Worker {
    let (commands, command_receiver) = mpsc::channel();
    let (event_sender, events) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let sink = EventSink {
        events: event_sender,
        ctx,
    };
    let worker_cancel = Arc::clone(&cancel);
    std::thread::spawn(move || run(command_receiver, sink, worker_cancel));
    Worker {
        commands,
        events,
        cancel,
    }
}

struct EventSink {
    events: Sender<Event>,
    ctx: egui::Context,
}

impl EventSink {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
        self.ctx.request_repaint();
    }
}

fn run(commands: Receiver<Command>, sink: EventSink, cancel: Arc<AtomicBool>) {
    let pdfium = match pdfium_bundled::bind_bundled() {
        Ok(pdfium) => pdfium,
        Err(err) => {
            sink.send(Event::Failed(format!("PDFium startet nicht: {err}")));
            return;
        }
    };
    let mut renderer = pdf::PageRenderer::new(&pdfium);
    let mut generation = 0;
    let mut wanted: Vec<PageKey> = Vec::new();
    loop {
        let command = if wanted.is_empty() {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            }
        } else {
            match commands.try_recv() {
                Ok(command) => Some(command),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        };
        match command {
            Some(Command::Render(new_generation, keys)) => {
                generation = new_generation;
                wanted = keys;
            }
            Some(command) => {
                wanted.clear();
                // The last shown PDF must not stay open: saving may replace it.
                renderer = pdf::PageRenderer::new(&pdfium);
                execute(command, &pdfium, &sink, &cancel);
            }
            None => {
                let key = wanted.remove(0);
                let image = render(&mut renderer, &key).map_err(|err| format!("{err:#}"));
                sink.send(Event::Rendered {
                    generation,
                    key,
                    image,
                });
            }
        }
    }
}

fn execute(command: Command, pdfium: &Pdfium, sink: &EventSink, cancel: &AtomicBool) {
    let result = match command {
        Command::Sort(config) => sort(&config, pdfium, sink, cancel),
        Command::Open(dir) => open_folder(&dir, pdfium).map(|event| sink.send(event)),
        Command::Save(plan) => {
            sink.send(save(&plan, pdfium));
            Ok(())
        }
        Command::Render(..) => Ok(()),
    };
    if let Err(err) = result {
        if err.downcast_ref::<Cancelled>().is_some() {
            sink.send(Event::SortCancelled);
        } else {
            error!("{err:#}");
            sink.send(Event::Failed(format!("{err:#}")));
        }
    }
}

fn render(renderer: &mut pdf::PageRenderer, key: &PageKey) -> Result<egui::ColorImage> {
    let image = renderer.render(&key.source, key.index, SHOW_DPI)?;
    let image = match key.crop {
        Some(crop) => crop_image(&image, crop),
        None => image,
    };
    let image = match key.rotation {
        90 => imageops::rotate90(&image),
        180 => imageops::rotate180(&image),
        270 => imageops::rotate270(&image),
        _ => image,
    };
    let longest = image.width().max(image.height());
    let image = if longest > MAX_IMAGE_SIDE {
        let scale = MAX_IMAGE_SIDE as f32 / longest as f32;
        imageops::thumbnail(
            &image,
            (image.width() as f32 * scale) as u32,
            (image.height() as f32 * scale) as u32,
        )
    } else {
        image
    };
    let size = [image.width() as usize, image.height() as usize];
    Ok(egui::ColorImage::from_rgb(size, image.as_raw()))
}

/// The part of the image, at least one pixel.
fn crop_image(image: &image::RgbImage, crop: Crop) -> image::RgbImage {
    let [left, top, right, bottom] = crop.shares();
    let (width, height) = (image.width() as f32, image.height() as f32);
    let x = ((left * width) as u32).min(image.width().saturating_sub(1));
    let y = ((top * height) as u32).min(image.height().saturating_sub(1));
    let crop_width = (((right - left) * width).round() as u32).clamp(1, image.width() - x);
    let crop_height = (((bottom - top) * height).round() as u32).clamp(1, image.height() - y);
    imageops::crop_imm(image, x, y, crop_width, crop_height).to_image()
}

struct SortReporter<'a> {
    sink: &'a EventSink,
    cancel: &'a AtomicBool,
    sizes: HashMap<PathBuf, Vec<(f32, f32)>>,
    scan_dir: &'a Path,
}

impl SortReporter<'_> {
    fn page_ref(&self, page: &crate::PageImage, undetected: bool) -> PageRef {
        let reading = record::ScanPage {
            undetected,
            ..record::scan_page(page, self.scan_dir)
        };
        let size = self
            .sizes
            .get(&page.source_file)
            .and_then(|sizes| sizes.get(page.source_page_index))
            .copied()
            .unwrap_or((595.0, 842.0));
        PageRef::new(
            Arc::new(page.source_file.clone()),
            page.source_page_index,
            page.orientation,
            size,
        )
        .with_scan(Some(ScanRef {
            page: page.id,
            turned: 0,
            area: None,
        }))
        .with_reading(Some(Arc::new(reading)))
    }
}

impl Reporter for SortReporter<'_> {
    fn progress(&self, stage: Stage, done: usize, total: usize) {
        self.sink.send(Event::Progress { stage, done, total });
    }

    fn file_written(&self, group: &InstrumentGroup) {
        let undetected = group.key.instrument_name == grouping::UNKNOWN_NAME;
        self.sink.send(Event::FileWritten {
            file_name: group.output_filename.clone(),
            pages: group
                .pages
                .iter()
                .map(|page| self.page_ref(page, undetected))
                .collect(),
        });
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

fn sort(config: &SortConfig, pdfium: &Pdfium, sink: &EventSink, cancel: &AtomicBool) -> Result<()> {
    let excluded = pdf::normalize_path(&config.output_dir);
    let sizes = pdf::discover_pdfs(&config.input_dir, &excluded)?
        .into_iter()
        .filter_map(|path| {
            let sizes = pdf::page_sizes(&path, pdfium).ok()?;
            Some((path, sizes))
        })
        .collect();
    let reporter = SortReporter {
        sink,
        cancel,
        sizes,
        scan_dir: &config.input_dir,
    };
    let outcome = pipeline::sort(config, pdfium, &reporter)?;

    // Pages no part file holds: the blank ones.
    let in_files: HashSet<(&Path, usize)> = outcome
        .groups
        .iter()
        .flat_map(|group| &group.pages)
        .map(|page| (page.source_file.as_path(), page.source_page_index))
        .collect();
    let unused = outcome
        .pages
        .iter()
        .filter(|page| !in_files.contains(&(page.source_file.as_path(), page.source_page_index)))
        .map(|page| reporter.page_ref(page, false))
        .collect();
    sink.send(Event::SortFinished {
        unused,
        summary: SortSummary {
            other_pieces: outcome.piece_titles.iter().skip(1).cloned().collect(),
            skipped: outcome.skipped.clone(),
            file_count: outcome.written.len(),
            undetected_count: outcome.undetected_count,
            duplicated_count: outcome.duplicated_count,
            blank_count: outcome.blank_count,
        },
    });
    Ok(())
}

fn open_folder(dir: &Path, pdfium: &Pdfium) -> Result<Event> {
    let record = match record::read(dir) {
        Ok(record) => record,
        Err(err) => {
            error!("{err:#}");
            None
        }
    };
    let readings: Vec<Arc<record::ScanPage>> = record
        .as_ref()
        .map(|record| record.pages.iter().cloned().map(Arc::new).collect())
        .unwrap_or_default();
    let mut files = Vec::new();
    let mut unreadable = Vec::new();
    for path in pdf::list_folder_pdfs(dir)? {
        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match pdf::page_sizes(&path, pdfium) {
            Ok(sizes) => {
                let scans = record
                    .as_ref()
                    .and_then(|record| record.scan_refs(&file_name, sizes.len()));
                let source = Arc::new(path);
                let pages = sizes
                    .into_iter()
                    .enumerate()
                    .map(|(index, size)| {
                        let scan = scans.as_ref().and_then(|scans| scans[index]);
                        let reading = scan.and_then(|scan| readings.get(scan.page)).cloned();
                        PageRef::new(Arc::clone(&source), index, 0, size)
                            .with_scan(scan)
                            .with_reading(reading)
                    })
                    .collect();
                files.push((file_name, pages));
            }
            Err(err) => {
                error!("{err:#}");
                unreadable.push(file_name);
            }
        }
    }
    Ok(Event::Opened {
        dir: dir.to_path_buf(),
        files,
        unreadable,
    })
}

fn save(plan: &SavePlan, pdfium: &Pdfium) -> Event {
    let result = output::apply_save(plan, pdfium, &|paths| {
        trash::delete_all(paths).map_err(|err| anyhow!("{err}"))
    });
    match result {
        Ok(()) => {
            if let Err(err) = record::after_save(&plan.dir, &plan.files) {
                error!("Cannot write the corrections: {err:#}");
            }
            Event::Saved
        }
        Err(failure) => {
            error!("{}", failure.message);
            Event::SaveFailed {
                message: failure.message,
                folder_changed: failure.folder_changed,
            }
        }
    }
}
