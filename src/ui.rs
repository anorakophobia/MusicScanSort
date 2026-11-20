//! The window: the PDFs on the left, the pages of the open PDF on the right, the progress below.

use crate::grouping;
use crate::pdf;
use crate::pipeline::{SortConfig, Stage, is_run_folder, next_run_dir, run_folders};
use crate::record::{self, MarkKind, ScanPage};
use crate::session::{self, Crop, FileId, NameError, Origin, PageId, PageRef, Session, UNUSED};
use crate::worker::{self, Command, Event, PageKey, SortSummary, Worker};
use anyhow::{Result, anyhow};
use eframe::egui::{
    self, Align, Color32, CursorIcon, Key, Layout, Modifiers, Pos2, Rect, RichText, Role, Sense,
    TextureHandle, TextureOptions, Vec2, ViewportCommand, WidgetInfo, style::ScrollAnimation,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

const CAPTION_HEIGHT: f32 = 22.0;
const PAGE_GAP: f32 = 14.0;
const ROW_HEIGHT: f32 = 26.0;
const MIN_ZOOM: f32 = 0.3;
const MAX_ZOOM: f32 = 5.0;
/// Closer to the edge of a list or the page view than this, a drag scrolls it.
const DRAG_SCROLL_EDGE: f32 = 70.0;
const DRAG_SCROLL_SPEED: f32 = 12.0;
/// Each shown page takes about 10 MB on the graphics card.
const MAX_TEXTURES: usize = 40;

const SELECTED_COLOR: Color32 = Color32::from_rgb(40, 120, 220);
// Bright enough for the dark theme.
const WARNING_COLOR: Color32 = Color32::from_rgb(235, 150, 40);
const ERROR_COLOR: Color32 = Color32::from_rgb(240, 90, 90);
const TITLE_COLOR: Color32 = Color32::from_rgb(30, 90, 210);
const LABEL_COLOR: Color32 = Color32::from_rgb(20, 140, 60);
const PAGE_NUMBER_COLOR: Color32 = Color32::from_rgb(220, 110, 0);
const OTHER_TEXT_COLOR: Color32 = Color32::from_rgb(150, 150, 150);

pub enum Start {
    /// Show the folder of the settings, without analysis.
    ShowFolder,
    /// Sort the folder of the settings at once, if it holds PDFs.
    SortFolder,
}

pub fn run(config: SortConfig, start: Start, own_console: bool) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MusicScanSort")
            .with_maximized(true),
        ..Default::default()
    };
    eframe::run_native(
        "MusicScanSort",
        options,
        Box::new(move |creation| {
            // Only now: if the window cannot start, the console shows why.
            if own_console {
                close_own_console();
            }
            // Dark, whatever the system setting: the scans stay white on it.
            creation.egui_ctx.set_theme(egui::Theme::Dark);
            let worker = worker::spawn(creation.egui_ctx.clone());
            let mut app = App::new(worker, config);
            app.start(start);
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow!("Cannot open the window: {err}"))
}

#[cfg(windows)]
fn close_own_console() {
    unsafe extern "system" {
        fn FreeConsole() -> i32;
    }
    unsafe {
        FreeConsole();
    }
}

#[cfg(not(windows))]
fn close_own_console() {}

/// What runs on the PDF thread.
enum Job {
    Sorting {
        stage: Stage,
        done: usize,
        total: usize,
        started: Instant,
    },
    Opening,
    Saving,
}

enum Status {
    Plain(String),
    Error(String),
}

/// What to do once the changes are saved or dropped.
#[derive(Clone)]
enum Next {
    Close,
    /// Back to the start screen, with no folder open.
    CloseFolder,
    Open(PathBuf),
    Sort(PathBuf),
}

enum Dialog {
    Rename {
        file: FileId,
        name: String,
        error: Option<NameError>,
        /// The characters to mark when the dialog opens.
        select: Option<Range<usize>>,
    },
    NewFile {
        from: FileId,
        pages: HashSet<PageId>,
        name: String,
        error: Option<NameError>,
    },
    ConfirmDiscard,
    Unsaved(Next),
    StopRun,
    Split(SplitState),
}

/// A line that splits a page: across at a height, or down at a width, as a share of the page.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cut {
    down: bool,
    at: f32,
}

/// The split dialog: one page and the lines drawn on it (spec §14.5).
struct SplitState {
    file: FileId,
    page: PageId,
    cuts: Vec<Cut>,
    /// Where the line being drawn started, in window points.
    drawing: Option<Pos2>,
    /// The line being moved.
    moving: Option<usize>,
    /// The line that Entf deletes.
    chosen: Option<usize>,
}

impl SplitState {
    fn across_and_down(&self) -> (Vec<f32>, Vec<f32>) {
        let at = |down: bool| {
            self.cuts
                .iter()
                .filter(|cut| cut.down == down)
                .map(|cut| cut.at)
                .collect()
        };
        (at(false), at(true))
    }
}

/// How much ink each row and each column of a shown page holds, as a share of its pixels.
struct Ink {
    rows: Vec<f32>,
    columns: Vec<f32>,
}

impl Ink {
    fn of(image: &egui::ColorImage) -> Self {
        let [width, height] = image.size;
        let mut rows = vec![0.0; height];
        let mut columns = vec![0.0; width];
        for (at, pixel) in image.pixels.iter().enumerate() {
            let [r, g, b, _] = pixel.to_array();
            if u16::from(r) + u16::from(g) + u16::from(b) < 3 * 128 {
                rows[at / width] += 1.0;
                columns[at % width] += 1.0;
            }
        }
        rows.iter_mut().for_each(|row| *row /= width.max(1) as f32);
        columns
            .iter_mut()
            .for_each(|column| *column /= height.max(1) as f32);
        Self { rows, columns }
    }
}

/// A line through the widest white gap in the middle of the page, with ink on both sides: across
/// for an upright page, down for a page wider than high, which may be two pages side by side.
fn suggested_cut(ink: &Ink, aspect: f32) -> Option<Cut> {
    let down = aspect > 1.15;
    let profile = if down { &ink.columns } else { &ink.rows };
    let length = profile.len();
    if length < 20 {
        return None;
    }
    let (start, end) = if down {
        (length * 30 / 100, length * 70 / 100)
    } else {
        (length * 20 / 100, length * 80 / 100)
    };
    let inked = |value: &f32| *value >= 0.005;
    let mut best: Option<(usize, usize)> = None;
    let mut run_start = None;
    for at in start..=end {
        let white = at < end && !inked(&profile[at]);
        match (white, run_start) {
            (true, None) => run_start = Some(at),
            (false, Some(from)) => {
                if best.is_none_or(|(best_from, best_to)| at - from > best_to - best_from) {
                    best = Some((from, at));
                }
                run_start = None;
            }
            _ => {}
        }
    }
    let (from, to) = best?;
    let ink_before = profile[..from].iter().any(inked);
    let ink_after = profile[to..].iter().any(inked);
    let wide_enough = (to - from) as f32 >= 0.02 * length as f32;
    (ink_before && ink_after && wide_enough).then(|| Cut {
        down,
        at: (from + to) as f32 / 2.0 / length as f32,
    })
}

struct Drag {
    from: FileId,
    pages: HashSet<PageId>,
    pointer: Pos2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DropRow {
    File(FileId),
    NewFile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DropTarget {
    Row(DropRow),
    /// In the open file, in front of this page or at the end.
    InFile(Option<PageId>),
}

/// Where things are in this frame.
#[derive(Default)]
struct FrameLayout {
    rows: Vec<(DropRow, Rect)>,
    pages: Vec<(PageId, Rect)>,
    pages_area: Option<Rect>,
    /// Pages to show, nearest first.
    wanted: Vec<PageKey>,
}

impl FrameLayout {
    fn target_at(&self, pos: Pos2) -> Option<DropTarget> {
        if let Some((row, _)) = self.rows.iter().find(|(_, rect)| rect.contains(pos)) {
            return Some(DropTarget::Row(*row));
        }
        self.pages_area
            .filter(|area| area.contains(pos))
            .map(|_| DropTarget::InFile(page_after_gap(&self.pages, pos.x)))
    }
}

/// The page to land in front of when dropped at this height of the row of pages.
fn page_after_gap(pages: &[(PageId, Rect)], x: f32) -> Option<PageId> {
    pages
        .iter()
        .find(|(_, rect)| rect.center().x > x)
        .map(|(id, _)| *id)
}

/// The selection after a click on a page: plain selects it alone, Ctrl adds or removes it, Shift
/// selects from the last clicked page up to it.
fn click_selection(
    order: &[PageId],
    selected: &HashSet<PageId>,
    anchor: Option<PageId>,
    clicked: PageId,
    modifiers: Modifiers,
) -> (HashSet<PageId>, Option<PageId>) {
    if modifiers.shift {
        let position = |id| order.iter().position(|page| *page == id);
        if let (Some(from), Some(to)) = (anchor.and_then(position), position(clicked)) {
            let range = from.min(to)..=from.max(to);
            return (order[range].iter().copied().collect(), anchor);
        }
    }
    if modifiers.command {
        let mut selected = selected.clone();
        if !selected.remove(&clicked) {
            selected.insert(clicked);
        }
        return (selected, Some(clicked));
    }
    (HashSet::from([clicked]), Some(clicked))
}

/// Share of the whole run, by the rough time each stage takes.
fn overall_progress(stage: Stage, done: usize, total: usize) -> f32 {
    let (start, width) = match stage {
        Stage::Reading => (0.0, 0.10),
        Stage::Analysing => (0.10, 0.65),
        Stage::Pieces => (0.75, 0.10),
        Stage::Labels => (0.85, 0.10),
        Stage::Writing => (0.95, 0.05),
    };
    let part = if total == 0 {
        1.0
    } else {
        done as f32 / total as f32
    };
    start + width * part
}

fn stage_text(stage: Stage) -> &'static str {
    match stage {
        Stage::Reading => "Scans werden gelesen",
        Stage::Analysing => "Seiten werden erkannt",
        Stage::Pieces => "Stücke werden getrennt",
        Stage::Labels => "Unklare Stimmen werden nachgelesen",
        Stage::Writing => "PDFs werden geschrieben",
    }
}

fn pages_text(count: usize) -> String {
    if count == 1 {
        "1 Seite".to_owned()
    } else {
        format!("{count} Seiten")
    }
}

fn files_text(count: usize) -> String {
    if count == 1 {
        "1 Datei".to_owned()
    } else {
        format!("{count} Dateien")
    }
}

/// What the list shows for one file.
struct Row {
    id: FileId,
    name: String,
    heading: Option<String>,
    page_count: usize,
    changed: bool,
    /// The user should look through the file: `UNDETECTED` or `DUPLICATED` pages.
    to_check: bool,
}

pub struct App {
    worker: Worker,
    config: SortConfig,
    /// Where `--out` asked the first run of the settings' folder to write.
    out_for_first_run: Option<PathBuf>,
    /// The folder on show, or the one being sorted.
    folder: PathBuf,
    /// The folder the choice above the list starts from, which holds the `out-N` folders.
    home: PathBuf,
    /// The `out-N` folders of `home`, newest first.
    runs: Vec<PathBuf>,
    session: Option<Session>,
    job: Option<Job>,
    status: Status,
    open_file: Option<FileId>,
    selected: HashSet<PageId>,
    anchor: Option<PageId>,
    drag: Option<Drag>,
    zoom: f32,
    textures: HashMap<PageKey, (TextureHandle, u64)>,
    /// The ink of the shown pages, for the line the split dialog suggests.
    ink: HashMap<PageKey, Ink>,
    unshowable: HashSet<PageKey>,
    frame_number: u64,
    /// Rises when the pages behind the keys may have changed, so older renders don't count.
    generation: u64,
    requested: Vec<PageKey>,
    dialog: Option<Dialog>,
    scroll_list_to_open: bool,
    /// The file to open again once a saved folder is read back.
    reopen_file_name: Option<String>,
    after_save: Option<Next>,
    /// What to show once the folder is read again after a failure or a stopped run.
    status_after_reading: Option<Status>,
    may_close: bool,
    close_when_stopped: bool,
    worker_gone: bool,
    /// Frames around what the run read on the pages. On at every start; the choice is not kept.
    show_marks: bool,
    /// "Schließen" left no folder on show, so "Sortieren" has nothing to sort.
    folder_closed: bool,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

impl App {
    pub fn new(worker: Worker, config: SortConfig) -> Self {
        let out_for_first_run = (config.output_dir != next_run_dir(&config.input_dir))
            .then(|| config.output_dir.clone());
        let folder = config.input_dir.clone();
        Self {
            worker,
            config,
            out_for_first_run,
            home: folder.clone(),
            folder,
            runs: Vec::new(),
            session: None,
            job: None,
            status: Status::Plain(String::new()),
            open_file: None,
            selected: HashSet::new(),
            anchor: None,
            drag: None,
            zoom: 1.0,
            textures: HashMap::new(),
            ink: HashMap::new(),
            unshowable: HashSet::new(),
            frame_number: 0,
            generation: 0,
            requested: Vec::new(),
            dialog: None,
            scroll_list_to_open: false,
            reopen_file_name: None,
            after_save: None,
            status_after_reading: None,
            may_close: false,
            close_when_stopped: false,
            worker_gone: false,
            show_marks: true,
            folder_closed: false,
        }
    }

    pub fn start(&mut self, start: Start) {
        let folder = self.config.input_dir.clone();
        if matches!(start, Start::SortFolder) && has_pdfs(&folder, &self.config.output_dir) {
            let settings = self.sort_settings(&folder);
            self.begin_sort(settings);
        } else {
            self.begin_open(folder);
        }
    }

    /// The settings for sorting a folder. A title from the command line counts for its folder, and
    /// `--out` for one run, so that a second run does not write over the first.
    fn sort_settings(&mut self, folder: &Path) -> SortConfig {
        let mut settings = self.config.for_folder(folder);
        if folder == self.config.input_dir {
            settings.title.clone_from(&self.config.title);
            if let Some(out) = self.out_for_first_run.take() {
                settings.output_dir = out;
            }
        }
        settings
    }

    fn enter_folder(&mut self, folder: &Path) {
        self.folder_closed = false;
        self.folder = folder.to_path_buf();
        self.home = folder
            .parent()
            .filter(|_| is_run_folder(folder))
            .unwrap_or(folder)
            .to_path_buf();
        self.runs = run_folders(&self.home);
    }

    /// The folders the choice above the list offers, with their names there.
    fn places(&self) -> Vec<(PathBuf, String)> {
        let folder_name = |folder: &Path| {
            folder
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };
        let mut places = vec![(self.home.clone(), "Scans".to_owned())];
        for (position, run) in self.runs.iter().enumerate() {
            let name = if position == 0 {
                format!("{} (neuestes)", folder_name(run))
            } else {
                folder_name(run)
            };
            places.push((run.clone(), name));
        }
        // A run with --out writes somewhere else.
        if !places.iter().any(|(place, _)| *place == self.folder) {
            places.push((self.folder.clone(), folder_name(&self.folder)));
        }
        places
    }

    fn begin_sort(&mut self, config: SortConfig) {
        self.enter_folder(&config.input_dir);
        self.forget_pages();
        self.session = Some(Session::new(&config.output_dir, Origin::Sorted));
        self.job = Some(Job::Sorting {
            stage: Stage::Reading,
            done: 0,
            total: 0,
            started: Instant::now(),
        });
        self.status = Status::Plain(String::new());
        self.worker.cancel.store(false, Ordering::Relaxed);
        self.send(Command::Sort(config));
    }

    pub fn begin_open(&mut self, folder: PathBuf) {
        self.enter_folder(&folder);
        self.forget_pages();
        self.session = None;
        self.job = Some(Job::Opening);
        self.status = Status::Plain(String::new());
        self.send(Command::Open(folder));
    }

    /// After a run that did not finish: back to the folder it was sorting.
    fn show_folder_again(&mut self, status: Status) {
        self.status_after_reading = Some(status);
        self.begin_open(self.folder.clone());
    }

    fn forget_pages(&mut self) {
        self.generation += 1;
        self.textures.clear();
        self.ink.clear();
        self.unshowable.clear();
        self.requested.clear();
        self.open_file = None;
        self.clear_selection();
    }

    fn clear_selection(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.drag = None;
    }

    fn send(&mut self, command: Command) {
        let _ = self.worker.commands.send(command);
    }

    fn is_dirty(&self) -> bool {
        self.session.as_ref().is_some_and(Session::is_dirty)
    }

    fn can_edit(&self) -> bool {
        self.job.is_none() && self.session.is_some() && !self.worker_gone
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.frame_number += 1;
        self.handle_events(&ctx);
        self.keep_open_file_valid();
        if self.dialog.is_none() {
            self.handle_keys(&ctx);
        }

        let mut layout = FrameLayout::default();
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.session.is_some() {
            egui::Panel::left("files")
                .resizable(true)
                .default_size(360.0)
                .size_range(180.0..=900.0)
                .show(ui, |ui| self.file_list(ui, &mut layout));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if self.session.is_some() {
                self.pages_view(ui, &mut layout);
            } else if self.job.is_none() {
                self.welcome(ui);
            }
        });

        self.update_drag(&ctx, &layout);
        self.show_dialog(&ctx);
        self.handle_close(&ctx);
        self.request_renders(layout.wanted);
        if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        loop {
            match self.worker.events.try_recv() {
                Ok(event) => self.handle_event(ctx, event),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.job = None;
                    if !self.worker_gone {
                        self.worker_gone = true;
                        self.status = Status::Error(
                            "Interner Fehler: Die PDF-Verarbeitung ist abgestürzt. Bitte neu starten."
                                .to_owned(),
                        );
                    }
                    if std::mem::take(&mut self.close_when_stopped) {
                        self.perform(ctx, Next::Close);
                    }
                    return;
                }
            }
        }
    }

    fn handle_event(&mut self, ctx: &egui::Context, event: Event) {
        match event {
            Event::Progress { stage, done, total } => {
                if let Some(Job::Sorting {
                    stage: job_stage,
                    done: job_done,
                    total: job_total,
                    ..
                }) = &mut self.job
                {
                    (*job_stage, *job_done, *job_total) = (stage, done, total);
                }
            }
            Event::FileWritten { file_name, pages } => {
                if let Some(session) = &mut self.session {
                    session.add_file(file_name, pages);
                }
            }
            Event::SortFinished { unused, summary } => {
                if let Some(session) = &mut self.session {
                    session.add_unused(unused);
                    session.mark_saved();
                    self.folder.clone_from(&session.dir);
                }
                self.runs = run_folders(&self.home);
                self.job = None;
                self.status = Status::Plain(self.summary_text(&summary));
                self.open_first_file();
                if self.close_when_stopped {
                    self.perform(ctx, Next::Close);
                }
            }
            Event::SortCancelled => {
                self.job = None;
                let written = self
                    .session
                    .as_ref()
                    .filter(|session| !session.sorted_files().is_empty())
                    .map(|session| session.dir.clone());
                let status = Status::Plain(match written {
                    Some(dir) => format!(
                        "Abgebrochen. Der Ordner {} ist unvollständig.",
                        dir.display()
                    ),
                    None => "Abgebrochen.".to_owned(),
                });
                if self.close_when_stopped {
                    self.perform(ctx, Next::Close);
                } else {
                    self.show_folder_again(status);
                }
            }
            Event::Opened {
                dir,
                files,
                unreadable,
            } => self.show_opened_folder(dir, files, unreadable),
            Event::Rendered {
                generation,
                key,
                image,
            } => {
                if generation == self.generation {
                    match image {
                        Ok(image) => self.add_texture(ctx, key, image),
                        Err(err) => {
                            log::error!("Cannot show page: {err}");
                            self.unshowable.insert(key);
                        }
                    }
                }
            }
            Event::Saved => {
                self.requested.clear();
                self.handle_saved(ctx);
            }
            Event::SaveFailed {
                message,
                folder_changed,
            } => {
                self.job = None;
                self.after_save = None;
                self.requested.clear();
                if folder_changed {
                    // The session no longer describes the folder, so show what the folder holds.
                    self.status_after_reading = Some(Status::Error(message));
                    if let Some(session) = &self.session {
                        let dir = session.dir.clone();
                        self.job = Some(Job::Opening);
                        self.send(Command::Open(dir));
                    }
                } else {
                    self.status = Status::Error(message);
                }
            }
            Event::Failed(message) => {
                // What a run has written or a folder read so far is not a state to edit.
                let sorting = matches!(self.job, Some(Job::Sorting { .. }));
                let incomplete = self
                    .session
                    .as_ref()
                    .filter(|session| sorting && !session.sorted_files().is_empty())
                    .map(|session| session.dir.clone());
                if sorting || matches!(self.job, Some(Job::Opening)) {
                    self.forget_pages();
                    self.session = None;
                }
                self.job = None;
                self.after_save = None;
                self.reopen_file_name = None;
                self.status_after_reading = None;
                self.requested.clear();
                let status = Status::Error(match incomplete {
                    Some(dir) => {
                        format!("{message}. Der Ordner {} ist unvollständig.", dir.display())
                    }
                    None => message,
                });
                if std::mem::take(&mut self.close_when_stopped) {
                    self.perform(ctx, Next::Close);
                } else if sorting {
                    self.show_folder_again(status);
                } else {
                    self.status = status;
                }
            }
        }
    }

    fn summary_text(&self, summary: &SortSummary) -> String {
        let mut text = format!(
            "Fertig: {} geschrieben, {} nicht erkannt, {} leer",
            files_text(summary.file_count),
            pages_text(summary.undetected_count),
            pages_text(summary.blank_count),
        );
        if summary.duplicated_count > 0 {
            text.push_str(&format!(
                ", {} mehrfach gescannt",
                pages_text(summary.duplicated_count)
            ));
        }
        if !summary.skipped.is_empty() {
            text.push_str(&format!(", nicht lesbar: {}", summary.skipped.join(", ")));
        }
        if !summary.other_pieces.is_empty() {
            text.push_str(&format!(
                ", weitere Stücke: {}",
                summary.other_pieces.join(", ")
            ));
        }
        text
    }

    fn show_opened_folder(
        &mut self,
        dir: PathBuf,
        files: Vec<(String, Vec<PageRef>)>,
        unreadable: Vec<String>,
    ) {
        self.job = None;
        self.forget_pages();
        let mut session = Session::new(&dir, Origin::Opened);
        for (file_name, pages) in files {
            session.add_file(file_name, pages);
        }
        session.mark_saved();
        let count = session.sorted_files().len();
        let reopen = self.reopen_file_name.take();
        self.runs = run_folders(&self.home);
        self.status = if let Some(status) = self.status_after_reading.take() {
            status
        } else if !unreadable.is_empty() {
            Status::Error(format!(
                "Nicht lesbar und nicht angezeigt: {}",
                unreadable.join(", ")
            ))
        } else if reopen.is_some() {
            Status::Plain("Gespeichert. Alte Fassungen liegen im Papierkorb.".to_owned())
        } else if count == 0 {
            Status::Plain(format!("Keine PDFs in {}", dir.display()))
        } else {
            Status::Plain(format!("{} geöffnet", files_text(count)))
        };
        let wanted_again = reopen.and_then(|name| {
            session
                .sorted_files()
                .iter()
                .find(|file| file.file_name == name)
                .map(|file| file.id)
        });
        // Nothing here and nowhere to go from here: the start page takes over.
        let nothing_to_show = count == 0 && self.runs.is_empty() && !is_run_folder(&dir);
        self.session = (!nothing_to_show).then_some(session);
        match wanted_again {
            Some(id) => self.open_file = Some(id),
            None => self.open_first_file(),
        }
    }

    fn open_first_file(&mut self) {
        if self.open_file.is_none() {
            self.open_file = self
                .session
                .as_ref()
                .and_then(|session| session.sorted_files().first().map(|file| file.id));
            self.scroll_list_to_open = true;
        }
    }

    fn handle_saved(&mut self, ctx: &egui::Context) {
        self.job = None;
        let Some(session) = &mut self.session else {
            return;
        };
        if let Some(next) = self.after_save.take() {
            session.mark_saved();
            self.status = Status::Plain("Gespeichert.".to_owned());
            self.perform(ctx, next);
            return;
        }
        match session.origin {
            Origin::Sorted => {
                session.mark_saved();
                self.status =
                    Status::Plain("Gespeichert. Alte Fassungen liegen im Papierkorb.".to_owned());
            }
            // The pages now live in the new files, so read the folder again.
            Origin::Opened => {
                let dir = session.dir.clone();
                self.reopen_file_name = self
                    .open_file
                    .and_then(|id| session.file(id))
                    .map(|file| file.file_name.clone())
                    .or(Some(String::new()));
                self.job = Some(Job::Opening);
                self.send(Command::Open(dir));
            }
        }
    }

    fn add_texture(&mut self, ctx: &egui::Context, key: PageKey, image: egui::ColorImage) {
        let options = TextureOptions {
            mipmap_mode: Some(egui::TextureFilter::Linear),
            ..TextureOptions::LINEAR
        };
        self.ink.insert(key.clone(), Ink::of(&image));
        let texture = ctx.load_texture(
            format!("{}#{}", key.source.display(), key.index),
            image,
            options,
        );
        self.textures.insert(key, (texture, self.frame_number));
        while self.textures.len() > MAX_TEXTURES {
            let oldest = self
                .textures
                .iter()
                .min_by_key(|(_, (_, last_used))| *last_used)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else {
                break;
            };
            self.textures.remove(&oldest);
            self.ink.remove(&oldest);
        }
    }

    fn keep_open_file_valid(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        if let Some(open) = self.open_file
            && open != UNUSED
            && session.file(open).is_none()
        {
            self.open_file = None;
            self.selected.clear();
            self.open_first_file();
            return;
        }
        let pages = self.open_file.map_or(&[][..], |open| session.pages(open));
        self.selected
            .retain(|id| pages.iter().any(|page| page.id == *id));
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let pressed = |modifiers, key| ctx.input_mut(|input| input.consume_key(modifiers, key));
        if pressed(Modifiers::NONE, Key::M) {
            self.show_marks = !self.show_marks;
        }
        // A drag only ends with its drop or Esc, so that no save or dialog gets in between.
        if !self.can_edit() || self.drag.is_some() {
            return;
        }
        if pressed(Modifiers::COMMAND, Key::S) {
            self.save();
        }
        if pressed(Modifiers::COMMAND, Key::Z) {
            self.undo();
        }
        if pressed(Modifiers::COMMAND, Key::A) {
            self.select_all();
        }
        // Entf removes the marked pages; with no page marked it deletes the open file.
        if pressed(Modifiers::NONE, Key::Delete) {
            if self.selected.is_empty() {
                self.delete_open_file();
            } else {
                self.remove_selected();
            }
        }
        if pressed(Modifiers::NONE, Key::R) {
            self.rotate_selected();
        }
        if pressed(Modifiers::NONE, Key::T) {
            self.ask_split();
        }
        if pressed(Modifiers::COMMAND, Key::D) {
            self.duplicate_selected();
        }
        if pressed(Modifiers::NONE, Key::F2) {
            self.ask_rename();
        }
        if pressed(Modifiers::NONE, Key::ArrowUp) {
            self.open_neighbour(-1);
        }
        if pressed(Modifiers::NONE, Key::ArrowDown) {
            self.open_neighbour(1);
        }
        if self.drag.is_none() && pressed(Modifiers::NONE, Key::Escape) {
            self.clear_selection();
        }
    }

    fn open_neighbour(&mut self, step: isize) {
        let Some(session) = &self.session else {
            return;
        };
        let ids: Vec<FileId> = session.sorted_files().iter().map(|file| file.id).collect();
        let position = self
            .open_file
            .and_then(|open| ids.iter().position(|id| *id == open));
        let target = match position {
            Some(position) => position as isize + step,
            None => 0,
        };
        if let Some(id) = usize::try_from(target).ok().and_then(|at| ids.get(at)) {
            self.open(*id);
            self.scroll_list_to_open = true;
        }
    }

    fn open(&mut self, file: FileId) {
        if self.open_file != Some(file) {
            self.open_file = Some(file);
            self.clear_selection();
        }
    }

    fn select_all(&mut self) {
        if let (Some(session), Some(open)) = (&self.session, self.open_file) {
            self.selected = session.pages(open).iter().map(|page| page.id).collect();
        }
    }

    fn undo(&mut self) {
        if let Some(session) = &mut self.session {
            session.undo();
        }
    }

    fn remove_selected(&mut self) {
        if let (Some(session), Some(open)) = (&mut self.session, self.open_file) {
            session.append(open, &self.selected, UNUSED);
        }
    }

    /// Deletes the open file: its pages go to "Nicht verwendet", and saving moves the emptied
    /// file to the Recycle Bin. The next file in the list opens.
    fn delete_open_file(&mut self) {
        let (Some(session), Some(open)) = (&mut self.session, self.open_file) else {
            return;
        };
        if open == UNUSED {
            return;
        }
        let pages: HashSet<PageId> = session.pages(open).iter().map(|page| page.id).collect();
        if pages.is_empty() {
            return;
        }
        session.append(open, &pages, UNUSED);
        self.clear_selection();
        self.open_neighbour(1);
    }

    /// Copies the marked pages, each right behind itself, and marks the copies.
    fn duplicate_selected(&mut self) {
        if let (Some(session), Some(open)) = (&mut self.session, self.open_file)
            && open != UNUSED
        {
            let copies = session.duplicate(open, &self.selected);
            if !copies.is_empty() {
                self.anchor = copies.first().copied();
                self.selected = copies.into_iter().collect();
            }
        }
    }

    fn rotate_selected(&mut self) {
        if let (Some(session), Some(open)) = (&mut self.session, self.open_file) {
            session.rotate(open, &self.selected);
        }
    }

    /// Opens the split dialog for the one marked page, with a suggested line if the page shows a
    /// clear gap.
    fn ask_split(&mut self) {
        let (Some(session), Some(file)) = (&self.session, self.open_file) else {
            return;
        };
        if self.selected.len() != 1 {
            self.status = Status::Plain("Zum Teilen genau eine Seite markieren.".to_owned());
            return;
        }
        let Some(page) = session
            .pages(file)
            .iter()
            .find(|page| self.selected.contains(&page.id))
        else {
            return;
        };
        let suggestion = self
            .ink
            .get(&PageKey::from(page))
            .and_then(|ink| suggested_cut(ink, page.aspect()));
        self.dialog = Some(Dialog::Split(SplitState {
            file,
            page: page.id,
            cuts: suggestion.into_iter().collect(),
            drawing: None,
            moving: None,
            chosen: None,
        }));
    }

    fn ask_rename(&mut self) {
        if let (Some(session), Some(open)) = (&self.session, self.open_file)
            && let Some(file) = session.file(open)
        {
            self.dialog = Some(Dialog::Rename {
                file: open,
                name: file.name().to_owned(),
                error: None,
                select: rename_selection(file.name()),
            });
        }
    }

    fn ask_new_file(&mut self, from: FileId, pages: HashSet<PageId>) {
        let heading = self
            .session
            .as_ref()
            .and_then(|session| session.file(from))
            .and_then(|file| file.heading())
            .map(|heading| format!("{heading} - "));
        self.dialog = Some(Dialog::NewFile {
            from,
            pages,
            name: heading.unwrap_or_default(),
            error: None,
        });
    }

    fn save(&mut self) {
        if !self.can_edit() || !self.is_dirty() {
            return;
        }
        if let Some(session) = &self.session {
            let plan = session.save_plan();
            self.job = Some(Job::Saving);
            self.status = Status::Plain("Wird gespeichert…".to_owned());
            self.send(Command::Save(plan));
        }
    }

    /// Does what the user chose, asking first if changes would get lost.
    fn request(&mut self, ctx: &egui::Context, next: Next) {
        // Checked first: saving for a step that then cannot happen would leave an outdated session.
        if let Next::Sort(folder) = &next
            && !has_pdfs(folder, &next_run_dir(folder))
        {
            self.status = Status::Error(format!("Keine PDFs in {}", folder.display()));
            return;
        }
        if self.is_dirty() {
            self.dialog = Some(Dialog::Unsaved(next));
        } else {
            self.perform(ctx, next);
        }
    }

    fn perform(&mut self, ctx: &egui::Context, next: Next) {
        match next {
            Next::Close => {
                self.may_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            Next::CloseFolder => {
                self.forget_pages();
                self.session = None;
                self.folder_closed = true;
                self.status = Status::Plain(String::new());
            }
            Next::Open(folder) => self.begin_open(folder),
            Next::Sort(folder) => {
                let settings = self.sort_settings(&folder);
                self.begin_sort(settings);
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let idle = self.job.is_none() && !self.worker_gone;
            let open = ui
                .add_enabled(idle, egui::Button::new("Öffnen…"))
                .on_hover_text("Einen Ordner öffnen: Seine PDFs ansehen und bearbeiten, ohne Analyse");
            if open.clicked()
                && let Some(folder) = self.pick_folder("Ordner zum Bearbeiten öffnen")
            {
                self.request(ui.ctx(), Next::Open(folder));
            }
            let close = ui
                .add_enabled(idle && self.session.is_some(), egui::Button::new("Schließen"))
                .on_hover_text("Den Ordner schließen, ungespeicherte Änderungen werden vorher abgefragt");
            if close.clicked() {
                self.request(ui.ctx(), Next::CloseFolder);
            }
            let can_sort = idle && !self.folder_closed && !is_run_folder(&self.folder);
            let sort = ui
                .add_enabled(
                    can_sort,
                    egui::Button::new(RichText::new("Sortieren").color(Color32::WHITE))
                        .fill(SELECTED_COLOR),
                )
                .on_hover_text("Die PDFs dieses Ordners erkennen und in einen neuen out-Ordner sortieren")
                .on_disabled_hover_text("Dieser Ordner ist schon ein Ergebnis. Zum Sortieren links die Scans wählen.");
            if sort.clicked() {
                self.request(ui.ctx(), Next::Sort(self.folder.clone()));
            }
            ui.separator();

            let can_edit = self.can_edit();
            let dirty = self.is_dirty();
            let save = ui
                .add_enabled(can_edit && dirty, egui::Button::new("Speichern"))
                .on_hover_text("Alle Änderungen in den Ordner schreiben (Strg+S). Alte Fassungen gehen in den Papierkorb.");
            if save.clicked() {
                self.save();
            }
            let discard = ui
                .add_enabled(can_edit && dirty, egui::Button::new("Alles verwerfen"))
                .on_hover_text("Zurück zum zuletzt gespeicherten Stand");
            if discard.clicked() {
                self.dialog = Some(Dialog::ConfirmDiscard);
            }
            let can_undo = self.session.as_ref().is_some_and(Session::can_undo);
            let undo = ui
                .add_enabled(can_edit && can_undo, egui::Button::new("Rückgängig"))
                .on_hover_text("Letzte Änderung zurücknehmen (Strg+Z)");
            if undo.clicked() {
                self.undo();
            }
            ui.separator();

            // File actions.
            let file_open = can_edit && self.open_file.is_some_and(|open| open != UNUSED);
            let rename = ui
                .add_enabled(file_open, egui::Button::new("Umbenennen"))
                .on_hover_text("Das geöffnete PDF umbenennen (F2 oder Doppelklick in der Liste)");
            if rename.clicked() {
                self.ask_rename();
            }
            let has_pages = self
                .session
                .as_ref()
                .zip(self.open_file)
                .is_some_and(|(session, open)| !session.pages(open).is_empty());
            let delete = ui
                .add_enabled(file_open && has_pages, egui::Button::new("Löschen"))
                .on_hover_text(
                    "Das geöffnete PDF löschen: Seine Seiten gehen nach „Nicht verwendet“, die Datei beim Speichern in den Papierkorb (Entf, wenn keine Seite markiert ist)",
                );
            if delete.clicked() {
                self.delete_open_file();
            }
            ui.separator();

            // Page actions.
            let has_selection = !self.selected.is_empty();
            let rotate = ui
                .add_enabled(can_edit && has_selection, egui::Button::new("Drehen"))
                .on_hover_text("Markierte Seiten um 90° im Uhrzeigersinn drehen (R)");
            if rotate.clicked() {
                self.rotate_selected();
            }
            let split = ui
                .add_enabled(can_edit && self.selected.len() == 1, egui::Button::new("Teilen"))
                .on_hover_text("Die markierte Seite an Linien in mehrere Seiten teilen (T)")
                .on_disabled_hover_text("Genau eine Seite markieren");
            if split.clicked() {
                self.ask_split();
            }
            let can_duplicate = can_edit && has_selection && self.open_file != Some(UNUSED);
            let duplicate = ui
                .add_enabled(can_duplicate, egui::Button::new("Duplizieren"))
                .on_hover_text("Markierte Seiten kopieren, die Kopie steht direkt dahinter (Strg+D)");
            if duplicate.clicked() {
                self.duplicate_selected();
            }
            let can_remove = can_edit && has_selection && self.open_file != Some(UNUSED);
            let remove = ui
                .add_enabled(can_remove, egui::Button::new("Entfernen"))
                .on_hover_text("Markierte Seiten nach „Nicht verwendet“ legen (Entf)");
            if remove.clicked() {
                self.remove_selected();
            }
            ui.separator();
            ui.toggle_value(&mut self.show_marks, "Markierungen")
                .on_hover_text("Rahmen um Titel, Instrument und Seitenzahl, wie sie gelesen wurden (M)");

            let shown_dir = self
                .session
                .as_ref()
                .map_or(&self.folder, |session| &session.dir);
            if !self.folder_closed {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(egui::Label::new(shown_dir.display().to_string()).truncate());
                });
            }
        });
        ui.add_space(4.0);
    }

    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title(title)
            .set_directory(&self.folder)
            .pick_folder()
            .map(|folder| pdf::normalize_path(&folder))
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(3.0);
        ui.horizontal(|ui| match &self.job {
            Some(Job::Sorting {
                stage,
                done,
                total,
                started,
            }) => {
                let share = overall_progress(*stage, *done, *total);
                let elapsed = started.elapsed().as_secs();
                let text = format!(
                    "{}: {done} von {total} · {:.0} % · {}:{:02}",
                    stage_text(*stage),
                    share * 100.0,
                    elapsed / 60,
                    elapsed % 60,
                );
                let cancelling = self.worker.cancel.load(Ordering::Relaxed);
                if ui
                    .add_enabled(!cancelling, egui::Button::new("Abbrechen"))
                    .clicked()
                {
                    self.worker.cancel.store(true, Ordering::Relaxed);
                }
                ui.add(egui::ProgressBar::new(share).text(text));
            }
            Some(Job::Opening) => {
                ui.spinner();
                ui.label("Ordner wird gelesen…");
            }
            Some(Job::Saving) => {
                ui.spinner();
                ui.label("Wird gespeichert…");
            }
            None => {
                match &self.status {
                    Status::Plain(text) => {
                        ui.label(text);
                    }
                    Status::Error(text) => {
                        ui.label(RichText::new(text).color(ERROR_COLOR));
                    }
                }
                if let Some(session) = &self.session {
                    let changed = session.changed_count();
                    if changed > 0 {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} geändert, nicht gespeichert",
                                    files_text(changed)
                                ))
                                .color(WARNING_COLOR),
                            );
                        });
                    }
                }
            }
        });
        ui.add_space(3.0);
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 3.0);
            ui.heading("MusicScanSort");
            ui.add_space(16.0);
            if ui
                .add(egui::Button::new("Ordner mit PDFs öffnen…").min_size(Vec2::new(280.0, 36.0)))
                .clicked()
                && let Some(folder) = self.pick_folder("Ordner zum Bearbeiten öffnen")
            {
                self.request(ui.ctx(), Next::Open(folder));
            }
        });
    }

    /// Moves between a folder of scans and its results. Hidden while there is only one.
    fn folder_choice(&mut self, ui: &mut egui::Ui) {
        let places = self.places();
        if places.len() < 2 {
            return;
        }
        let current = places
            .iter()
            .find(|(place, _)| *place == self.folder)
            .map_or_else(String::new, |(_, name)| name.clone());
        let mut chosen = None;
        egui::Panel::top("folder_choice")
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.add_enabled_ui(self.job.is_none() && !self.worker_gone, |ui| {
                    ui.horizontal(|ui| {
                        let label = ui.label("Ordner:");
                        egui::ComboBox::from_id_salt("folder_choice_box")
                            .selected_text(current)
                            .width(ui.available_width() - 8.0)
                            .show_ui(ui, |ui| {
                                for (place, name) in &places {
                                    let is_current = *place == self.folder;
                                    let row = ui.selectable_label(is_current, name);
                                    if row.clicked() && !is_current {
                                        chosen = Some(place.clone());
                                    }
                                }
                            })
                            .response
                            .labelled_by(label.id);
                    });
                });
                ui.add_space(4.0);
            });
        if let Some(folder) = chosen {
            self.request(ui.ctx(), Next::Open(folder));
        }
    }

    fn file_rows(&self) -> Vec<Row> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        session
            .sorted_files()
            .iter()
            .map(|file| Row {
                id: file.id,
                name: file.name().to_owned(),
                heading: file.heading().map(str::to_owned),
                page_count: file.pages.len(),
                changed: session.is_changed(file.id),
                to_check: file.file_name.contains("UNDETECTED")
                    || file.file_name.contains(grouping::DUPLICATED_NAME),
            })
            .collect()
    }

    fn file_list(&mut self, ui: &mut egui::Ui, layout: &mut FrameLayout) {
        self.folder_choice(ui);
        let rows = self.file_rows();
        let unused_count = self
            .session
            .as_ref()
            .map_or(0, |session| session.pages(UNUSED).len());
        let can_edit = self.can_edit();
        let mut opened = None;
        let mut renamed = None;
        let mut new_file_clicked = false;

        egui::Panel::bottom("file_list_end")
            .show_separator_line(true)
            .show(ui, |ui| {
                if can_edit {
                    let response = self.list_row(
                        ui,
                        RowLook {
                            text: "+ Neues PDF (Seiten hierher ziehen)".to_owned(),
                            tail: String::new(),
                            open: false,
                            color: None,
                        },
                    );
                    layout.rows.push((DropRow::NewFile, response.rect));
                    new_file_clicked = response.clicked();
                }
                // Also when empty, so that the rows don't move when a drag starts.
                if can_edit || unused_count > 0 {
                    let response = self.list_row(
                        ui,
                        RowLook {
                            text: "Nicht verwendet".to_owned(),
                            tail: unused_count.to_string(),
                            open: self.open_file == Some(UNUSED),
                            color: None,
                        },
                    );
                    layout.rows.push((DropRow::File(UNUSED), response.rect));
                    if response.clicked() {
                        opened = Some(UNUSED);
                    }
                }
            });

        let several_headings = rows.iter().any(|row| row.heading != rows[0].heading);
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    self.scroll_while_dragging(ui, Vec2::Y);
                    let mut last_heading: Option<&str> = None;
                    for row in &rows {
                        let heading = row.heading.as_deref();
                        if several_headings && heading.is_some() && heading != last_heading {
                            ui.add_space(6.0);
                            ui.label(RichText::new(heading.unwrap_or_default()).strong());
                        }
                        last_heading = heading;
                        // With a heading above, the row need not repeat the piece.
                        let text = match heading {
                            Some(heading) if several_headings => {
                                row.name[heading.len() + 3..].to_owned()
                            }
                            _ => row.name.clone(),
                        };
                        let response = self.list_row(
                            ui,
                            RowLook {
                                text: if row.changed {
                                    format!("{text} *")
                                } else {
                                    text
                                },
                                tail: row.page_count.to_string(),
                                open: self.open_file == Some(row.id),
                                color: if row.to_check {
                                    Some(WARNING_COLOR)
                                } else if row.page_count == 0 {
                                    Some(ui.visuals().weak_text_color())
                                } else {
                                    None
                                },
                            },
                        );
                        let response = response.on_hover_text(&row.name);
                        // Only the visible part: a row scrolled away is no place to drop on.
                        let visible = response.rect.intersect(ui.clip_rect());
                        layout.rows.push((DropRow::File(row.id), visible));
                        if response.clicked() {
                            opened = Some(row.id);
                        }
                        if response.double_clicked() {
                            renamed = Some(row.id);
                        }
                        if self.scroll_list_to_open && self.open_file == Some(row.id) {
                            response.scroll_to_me(None);
                            self.scroll_list_to_open = false;
                        }
                    }
                });
        });

        if let Some(file) = opened {
            self.open(file);
        }
        if let Some(file) = renamed
            && can_edit
        {
            self.open(file);
            self.ask_rename();
        }
        if new_file_clicked {
            if !self.selected.is_empty()
                && let Some(open) = self.open_file
            {
                self.ask_new_file(open, self.selected.clone());
            } else {
                self.status =
                    Status::Plain("Seiten markieren und auf „Neues PDF“ ziehen.".to_owned());
            }
        }
    }

    fn list_row(&self, ui: &mut egui::Ui, look: RowLook) -> egui::Response {
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &look.text));
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let is_drop_target = self
            .drag
            .as_ref()
            .is_some_and(|drag| rect.contains(drag.pointer));
        let visuals = ui.visuals();
        let fill = if is_drop_target {
            Some(SELECTED_COLOR.gamma_multiply(0.6))
        } else if look.open {
            Some(visuals.selection.bg_fill)
        } else if response.hovered() {
            Some(visuals.widgets.hovered.weak_bg_fill)
        } else {
            None
        };
        if let Some(fill) = fill {
            ui.painter().rect_filled(rect, 3.0, fill);
        }
        let text_color = look.color.unwrap_or(if look.open {
            visuals.selection.stroke.color
        } else {
            visuals.text_color()
        });
        let font = egui::TextStyle::Body.resolve(ui.style());
        let tail_galley = ui
            .painter()
            .layout_no_wrap(look.tail, font.clone(), text_color);
        let tail_width = tail_galley.size().x;
        let mut job = egui::text::LayoutJob::simple_singleline(look.text, font, text_color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(width - tail_width - 22.0);
        let galley = ui.painter().layout_job(job);
        let middle = rect.center().y;
        ui.painter().galley(
            Pos2::new(rect.left() + 6.0, middle - galley.size().y / 2.0),
            galley,
            text_color,
        );
        let tail_y = middle - tail_galley.size().y / 2.0;
        ui.painter().galley(
            Pos2::new(rect.right() - tail_width - 6.0, tail_y),
            tail_galley,
            text_color,
        );
        response
    }

    /// Scrolls the area around `ui` along `axis` while a drag is close to its edge.
    fn scroll_while_dragging(&self, ui: &egui::Ui, axis: Vec2) {
        let Some(drag) = &self.drag else {
            return;
        };
        let clip = ui.clip_rect();
        if !clip.contains(drag.pointer) {
            return;
        }
        let (position, start, end) = if axis.x > 0.0 {
            (drag.pointer.x, clip.left(), clip.right())
        } else {
            (drag.pointer.y, clip.top(), clip.bottom())
        };
        let direction = if position < start + DRAG_SCROLL_EDGE {
            1.0
        } else if position > end - DRAG_SCROLL_EDGE {
            -1.0
        } else {
            return;
        };
        ui.scroll_with_delta_animation(
            axis * direction * DRAG_SCROLL_SPEED,
            ScrollAnimation::none(),
        );
        ui.ctx().request_repaint();
    }

    fn pages_view(&mut self, ui: &mut egui::Ui, layout: &mut FrameLayout) {
        let Some(open) = self.open_file else {
            let no_files = self
                .session
                .as_ref()
                .is_some_and(|session| session.sorted_files().is_empty());
            let hint = if self.job.is_none() && no_files {
                "In diesem Ordner liegen keine PDFs."
            } else if self.job.is_none() {
                "Links ein PDF auswählen."
            } else {
                "Die PDFs erscheinen links, sobald alle Seiten gelesen sind."
            };
            ui.centered_and_justified(|ui| ui.label(hint));
            return;
        };
        let pages: Vec<PageRef> = self
            .session
            .as_ref()
            .map(|session| session.pages(open).to_vec())
            .unwrap_or_default();
        let area = ui.available_rect_before_wrap();
        layout.pages_area = Some(area);
        if pages.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(if open == UNUSED {
                    "Keine Seiten."
                } else {
                    "Keine Seiten mehr. Die Datei wird beim Speichern entfernt."
                })
            });
            return;
        }

        let background = ui.interact(area, egui::Id::unique("pages_background"), Sense::click());
        if background.clicked() {
            self.selected.clear();
            self.anchor = None;
        }
        let pointer_over = ui.rect_contains_pointer(area);
        let wheel = if pointer_over {
            self.zoom =
                (self.zoom * ui.input(|input| input.zoom_delta())).clamp(MIN_ZOOM, MAX_ZOOM);
            ui.input(|input| input.smooth_scroll_delta())
        } else {
            Vec2::ZERO
        };

        let order: Vec<PageId> = pages.iter().map(|page| page.id).collect();
        let can_edit = self.can_edit();
        let mut clicked = None;
        let mut drag_started = None;
        let row_height = ((area.height() - CAPTION_HEIGHT - 28.0) * self.zoom).max(60.0);
        egui::ScrollArea::both()
            .id_salt(("pages", open))
            .auto_shrink(false)
            .scroll_source(egui::scroll_area::ScrollSource::SCROLL_BAR)
            .show(ui, |ui| {
                // The wheel moves along the row of pages, whichever way it turns.
                ui.scroll_with_delta_animation(
                    Vec2::new(wheel.x + wheel.y, 0.0),
                    ScrollAnimation::none(),
                );
                self.scroll_while_dragging(ui, Vec2::X);
                ui.spacing_mut().item_spacing.x = PAGE_GAP;
                let clip = ui.clip_rect();
                let mut wanted: Vec<(f32, PageKey)> = Vec::new();
                ui.horizontal_top(|ui| {
                    for (position, page) in pages.iter().enumerate() {
                        let size =
                            Vec2::new(row_height * page.aspect(), row_height + CAPTION_HEIGHT);
                        let sense = if can_edit {
                            Sense::click_and_drag()
                        } else {
                            Sense::hover()
                        };
                        let (rect, response) = ui.allocate_exact_size(size, sense);
                        response.widget_info(|| {
                            WidgetInfo::labeled(
                                Role::Button,
                                true,
                                format!("Seite {}", position + 1),
                            )
                        });
                        layout.pages.push((page.id, rect));
                        if response.clicked() {
                            clicked = Some(page.id);
                        }
                        if response.drag_started() {
                            drag_started = Some(page.id);
                        }
                        let key = PageKey::from(page);
                        let distance = (rect.center().x - clip.center().x).abs();
                        let near = distance < rect.width() / 2.0 + clip.width();
                        if near
                            && !self.textures.contains_key(&key)
                            && !self.unshowable.contains(&key)
                        {
                            wanted.push((distance, key.clone()));
                        }
                        if ui.is_rect_visible(rect) {
                            self.paint_page(ui, rect, page, position, &key);
                        }
                        if self.show_marks
                            && let Some(reading) = &page.reading
                        {
                            response.on_hover_text(record::page_summary(reading));
                        }
                    }
                });
                wanted.sort_by(|a, b| a.0.total_cmp(&b.0));
                layout.wanted = wanted.into_iter().map(|(_, key)| key).collect();
            });

        if let Some(page) = clicked {
            let modifiers = ui.input(|input| input.modifiers);
            (self.selected, self.anchor) =
                click_selection(&order, &self.selected, self.anchor, page, modifiers);
        }
        if let Some(page) = drag_started {
            self.start_drag(ui, open, page);
        }
    }

    fn start_drag(&mut self, ui: &egui::Ui, open: FileId, page: PageId) {
        if !self.selected.contains(&page) {
            if ui.input(|input| input.modifiers.command) {
                self.selected.insert(page);
            } else {
                self.selected = HashSet::from([page]);
            }
            self.anchor = Some(page);
        }
        let pointer = ui.input(|input| input.pointer.latest_pos());
        self.drag = Some(Drag {
            from: open,
            pages: self.selected.clone(),
            pointer: pointer.unwrap_or_default(),
        });
    }

    fn paint_page(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        page: &PageRef,
        position: usize,
        key: &PageKey,
    ) {
        let image_rect =
            Rect::from_min_max(rect.min, Pos2::new(rect.max.x, rect.max.y - CAPTION_HEIGHT));
        let painter = ui.painter();
        painter.rect_filled(image_rect, 0.0, Color32::WHITE);
        let frame_number = self.frame_number;
        if let Some((texture, last_used)) = self.textures.get_mut(key) {
            *last_used = frame_number;
            let whole = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            painter.image(texture.id(), image_rect, whole, Color32::WHITE);
            if self.show_marks {
                paint_marks(ui, image_rect, page);
            }
        } else {
            let text = if self.unshowable.contains(key) {
                "Seite kann nicht angezeigt werden"
            } else {
                "Lädt…"
            };
            painter.text(
                image_rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::TextStyle::Body.resolve(ui.style()),
                Color32::DARK_GRAY,
            );
        }
        if self
            .drag
            .as_ref()
            .is_some_and(|drag| drag.pages.contains(&page.id))
        {
            painter.rect_filled(image_rect, 0.0, Color32::from_white_alpha(170));
        }
        let selected = self.selected.contains(&page.id);
        if selected {
            painter.rect_filled(image_rect, 0.0, SELECTED_COLOR.gamma_multiply(0.15));
        }
        let stroke = if selected {
            egui::Stroke::new(3.0, SELECTED_COLOR)
        } else {
            egui::Stroke::new(1.0, ui.visuals().weak_text_color())
        };
        painter.rect_stroke(image_rect, 0.0, stroke, egui::StrokeKind::Inside);

        let source_name = source_name(&page.source);
        let part = if page.crop.is_some() { " (Teil)" } else { "" };
        let caption = format!(
            "{} · {source_name}, S. {}{part}",
            position + 1,
            page.index + 1
        );
        let font = egui::TextStyle::Small.resolve(ui.style());
        let color = ui.visuals().text_color();
        let mut job = egui::text::LayoutJob::simple_singleline(caption, font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width());
        let galley = painter.layout_job(job);
        painter.galley(
            Pos2::new(rect.left(), rect.bottom() - CAPTION_HEIGHT + 4.0),
            galley,
            color,
        );
    }

    /// Follows the pointer during a drag, shows where the pages would land and drops them.
    fn update_drag(&mut self, ctx: &egui::Context, layout: &FrameLayout) {
        // Closing the window can open a dialog during a drag. The drag ends there.
        if self.dialog.is_some() || !self.can_edit() {
            self.drag = None;
        }
        let Some(drag) = &mut self.drag else {
            return;
        };
        if let Some(pointer) = ctx.input(|input| input.pointer.latest_pos()) {
            drag.pointer = pointer;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
            self.drag = None;
            return;
        }
        let pointer = drag.pointer;
        let count = drag.pages.len();
        let from = drag.from;
        let target = layout.target_at(pointer);
        ctx.set_cursor_icon(CursorIcon::Grabbing);
        paint_drag(ctx, layout, pointer, count, target);
        if !ctx.input(|input| input.pointer.any_released()) {
            return;
        }
        let Some(drag) = self.drag.take() else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        match target {
            Some(DropTarget::InFile(before)) => session.reorder(from, &drag.pages, before),
            Some(DropTarget::Row(DropRow::File(to))) => {
                session.append(from, &drag.pages, to);
            }
            Some(DropTarget::Row(DropRow::NewFile)) => self.ask_new_file(from, drag.pages),
            None => {}
        }
    }

    fn show_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let keep_open = match &mut dialog {
            Dialog::Rename {
                file,
                name,
                error,
                select,
            } => {
                let file = *file;
                let answer = name_dialog(ctx, "Datei umbenennen", name, error.as_ref(), select);
                match answer {
                    Answer::Pending => true,
                    Answer::No => false,
                    Answer::Yes => {
                        let result = self
                            .session
                            .as_mut()
                            .map_or(Ok(()), |session| session.rename(file, name));
                        match result {
                            Ok(()) => false,
                            Err(err) => {
                                *error = Some(err);
                                true
                            }
                        }
                    }
                }
            }
            Dialog::NewFile {
                from,
                pages,
                name,
                error,
            } => {
                let answer = name_dialog(ctx, "Neues PDF", name, error.as_ref(), &mut None);
                match answer {
                    Answer::Pending => true,
                    Answer::No => false,
                    Answer::Yes => {
                        let result = self
                            .session
                            .as_mut()
                            .map(|session| session.move_to_new_file(*from, pages, name));
                        match result {
                            Some(Err(err)) => {
                                *error = Some(err);
                                true
                            }
                            _ => false,
                        }
                    }
                }
            }
            Dialog::ConfirmDiscard => {
                let count = self.session.as_ref().map_or(0, Session::changed_count);
                let text = format!(
                    "Änderungen an {} verwerfen und zum zuletzt gespeicherten Stand zurückgehen?",
                    files_text(count)
                );
                match yes_no_dialog(ctx, &text, "Verwerfen", "Abbrechen") {
                    Answer::Pending => true,
                    Answer::No => false,
                    Answer::Yes => {
                        if let Some(session) = &mut self.session {
                            session.discard();
                        }
                        false
                    }
                }
            }
            Dialog::Unsaved(next) => {
                let count = self.session.as_ref().map_or(0, Session::changed_count);
                let text = format!("{} geändert. Speichern?", files_text(count));
                match unsaved_dialog(ctx, &text) {
                    UnsavedAnswer::Pending => true,
                    UnsavedAnswer::Cancel => false,
                    UnsavedAnswer::Save => {
                        self.after_save = Some(next.clone());
                        self.save();
                        false
                    }
                    UnsavedAnswer::Drop => {
                        self.perform(ctx, next.clone());
                        false
                    }
                }
            }
            Dialog::Split(state) => match self.split_dialog(ctx, state) {
                Answer::Pending => true,
                Answer::No => false,
                Answer::Yes => {
                    let (across, down) = state.across_and_down();
                    if let Some(session) = &mut self.session {
                        let parts = session.split(state.file, state.page, &across, &down);
                        if !parts.is_empty() {
                            self.anchor = parts.first().copied();
                            self.selected = parts.into_iter().collect();
                        }
                    }
                    false
                }
            },
            Dialog::StopRun => {
                let text = "Die Verarbeitung läuft noch. Abbrechen und schließen?";
                match yes_no_dialog(ctx, text, "Abbrechen und schließen", "Weiterlaufen") {
                    Answer::Pending => true,
                    Answer::No => false,
                    Answer::Yes => {
                        if matches!(self.job, Some(Job::Sorting { .. })) {
                            self.close_when_stopped = true;
                            self.worker.cancel.store(true, Ordering::Relaxed);
                        } else {
                            // The run ended while the question was open.
                            self.request(ctx, Next::Close);
                        }
                        false
                    }
                }
            }
        };
        if keep_open && self.dialog.is_none() {
            self.dialog = Some(dialog);
        }
    }

    fn split_dialog(&mut self, ctx: &egui::Context, state: &mut SplitState) -> Answer {
        let Some(page) = self
            .session
            .as_ref()
            .and_then(|session| {
                session
                    .pages(state.file)
                    .iter()
                    .find(|page| page.id == state.page)
            })
            .cloned()
        else {
            return Answer::No;
        };
        let texture = self
            .textures
            .get(&PageKey::from(&page))
            .map(|(texture, _)| texture.id());
        let screen = ctx.content_rect();
        let modal = egui::Modal::new(egui::Id::unique("split_dialog")).show(ctx, |ui| {
            ui.heading("Seite teilen");
            ui.label(
                "Eine Linie über die Seite ziehen, waagrecht oder senkrecht. \
                 Eine Linie anklicken und Entf löscht sie, ziehen verschiebt sie.",
            );
            ui.add_space(6.0);
            let room = Vec2::new(screen.width() * 0.8, screen.height() * 0.72)
                .max(Vec2::splat(100.0));
            let aspect = page.aspect();
            let size = if room.x / room.y > aspect {
                Vec2::new(room.y * aspect, room.y)
            } else {
                Vec2::new(room.x, room.x / aspect)
            };
            let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
            response.widget_info(|| WidgetInfo::labeled(Role::Image, true, "Seite zum Teilen"));
            edit_cuts(ui, rect, &response, state);
            paint_split(ui, rect, texture, state);
            ui.add_space(6.0);
            let mut answer = Answer::Pending;
            ui.horizontal(|ui| {
                let can_split = !state.cuts.is_empty();
                if ui.add_enabled(can_split, egui::Button::new("Teilen")).clicked()
                    || (can_split && ui.input(|input| input.key_pressed(Key::Enter)))
                {
                    answer = Answer::Yes;
                }
                if ui.button("Abbrechen").clicked() {
                    answer = Answer::No;
                }
                if ui
                    .add_enabled(state.chosen.is_some(), egui::Button::new("Linie löschen"))
                    .clicked()
                {
                    delete_chosen_cut(state);
                }
            });
            answer
        });
        if modal.should_close() {
            return Answer::No;
        }
        modal.inner
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if self.may_close || !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        match &self.job {
            Some(Job::Sorting { .. }) => {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.dialog = Some(Dialog::StopRun);
            }
            Some(_) => ctx.send_viewport_cmd(ViewportCommand::CancelClose),
            None if self.is_dirty() && !self.worker_gone => {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.dialog = Some(Dialog::Unsaved(Next::Close));
            }
            None => {}
        }
    }

    fn request_renders(&mut self, wanted: Vec<PageKey>) {
        if wanted != self.requested && !matches!(self.job, Some(Job::Saving | Job::Opening)) {
            self.requested.clone_from(&wanted);
            self.send(Command::Render(self.generation, wanted));
        }
    }
}

/// Pixels from a line within which a click or drag takes that line.
const CUT_GRIP: f32 = 8.0;
/// Shortest drag that draws a line.
const MIN_CUT_DRAG: f32 = 12.0;

fn cut_at(cut: Cut, rect: Rect) -> f32 {
    if cut.down {
        rect.left() + cut.at * rect.width()
    } else {
        rect.top() + cut.at * rect.height()
    }
}

/// The line under the pointer, if any.
fn cut_near(cuts: &[Cut], rect: Rect, pos: Pos2) -> Option<usize> {
    cuts.iter()
        .enumerate()
        .map(|(index, cut)| {
            let along = if cut.down { pos.x } else { pos.y };
            (index, (along - cut_at(*cut, rect)).abs())
        })
        .filter(|(_, distance)| *distance <= CUT_GRIP)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// The line a drag from `from` to `to` draws: in the direction it goes further, through its start.
fn drawn_cut(from: Pos2, to: Pos2, rect: Rect) -> Option<Cut> {
    let delta = to - from;
    if delta.x.abs().max(delta.y.abs()) < MIN_CUT_DRAG {
        return None;
    }
    let share = |value: f32, start: f32, length: f32| ((value - start) / length).clamp(0.0, 1.0);
    Some(if delta.x.abs() >= delta.y.abs() {
        Cut {
            down: false,
            at: share(from.y, rect.top(), rect.height()),
        }
    } else {
        Cut {
            down: true,
            at: share(from.x, rect.left(), rect.width()),
        }
    })
}

fn delete_chosen_cut(state: &mut SplitState) {
    if let Some(chosen) = state.chosen.take()
        && chosen < state.cuts.len()
    {
        state.cuts.remove(chosen);
    }
}

/// Drawing, moving, choosing and deleting lines with the mouse and Entf.
fn edit_cuts(ui: &egui::Ui, rect: Rect, response: &egui::Response, state: &mut SplitState) {
    let pointer = ui.input(|input| input.pointer.latest_pos());
    if response.drag_started()
        && let Some(origin) = ui.input(|input| input.pointer.press_origin())
    {
        match cut_near(&state.cuts, rect, origin) {
            Some(index) => {
                state.moving = Some(index);
                state.chosen = Some(index);
            }
            None => state.drawing = Some(origin),
        }
    }
    if let (Some(index), Some(pos)) = (state.moving, pointer)
        && let Some(cut) = state.cuts.get_mut(index)
    {
        cut.at = if cut.down {
            (pos.x - rect.left()) / rect.width()
        } else {
            (pos.y - rect.top()) / rect.height()
        }
        .clamp(0.0, 1.0);
    }
    if response.drag_stopped() {
        if let (Some(from), Some(to)) = (state.drawing, pointer)
            && let Some(cut) = drawn_cut(from, to, rect)
        {
            state.cuts.push(cut);
            state.chosen = Some(state.cuts.len() - 1);
        }
        state.drawing = None;
        state.moving = None;
    }
    if response.clicked() {
        state.chosen = pointer.and_then(|pos| cut_near(&state.cuts, rect, pos));
    }
    if ui.input_mut(|input| {
        input.consume_key(Modifiers::NONE, Key::Delete)
            || input.consume_key(Modifiers::NONE, Key::Backspace)
    }) {
        delete_chosen_cut(state);
    }
}

/// The page with its parts tinted and numbered, the lines, and the line being drawn.
fn paint_split(ui: &egui::Ui, rect: Rect, texture: Option<egui::TextureId>, state: &SplitState) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::WHITE);
    match texture {
        Some(texture) => {
            let whole = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            painter.image(texture, rect, whole, Color32::WHITE);
        }
        None => {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Lädt…",
                egui::TextStyle::Body.resolve(ui.style()),
                Color32::DARK_GRAY,
            );
        }
    }
    let (across, down) = state.across_and_down();
    let cells = session::split_cells(&across, &down);
    if cells.len() > 1 {
        let font = egui::FontId::proportional((rect.height() * 0.05).clamp(16.0, 48.0));
        for (number, [left, top, right, bottom]) in cells.iter().enumerate() {
            let cell = Rect::from_min_max(
                rect.lerp_inside(Vec2::new(*left, *top)),
                rect.lerp_inside(Vec2::new(*right, *bottom)),
            );
            let tint = if number % 2 == 0 {
                SELECTED_COLOR
            } else {
                WARNING_COLOR
            };
            painter.rect_filled(cell, 0.0, tint.gamma_multiply(0.12));
            painter.text(
                cell.center(),
                egui::Align2::CENTER_CENTER,
                (number + 1).to_string(),
                font.clone(),
                tint.gamma_multiply(0.8),
            );
        }
    }
    let line = |cut: Cut, stroke: egui::Stroke| {
        let at = cut_at(cut, rect);
        let points = if cut.down {
            [Pos2::new(at, rect.top()), Pos2::new(at, rect.bottom())]
        } else {
            [Pos2::new(rect.left(), at), Pos2::new(rect.right(), at)]
        };
        painter.line_segment(points, stroke);
    };
    for (index, cut) in state.cuts.iter().enumerate() {
        let stroke = if state.chosen == Some(index) {
            egui::Stroke::new(4.0, WARNING_COLOR)
        } else {
            egui::Stroke::new(2.0, ERROR_COLOR)
        };
        line(*cut, stroke);
    }
    if let (Some(from), Some(to)) = (state.drawing, ui.input(|input| input.pointer.latest_pos()))
        && let Some(cut) = drawn_cut(from, to, rect)
    {
        line(cut, egui::Stroke::new(2.0, ERROR_COLOR.gamma_multiply(0.6)));
    }
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
        egui::StrokeKind::Inside,
    );
}

struct RowLook {
    text: String,
    tail: String,
    open: bool,
    color: Option<Color32>,
}

/// Frames around what the run read on the page, each with what it made of it. Other text in the
/// margins only gets a faint frame on pages the run could not place.
fn paint_marks(ui: &egui::Ui, image_rect: Rect, page: &PageRef) {
    let (Some(reading), Some((_, turned))) = (&page.reading, page.turned_from_scan()) else {
        return;
    };
    let part = page.scan.and_then(|scan| scan.area);
    let painter = ui.painter_at(image_rect);
    let font = egui::TextStyle::Small.resolve(ui.style());
    let padding = (image_rect.height() * 0.004).clamp(2.0, 6.0);
    let mut tags: Vec<Rect> = Vec::new();
    for mark in &reading.marks {
        let (color, width, tag) = match mark.kind {
            MarkKind::Title => (
                TITLE_COLOR,
                2.0,
                Some(format!(
                    "Titel: {}",
                    reading.title.as_deref().unwrap_or(&mark.text)
                )),
            ),
            MarkKind::Label => (LABEL_COLOR, 2.0, Some(label_tag(reading))),
            MarkKind::PageNumber => (
                PAGE_NUMBER_COLOR,
                2.0,
                Some(match reading.page_number {
                    Some(number) => format!("Seite {number}"),
                    None => mark.text.clone(),
                }),
            ),
            MarkKind::Text if reading.undetected => (OTHER_TEXT_COLOR, 1.0, None),
            MarkKind::Text => continue,
        };
        let Some(area) = mark_rect(mark.area, part, turned, image_rect) else {
            continue;
        };
        let area = area.expand(padding);
        painter.rect_stroke(
            area,
            2.0,
            egui::Stroke::new(width, color),
            egui::StrokeKind::Outside,
        );
        let Some(tag) = tag else {
            continue;
        };
        let galley = painter.layout_no_wrap(tag, font.clone(), Color32::WHITE);
        let size = galley.size() + Vec2::new(6.0, 2.0);
        // Above the frame, or below it at the top edge of the page.
        let top = if area.top() - size.y >= image_rect.top() {
            area.top() - size.y
        } else {
            area.bottom()
        };
        let mut tag_rect = Rect::from_min_size(Pos2::new(area.left(), top), size);
        // Title and label can come from the same line: their tags go side by side.
        while let Some(taken) = tags.iter().find(|taken| taken.intersects(tag_rect)) {
            tag_rect = tag_rect.translate(Vec2::new(taken.right() + 4.0 - tag_rect.left(), 0.0));
        }
        tags.push(tag_rect);
        painter.rect_filled(tag_rect, 2.0, color);
        painter.galley(tag_rect.min + Vec2::new(3.0, 1.0), galley, Color32::WHITE);
    }
}

fn label_tag(reading: &ScanPage) -> String {
    let label = reading.label.as_deref().unwrap_or_default();
    match &reading.instrument {
        Some(instrument) => {
            let voice = reading
                .voice
                .map(|voice| format!(" {voice}"))
                .unwrap_or_default();
            format!("„{label}“: {instrument}{voice}")
        }
        None => format!("„{label}“: kein Instrument"),
    }
}

/// Where an area of the scan page, in shares of it before it is turned, lies on a page shown
/// turned clockwise by `turned`. A page that shows only `part` of the scan page shows no area
/// outside the part.
fn mark_rect(area: [f32; 4], part: Option<Crop>, turned: u16, image_rect: Rect) -> Option<Rect> {
    let [left, top, width, height] = area;
    let (mut a, mut b) = ((left, top), (left + width, top + height));
    if let Some(part) = part {
        let [part_left, part_top, part_right, part_bottom] = part.shares();
        let center = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        if !(part_left..part_right).contains(&center.0) || !(part_top..part_bottom).contains(&center.1) {
            return None;
        }
        let within = |(x, y): (f32, f32)| {
            (
                (x - part_left) / (part_right - part_left),
                (y - part_top) / (part_bottom - part_top),
            )
        };
        (a, b) = (within(a), within(b));
    }
    let turn = |point| session::turn(point, turned);
    let (a, b) = (turn(a), turn(b));
    let at = |(x, y): (f32, f32)| {
        Pos2::new(
            image_rect.left() + x * image_rect.width(),
            image_rect.top() + y * image_rect.height(),
        )
    };
    Some(Rect::from_two_pos(at(a), at(b)))
}

fn has_pdfs(folder: &Path, output_dir: &Path) -> bool {
    pdf::discover_pdfs(folder, &pdf::normalize_path(output_dir))
        .is_ok_and(|files| !files.is_empty())
}

fn source_name(source: &Path) -> String {
    source
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn paint_drag(
    ctx: &egui::Context,
    layout: &FrameLayout,
    pointer: Pos2,
    count: usize,
    target: Option<DropTarget>,
) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::unique("page_drag"),
    ));
    if let (Some(DropTarget::InFile(before)), Some(area)) = (target, layout.pages_area) {
        let x = match before {
            Some(before) => layout
                .pages
                .iter()
                .find(|(id, _)| *id == before)
                .map(|(_, rect)| rect.left() - PAGE_GAP / 2.0),
            None => layout
                .pages
                .last()
                .map(|(_, rect)| rect.right() + PAGE_GAP / 2.0),
        };
        if let Some(x) = x {
            painter.vline(
                x,
                area.top()..=area.bottom(),
                egui::Stroke::new(4.0, SELECTED_COLOR),
            );
        }
    }
    let font = egui::FontId::proportional(14.0);
    let galley = painter.layout_no_wrap(pages_text(count), font, Color32::WHITE);
    let tag = Rect::from_min_size(
        pointer + Vec2::new(14.0, 14.0),
        galley.size() + Vec2::splat(10.0),
    );
    painter.rect_filled(tag, 4.0, SELECTED_COLOR);
    painter.galley(tag.min + Vec2::splat(5.0), galley, Color32::WHITE);
}

enum Answer {
    Pending,
    Yes,
    No,
}

/// The part of a name the rename dialog marks at the start: what follows the last " - ", such as
/// "Copy 1" in "Stück - Horn - Copy 1", in characters.
fn rename_selection(name: &str) -> Option<Range<usize>> {
    let at = name.rfind(" - ")? + " - ".len();
    let (start, end) = (name[..at].chars().count(), name.chars().count());
    (start < end).then_some(start..end)
}

/// Asks for a name. `select` marks these characters once, when the dialog opens.
fn name_dialog(
    ctx: &egui::Context,
    title: &str,
    name: &mut String,
    error: Option<&NameError>,
    select: &mut Option<Range<usize>>,
) -> Answer {
    let modal = egui::Modal::new(egui::Id::unique("name_dialog")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.heading(title);
        ui.add_space(6.0);
        let edit = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
        edit.request_focus();
        if let Some(range) = select.take()
            && let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), edit.id)
        {
            state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(range.start),
                egui::text::CCursor::new(range.end),
            )));
            state.store(ui.ctx(), edit.id);
        }
        if let Some(error) = error {
            ui.label(RichText::new(error.to_string()).color(ERROR_COLOR));
        }
        ui.add_space(6.0);
        let mut answer = Answer::Pending;
        ui.horizontal(|ui| {
            if ui.button("OK").clicked() || ui.input(|input| input.key_pressed(Key::Enter)) {
                answer = Answer::Yes;
            }
            if ui.button("Abbrechen").clicked() {
                answer = Answer::No;
            }
        });
        answer
    });
    if modal.should_close() {
        return Answer::No;
    }
    modal.inner
}

fn yes_no_dialog(ctx: &egui::Context, text: &str, yes: &str, no: &str) -> Answer {
    let modal = egui::Modal::new(egui::Id::unique("yes_no_dialog")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.label(text);
        ui.add_space(8.0);
        let mut answer = Answer::Pending;
        ui.horizontal(|ui| {
            if ui.button(yes).clicked() {
                answer = Answer::Yes;
            }
            if ui.button(no).clicked() {
                answer = Answer::No;
            }
        });
        answer
    });
    if modal.should_close() {
        return Answer::No;
    }
    modal.inner
}

enum UnsavedAnswer {
    Pending,
    Save,
    Drop,
    Cancel,
}

fn unsaved_dialog(ctx: &egui::Context, text: &str) -> UnsavedAnswer {
    let modal = egui::Modal::new(egui::Id::unique("unsaved_dialog")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.label(text);
        ui.add_space(8.0);
        let mut answer = UnsavedAnswer::Pending;
        ui.horizontal(|ui| {
            if ui.button("Speichern").clicked() {
                answer = UnsavedAnswer::Save;
            }
            if ui.button("Nicht speichern").clicked() {
                answer = UnsavedAnswer::Drop;
            }
            if ui.button("Abbrechen").clicked() {
                answer = UnsavedAnswer::Cancel;
            }
        });
        answer
    });
    if modal.should_close() {
        return UnsavedAnswer::Cancel;
    }
    modal.inner
}
