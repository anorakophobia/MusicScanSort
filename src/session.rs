//! The files and pages the window shows, the edits on them and what saving has to do.

use crate::output::sanitise_filename;
use crate::record::ScanPage;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type FileId = u64;
pub type PageId = u64;

/// The list of pages no file holds. They are never saved.
pub const UNUSED: FileId = 0;

/// Leaves room in the 255 characters Windows allows for a file name, also for a temporary file.
const MAX_NAME_LENGTH: usize = 200;

/// A part of a page, as shares of it in steps of 1/`Crop::FULL`: the left, top, right and bottom
/// edge. Whole numbers, so that a page with a crop can be a key.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Crop {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

impl Crop {
    pub const FULL: u16 = 10_000;

    /// Left, top, right and bottom as shares.
    pub fn shares(self) -> [f32; 4] {
        let share = |value: u16| f32::from(value) / f32::from(Self::FULL);
        [
            share(self.left),
            share(self.top),
            share(self.right),
            share(self.bottom),
        ]
    }

    /// The part `cell` (left, top, right, bottom as shares) of a page shown turned clockwise by
    /// `turned`, as a crop of the page before the turn. `outer` is the crop the page already has.
    pub fn of_shown(outer: Option<Self>, turned: u16, cell: [f32; 4]) -> Self {
        let [x0, y0, x1, y1] = cell;
        let (a, b) = (unturn((x0, y0), turned), unturn((x1, y1), turned));
        let [left, top, right, bottom] = outer.map_or([0.0, 0.0, 1.0, 1.0], Self::shares);
        let within = |(x, y): (f32, f32)| {
            (
                left + x.clamp(0.0, 1.0) * (right - left),
                top + y.clamp(0.0, 1.0) * (bottom - top),
            )
        };
        let (a, b) = (within(a), within(b));
        let step = |share: f32| (share * f32::from(Self::FULL)).round() as u16;
        Self {
            left: step(a.0.min(b.0)),
            top: step(a.1.min(b.1)),
            right: step(a.0.max(b.0)),
            bottom: step(a.1.max(b.1)),
        }
    }
}

/// Where a point of a page shown turned clockwise by `turned` lies on the page before the turn,
/// as shares of each.
pub fn unturn((x, y): (f32, f32), turned: u16) -> (f32, f32) {
    match turned % 360 {
        90 => (y, 1.0 - x),
        180 => (1.0 - x, 1.0 - y),
        270 => (1.0 - y, x),
        _ => (x, y),
    }
}

/// Where a point of a page lies once it is turned clockwise by `turned`, as shares of each.
pub fn turn((x, y): (f32, f32), turned: u16) -> (f32, f32) {
    match turned % 360 {
        90 => (1.0 - y, x),
        180 => (1.0 - x, 1.0 - y),
        270 => (y, 1.0 - x),
        _ => (x, y),
    }
}

/// One page of a file: a page of a source PDF, or a part of it, plus the rotation to add to it.
#[derive(Clone, Debug, PartialEq)]
pub struct PageRef {
    pub id: PageId,
    pub source: Arc<PathBuf>,
    pub index: usize,
    pub rotation: u16,
    /// Size of the source page before `crop` and `rotation`, in points.
    pub width: f32,
    pub height: f32,
    /// The part of the source page, before `rotation`, of a page that was split (spec §14.5).
    pub crop: Option<Crop>,
    /// The scan page of the run record, when the folder has one.
    pub scan: Option<ScanRef>,
    /// What the run read on that scan page.
    pub reading: Option<Arc<ScanPage>>,
}

/// A page of the run record, and how far the source page is already turned against that scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanRef {
    pub page: usize,
    pub turned: u16,
    /// The part of the scan page, before it is turned, that the page shows, if not all of it.
    pub area: Option<Crop>,
}

impl PageRef {
    /// A page that gets its id when it is added to a session.
    pub fn new(source: Arc<PathBuf>, index: usize, rotation: u16, size: (f32, f32)) -> Self {
        Self {
            id: 0,
            source,
            index,
            rotation,
            width: size.0,
            height: size.1,
            crop: None,
            scan: None,
            reading: None,
        }
    }

    /// The part `cell` (left, top, right, bottom as shares) of the page as it is shown.
    pub fn part(&self, cell: [f32; 4]) -> Self {
        let scan = self.scan.map(|scan| ScanRef {
            area: Some(Crop::of_shown(
                scan.area,
                (scan.turned + self.rotation) % 360,
                cell,
            )),
            ..scan
        });
        Self {
            crop: Some(Crop::of_shown(self.crop, self.rotation, cell)),
            scan,
            ..self.clone()
        }
    }

    pub fn with_scan(self, scan: Option<ScanRef>) -> Self {
        Self { scan, ..self }
    }

    pub fn with_reading(self, reading: Option<Arc<ScanPage>>) -> Self {
        Self { reading, ..self }
    }

    /// The rotation against the scan page, if the page is known to the run record.
    pub fn turned_from_scan(&self) -> Option<(usize, u16)> {
        self.scan
            .map(|scan| (scan.page, (scan.turned + self.rotation) % 360))
    }

    /// Width divided by height, as the page is shown.
    pub fn aspect(&self) -> f32 {
        let [left, top, right, bottom] = self.crop.map_or([0.0, 0.0, 1.0, 1.0], Crop::shares);
        let (width, height) = (self.width * (right - left), self.height * (bottom - top));
        let (width, height) = if self.rotation % 180 == 90 {
            (height, width)
        } else {
            (width, height)
        };
        if height > 0.0 { width / height } else { 1.0 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PdfEntry {
    pub id: FileId,
    pub file_name: String,
    pub pages: Vec<PageRef>,
}

impl PdfEntry {
    /// The name without the `.pdf` ending.
    pub fn name(&self) -> &str {
        name_without_ending(&self.file_name)
    }

    /// The part of the name before " - ": the piece, in the names this program gives.
    pub fn heading(&self) -> Option<&str> {
        self.name().split_once(" - ").map(|(heading, _)| heading)
    }
}

fn name_without_ending(file_name: &str) -> &str {
    match file_name.len().checked_sub(4) {
        Some(end)
            if file_name.is_char_boundary(end) && file_name[end..].eq_ignore_ascii_case(".pdf") =>
        {
            &file_name[..end]
        }
        _ => file_name,
    }
}

/// Whether the pages come from other files or are the files of the folder itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    /// A sorting run: the pages come from the scans, the folder holds the results.
    Sorted,
    /// An opened folder: saving replaces the files the pages come from.
    Opened,
}

#[derive(Debug, Eq, PartialEq)]
pub enum NameError {
    Empty,
    TooLong,
    Taken,
}

impl fmt::Display for NameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "Der Name darf nicht leer sein.",
            Self::TooLong => "Der Name ist zu lang.",
            Self::Taken => "Eine Datei mit diesem Namen gibt es schon.",
        })
    }
}

/// What saving has to do in the folder. All names are file names in `dir`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavePlan {
    pub dir: PathBuf,
    /// Files to write with these pages.
    pub writes: Vec<(String, Vec<PageRef>)>,
    /// Files whose pages stayed the same, so they keep their content under a new name.
    pub renames: Vec<(String, String)>,
    /// Files on disk that are replaced or no longer needed.
    pub removes: Vec<String>,
    /// All files the folder holds after the save, for the run record.
    pub files: Vec<(String, Vec<PageRef>)>,
}

#[derive(Clone, Debug, PartialEq)]
struct State {
    files: Vec<PdfEntry>,
    unused: Vec<PageRef>,
}

pub struct Session {
    pub dir: PathBuf,
    pub origin: Origin,
    state: State,
    saved: State,
    undo: Vec<State>,
    next_id: u64,
}

impl Session {
    pub fn new(dir: &Path, origin: Origin) -> Self {
        let empty = State {
            files: Vec::new(),
            unused: Vec::new(),
        };
        Self {
            dir: dir.to_path_buf(),
            origin,
            state: empty.clone(),
            saved: empty,
            undo: Vec::new(),
            next_id: 1,
        }
    }

    fn new_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn with_ids(&mut self, pages: Vec<PageRef>) -> Vec<PageRef> {
        pages
            .into_iter()
            .map(|page| PageRef {
                id: self.new_id(),
                ..page
            })
            .collect()
    }

    /// Adds a file that is on disk. It counts as changed until `mark_saved`.
    pub fn add_file(&mut self, file_name: String, pages: Vec<PageRef>) -> FileId {
        let id = self.new_id();
        let pages = self.with_ids(pages);
        self.state.files.push(PdfEntry {
            id,
            file_name,
            pages,
        });
        id
    }

    pub fn add_unused(&mut self, pages: Vec<PageRef>) {
        let pages = self.with_ids(pages);
        self.state.unused.extend(pages);
    }

    /// The current state is what the folder holds now. Emptied files are gone from it.
    pub fn mark_saved(&mut self) {
        self.state.files.retain(|file| !file.pages.is_empty());
        self.saved = self.state.clone();
        self.undo.clear();
    }

    /// The files in the order of the list: by piece, then by name. Names without a piece come first.
    pub fn sorted_files(&self) -> Vec<&PdfEntry> {
        let mut files: Vec<&PdfEntry> = self.state.files.iter().collect();
        files.sort_by_cached_key(|file| {
            (file.heading().map(sort_key), sort_key(file.name()), file.id)
        });
        files
    }

    pub fn file(&self, id: FileId) -> Option<&PdfEntry> {
        self.state.files.iter().find(|file| file.id == id)
    }

    /// The pages of a file or of `UNUSED`.
    pub fn pages(&self, id: FileId) -> &[PageRef] {
        if id == UNUSED {
            return &self.state.unused;
        }
        self.file(id).map_or(&[], |file| &file.pages)
    }

    fn pages_mut(&mut self, id: FileId) -> Option<&mut Vec<PageRef>> {
        if id == UNUSED {
            return Some(&mut self.state.unused);
        }
        self.state
            .files
            .iter_mut()
            .find(|file| file.id == id)
            .map(|file| &mut file.pages)
    }

    fn take_pages(&mut self, from: FileId, ids: &HashSet<PageId>) -> Vec<PageRef> {
        let Some(pages) = self.pages_mut(from) else {
            return Vec::new();
        };
        let (taken, kept) = std::mem::take(pages)
            .into_iter()
            .partition(|page| ids.contains(&page.id));
        *pages = kept;
        taken
    }

    /// Runs an edit, and remembers the state before it if it changed anything.
    fn edit(&mut self, change: impl FnOnce(&mut Self)) {
        let before = self.state.clone();
        change(self);
        if self.state != before {
            self.undo.push(before);
        }
    }

    /// Puts the pages in front of page `before`, or at the end. They stay in their order and
    /// together.
    pub fn reorder(&mut self, file: FileId, ids: &HashSet<PageId>, before: Option<PageId>) {
        self.edit(|session| {
            // The page to land in front of may be one of those that move.
            let anchor = before.and_then(|before| {
                let pages = session.pages(file);
                let start = pages.iter().position(|page| page.id == before)?;
                pages[start..]
                    .iter()
                    .find(|page| !ids.contains(&page.id))
                    .map(|page| page.id)
            });
            let moved = session.take_pages(file, ids);
            if let Some(pages) = session.pages_mut(file) {
                let at = anchor
                    .and_then(|anchor| pages.iter().position(|page| page.id == anchor))
                    .unwrap_or(pages.len());
                pages.splice(at..at, moved);
            }
        });
    }

    /// Adds the pages to the end of another file. To the same file it changes nothing.
    pub fn append(&mut self, from: FileId, ids: &HashSet<PageId>, to: FileId) {
        if from == to {
            return;
        }
        self.edit(|session| {
            let moved = session.take_pages(from, ids);
            if let Some(pages) = session.pages_mut(to) {
                pages.extend(moved);
            }
        });
    }

    /// Moves the pages into a new file with this name.
    pub fn move_to_new_file(
        &mut self,
        from: FileId,
        ids: &HashSet<PageId>,
        name: &str,
    ) -> Result<FileId, NameError> {
        let file_name = self.checked_file_name(name, None)?;
        let id = self.new_id();
        self.edit(|session| {
            session.drop_emptied(&file_name, None);
            let pages = session.take_pages(from, ids);
            session.state.files.push(PdfEntry {
                id,
                file_name,
                pages,
            });
        });
        Ok(id)
    }

    /// Splits a page at lines across (`across`, heights as shares of the page as shown) and lines
    /// down (`down`, widths as shares) into parts, which take its place in reading order: row by
    /// row, from the left. Returns the ids of the parts, none if no line lies inside the page.
    pub fn split(&mut self, file: FileId, page: PageId, across: &[f32], down: &[f32]) -> Vec<PageId> {
        let cells = split_cells(across, down);
        if cells.len() < 2 {
            return Vec::new();
        }
        let Some(position) = self.pages(file).iter().position(|shown| shown.id == page) else {
            return Vec::new();
        };
        let whole = self.pages(file)[position].clone();
        let parts: Vec<PageRef> = cells
            .into_iter()
            .map(|cell| PageRef {
                id: self.new_id(),
                ..whole.part(cell)
            })
            .collect();
        let ids = parts.iter().map(|part| part.id).collect();
        self.edit(|session| {
            if let Some(pages) = session.pages_mut(file) {
                pages.splice(position..=position, parts);
            }
        });
        ids
    }

    /// Puts a copy of each of the pages right behind it. Returns the ids of the copies.
    pub fn duplicate(&mut self, file: FileId, ids: &HashSet<PageId>) -> Vec<PageId> {
        let mut pages = Vec::new();
        let mut copies = Vec::new();
        for page in self.pages(file).to_vec() {
            let copied = ids.contains(&page.id).then(|| PageRef {
                id: self.new_id(),
                ..page.clone()
            });
            pages.push(page);
            if let Some(copy) = copied {
                copies.push(copy.id);
                pages.push(copy);
            }
        }
        if !copies.is_empty() {
            self.edit(|session| {
                if let Some(shown) = session.pages_mut(file) {
                    *shown = pages;
                }
            });
        }
        copies
    }

    /// Turns the pages by 90 degrees clockwise.
    pub fn rotate(&mut self, file: FileId, ids: &HashSet<PageId>) {
        self.edit(|session| {
            if let Some(pages) = session.pages_mut(file) {
                for page in pages.iter_mut().filter(|page| ids.contains(&page.id)) {
                    page.rotation = (page.rotation + 90) % 360;
                }
            }
        });
    }

    pub fn rename(&mut self, id: FileId, name: &str) -> Result<(), NameError> {
        let file_name = self.checked_file_name(name, Some(id))?;
        self.edit(|session| {
            session.drop_emptied(&file_name, Some(id));
            if let Some(file) = session.state.files.iter_mut().find(|file| file.id == id) {
                if name_without_ending(&file.file_name) != name_without_ending(&file_name) {
                    file.file_name = file_name;
                }
            }
        });
        Ok(())
    }

    /// Takes deleted (emptied) files with this name out of the list, so that another file can
    /// have the name. Saving still moves the old file to the Recycle Bin, and undo brings it back.
    fn drop_emptied(&mut self, file_name: &str, except: Option<FileId>) {
        self.state.files.retain(|file| {
            Some(file.id) == except
                || !file.pages.is_empty()
                || file.file_name.to_lowercase() != file_name.to_lowercase()
        });
    }

    /// The safe file name for a name the user typed, if no other file with pages has it.
    fn checked_file_name(&self, name: &str, except: Option<FileId>) -> Result<String, NameError> {
        let name = name_without_ending(name.trim());
        if name.trim().is_empty() {
            return Err(NameError::Empty);
        }
        if name.chars().count() > MAX_NAME_LENGTH {
            return Err(NameError::TooLong);
        }
        let file_name = format!("{}.pdf", sanitise_filename(name));
        let taken = self.state.files.iter().any(|file| {
            Some(file.id) != except
                && !file.pages.is_empty()
                && file.file_name.to_lowercase() == file_name.to_lowercase()
        });
        if taken {
            return Err(NameError::Taken);
        }
        Ok(file_name)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn undo(&mut self) {
        if let Some(state) = self.undo.pop() {
            self.state = state;
        }
    }

    /// Back to what the folder holds.
    pub fn discard(&mut self) {
        self.state = self.saved.clone();
        self.undo.clear();
    }

    pub fn is_changed(&self, id: FileId) -> bool {
        let Some(file) = self.file(id) else {
            return false;
        };
        match self.saved.files.iter().find(|saved| saved.id == id) {
            Some(saved) => saved != file,
            None => !file.pages.is_empty(),
        }
    }

    /// Changed files, including deleted ones that another file took the name of.
    pub fn changed_count(&self) -> usize {
        let changed = self
            .state
            .files
            .iter()
            .filter(|file| self.is_changed(file.id))
            .count();
        let dropped = self
            .saved
            .files
            .iter()
            .filter(|saved| self.file(saved.id).is_none())
            .count();
        changed + dropped
    }

    pub fn is_dirty(&self) -> bool {
        self.changed_count() > 0
    }

    pub fn save_plan(&self) -> SavePlan {
        let mut plan = SavePlan {
            dir: self.dir.clone(),
            ..SavePlan::default()
        };
        for file in &self.state.files {
            let saved = self.saved.files.iter().find(|saved| saved.id == file.id);
            match saved {
                None if file.pages.is_empty() => {}
                None => plan
                    .writes
                    .push((file.file_name.clone(), file.pages.clone())),
                Some(saved) if file.pages.is_empty() => {
                    plan.removes.push(saved.file_name.clone());
                }
                Some(saved) if saved.pages == file.pages => {
                    if saved.file_name != file.file_name {
                        plan.renames
                            .push((saved.file_name.clone(), file.file_name.clone()));
                    }
                }
                Some(saved) => {
                    plan.removes.push(saved.file_name.clone());
                    plan.writes
                        .push((file.file_name.clone(), file.pages.clone()));
                }
            }
            if !file.pages.is_empty() {
                plan.files
                    .push((file.file_name.clone(), file.pages.clone()));
            }
        }
        // Deleted files that another file took the name of.
        for saved in &self.saved.files {
            if self.file(saved.id).is_none() {
                plan.removes.push(saved.file_name.clone());
            }
        }
        plan
    }
}

/// Lines closer than this to each other or to the edge cut nothing off.
const MIN_PART: f32 = 0.01;

/// The parts that lines across and down cut a page into, in reading order, as left, top, right
/// and bottom shares.
pub fn split_cells(across: &[f32], down: &[f32]) -> Vec<[f32; 4]> {
    let edges = |lines: &[f32]| {
        let mut edges = vec![0.0];
        let mut lines: Vec<f32> = lines.to_vec();
        lines.sort_by(f32::total_cmp);
        for line in lines {
            if line - edges[edges.len() - 1] >= MIN_PART && 1.0 - line >= MIN_PART {
                edges.push(line);
            }
        }
        edges.push(1.0);
        edges
    };
    let (rows, columns) = (edges(across), edges(down));
    let mut cells = Vec::new();
    for row in rows.windows(2) {
        for column in columns.windows(2) {
            cells.push([column[0], row[0], column[1], row[1]]);
        }
    }
    cells
}

/// Sorts umlauts next to their base letters and ignores case.
fn sort_key(text: &str) -> String {
    text.to_lowercase()
        .replace('ä', "a")
        .replace('ö', "o")
        .replace('ü', "u")
        .replace('ß', "ss")
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn page(name: &str, index: usize) -> PageRef {
        PageRef::new(Arc::new(PathBuf::from(name)), index, 0, (595.0, 842.0))
    }

    /// Files A (pages a0, a1, a2), B (b0), both saved.
    fn session() -> (Session, FileId, FileId) {
        let mut session = Session::new(Path::new("out"), Origin::Sorted);
        let a = session.add_file(
            "A.pdf".to_owned(),
            vec![page("a", 0), page("a", 1), page("a", 2)],
        );
        let b = session.add_file("B.pdf".to_owned(), vec![page("b", 0)]);
        session.mark_saved();
        (session, a, b)
    }

    fn ids(session: &Session, file: FileId, positions: &[usize]) -> HashSet<PageId> {
        positions
            .iter()
            .map(|position| session.pages(file)[*position].id)
            .collect()
    }

    /// The pages as source name and index, like "a0".
    fn shown(session: &Session, file: FileId) -> Vec<String> {
        session
            .pages(file)
            .iter()
            .map(|page| format!("{}{}", page.source.display(), page.index))
            .collect()
    }

    #[test]
    fn reordering_puts_the_pages_in_front_of_the_target_and_keeps_them_together() {
        let (mut session, a, _) = session();
        let last = ids(&session, a, &[2]);
        let first = session.pages(a)[0].id;
        session.reorder(a, &last, Some(first));
        assert_eq!(shown(&session, a), ["a2", "a0", "a1"]);

        let (mut session, a, _) = self::session();
        let outer = ids(&session, a, &[0, 2]);
        session.reorder(a, &outer, None);
        assert_eq!(shown(&session, a), ["a1", "a0", "a2"]);
    }

    #[test]
    fn reordering_onto_a_moved_page_lands_in_front_of_the_next_other_page() {
        let (mut session, a, _) = session();
        let first_two = ids(&session, a, &[0, 1]);
        let second = session.pages(a)[1].id;
        session.reorder(a, &first_two, Some(second));
        assert_eq!(shown(&session, a), ["a0", "a1", "a2"]);
        assert!(!session.can_undo(), "nothing changed, so nothing to undo");
        assert!(!session.is_dirty());
    }

    #[test]
    fn appending_moves_the_pages_in_their_order_to_the_end_of_the_target() {
        let (mut session, a, b) = session();
        let chosen = ids(&session, a, &[2, 0]);
        session.append(a, &chosen, b);
        assert_eq!(shown(&session, a), ["a1"]);
        assert_eq!(shown(&session, b), ["b0", "a0", "a2"]);
        assert!(session.is_changed(a) && session.is_changed(b));
    }

    #[test]
    fn appending_to_the_same_file_changes_nothing() {
        let (mut session, a, _) = session();
        let chosen = ids(&session, a, &[0]);
        session.append(a, &chosen, a);
        assert_eq!(shown(&session, a), ["a0", "a1", "a2"]);
        assert!(!session.can_undo());
    }

    #[test]
    fn removed_pages_wait_in_the_unused_list_and_can_come_back() {
        let (mut session, a, b) = session();
        let chosen = ids(&session, a, &[1]);
        session.append(a, &chosen, UNUSED);
        assert_eq!(shown(&session, UNUSED), ["a1"]);
        let back = ids(&session, UNUSED, &[0]);
        session.append(UNUSED, &back, b);
        assert_eq!(shown(&session, b), ["b0", "a1"]);
        assert!(session.pages(UNUSED).is_empty());
    }

    #[test]
    fn a_new_file_needs_a_free_name() {
        let (mut session, a, _) = session();
        let chosen = ids(&session, a, &[0]);
        assert_eq!(
            session.move_to_new_file(a, &chosen, "b"),
            Err(NameError::Taken)
        );
        assert_eq!(
            session.move_to_new_file(a, &chosen, "  "),
            Err(NameError::Empty)
        );
        assert_eq!(shown(&session, a), ["a0", "a1", "a2"]);

        let new = session.move_to_new_file(a, &chosen, "Lyra.pdf").unwrap();
        assert_eq!(session.file(new).unwrap().file_name, "Lyra.pdf");
        assert_eq!(shown(&session, new), ["a0"]);
    }

    #[test]
    fn names_get_safe_characters_and_a_file_may_keep_its_own_name() {
        let (mut session, a, b) = session();
        session.rename(a, "Flöte/Oboe").unwrap();
        assert_eq!(session.file(a).unwrap().file_name, "Flöte_Oboe.pdf");
        assert_eq!(session.rename(a, "B"), Err(NameError::Taken));
        assert_eq!(session.rename(a, &"x".repeat(201)), Err(NameError::TooLong));
        assert_eq!(session.rename(b, "b"), Ok(()), "only the case changes");
        assert_eq!(session.file(b).unwrap().file_name, "b.pdf");
    }

    #[test]
    fn lines_cut_a_page_into_parts_in_reading_order() {
        assert_eq!(split_cells(&[], &[]), [[0.0, 0.0, 1.0, 1.0]]);
        assert_eq!(
            split_cells(&[0.5], &[0.3]),
            [
                [0.0, 0.0, 0.3, 0.5],
                [0.3, 0.0, 1.0, 0.5],
                [0.0, 0.5, 0.3, 1.0],
                [0.3, 0.5, 1.0, 1.0]
            ]
        );
        // Lines at the edge or on top of each other cut nothing off.
        assert_eq!(split_cells(&[0.6, 0.005, 0.6, 0.999], &[]).len(), 2);
    }

    #[test]
    fn a_part_of_a_turned_page_is_cut_from_the_page_before_the_turn() {
        let shares = |crop: Crop| crop.shares().map(|share| (share * 100.0).round());
        let upper = [0.0, 0.0, 1.0, 0.25];
        assert_eq!(shares(Crop::of_shown(None, 0, upper)), [0.0, 0.0, 100.0, 25.0]);
        // Turned clockwise, the top of the shown page is the left of the page.
        assert_eq!(shares(Crop::of_shown(None, 90, upper)), [0.0, 0.0, 25.0, 100.0]);
        assert_eq!(shares(Crop::of_shown(None, 180, upper)), [0.0, 75.0, 100.0, 100.0]);
        assert_eq!(shares(Crop::of_shown(None, 270, upper)), [75.0, 0.0, 100.0, 100.0]);
        // A part of a part.
        let half = Crop::of_shown(None, 0, [0.0, 0.5, 1.0, 1.0]);
        assert_eq!(
            shares(Crop::of_shown(Some(half), 0, [0.5, 0.0, 1.0, 0.5])),
            [50.0, 50.0, 100.0, 75.0]
        );
        for turned in [0, 90, 180, 270] {
            let point = (0.2, 0.7);
            let back = unturn(turn(point, turned), turned);
            assert!((back.0 - point.0).abs() < 1e-6 && (back.1 - point.1).abs() < 1e-6);
        }
    }

    #[test]
    fn splitting_puts_the_parts_in_place_of_the_page_and_undo_joins_them() {
        let (mut session, a, _) = session();
        let middle = session.pages(a)[1].id;
        session.pages_mut(a).unwrap()[1].scan = Some(ScanRef {
            page: 7,
            turned: 90,
            area: None,
        });
        session.mark_saved();
        assert!(session.split(a, middle, &[], &[]).is_empty(), "no line");
        let parts = session.split(a, middle, &[0.4], &[]);
        assert_eq!(parts.len(), 2);
        assert_eq!(shown(&session, a), ["a0", "a1", "a1", "a2"]);
        let pages = session.pages(a);
        assert_eq!([pages[1].id, pages[2].id], [parts[0], parts[1]]);
        // A4 upright is about 0.71 wide per high; the upper 40 % is wider than high.
        assert!((pages[1].aspect() - 595.0 / (842.0 * 0.4)).abs() < 0.01);
        let crop = pages[2].crop.unwrap().shares();
        assert!((crop[1] - 0.4).abs() < 1e-3 && (crop[3] - 1.0).abs() < 1e-3);
        // The scan page is turned clockwise by 90° to be shown: its left side is at the top, so
        // the lower part is its right side.
        let area = pages[2].scan.unwrap().area.unwrap().shares();
        assert!((area[0] - 0.4).abs() < 1e-3 && area[2] == 1.0 && area[1] == 0.0 && area[3] == 1.0);
        assert_eq!(pages[2].scan.unwrap().page, 7);

        let plan = session.save_plan();
        assert_eq!(plan.writes.len(), 1, "the file is written again");
        assert_eq!(plan.writes[0].1.len(), 4);
        session.undo();
        assert_eq!(shown(&session, a), ["a0", "a1", "a2"]);
        assert!(!session.is_dirty());
    }

    #[test]
    fn a_duplicate_lands_right_behind_its_page() {
        let (mut session, a, _) = session();
        let chosen = ids(&session, a, &[0, 2]);
        let copies = session.duplicate(a, &chosen);
        assert_eq!(shown(&session, a), ["a0", "a0", "a1", "a2", "a2"]);
        let pages = session.pages(a);
        assert_eq!(copies, [pages[1].id, pages[4].id]);
        assert_ne!(pages[0].id, pages[1].id, "a copy is a page of its own");
        assert_eq!(session.save_plan().writes[0].1.len(), 5);
        session.undo();
        assert_eq!(shown(&session, a), ["a0", "a1", "a2"]);
        assert!(!session.is_dirty());
        assert!(session.duplicate(a, &HashSet::new()).is_empty());
        assert!(!session.can_undo(), "nothing to duplicate is no edit");
    }

    #[test]
    fn a_file_may_take_the_name_of_a_deleted_file() {
        let (mut session, a, b) = session();
        let all = ids(&session, b, &[0]);
        session.append(b, &all, UNUSED);
        session.rename(a, "B").unwrap();
        assert!(session.file(b).is_none(), "the deleted file leaves the list");
        assert_eq!(session.sorted_files().len(), 1);
        assert_eq!(session.changed_count(), 2);
        let plan = session.save_plan();
        assert_eq!(plan.renames, [("A.pdf".to_owned(), "B.pdf".to_owned())]);
        assert_eq!(plan.removes, ["B.pdf"]);

        session.undo();
        assert_eq!(session.file(a).unwrap().file_name, "A.pdf");
        assert!(session.file(b).is_some(), "undo brings the deleted file back");

        let chosen = ids(&session, a, &[0]);
        let new = session.move_to_new_file(a, &chosen, "b").unwrap();
        assert!(session.file(b).is_none());
        assert_eq!(session.file(new).unwrap().file_name, "b.pdf");
    }

    #[test]
    fn undo_goes_back_one_edit_at_a_time_and_removes_new_files() {
        let (mut session, a, b) = session();
        let chosen = ids(&session, a, &[0]);
        session.append(a, &chosen, b);
        let chosen = ids(&session, a, &[0]);
        let new = session.move_to_new_file(a, &chosen, "Neu").unwrap();

        session.undo();
        assert!(session.file(new).is_none());
        assert_eq!(shown(&session, a), ["a1", "a2"]);
        session.undo();
        assert_eq!(shown(&session, a), ["a0", "a1", "a2"]);
        assert!(!session.can_undo());
        assert!(!session.is_dirty());
    }

    #[test]
    fn moving_a_page_back_by_hand_counts_as_unchanged() {
        let (mut session, a, _) = session();
        let chosen = ids(&session, a, &[2]);
        let first = session.pages(a)[0].id;
        session.reorder(a, &chosen, Some(first));
        assert!(session.is_dirty());
        session.reorder(a, &chosen, None);
        assert!(!session.is_dirty());
    }

    #[test]
    fn four_turns_of_a_page_change_nothing_and_a_quarter_turn_swaps_the_shape() {
        let (mut session, a, _) = session();
        let chosen = ids(&session, a, &[0]);
        let upright = session.pages(a)[0].aspect();
        session.rotate(a, &chosen);
        assert!((session.pages(a)[0].aspect() - 1.0 / upright).abs() < 0.001);
        for _ in 0..3 {
            session.rotate(a, &chosen);
        }
        assert!(!session.is_dirty());
    }

    #[test]
    fn discarding_returns_to_the_last_saved_state() {
        let (mut session, a, b) = session();
        let chosen = ids(&session, a, &[0]);
        session.append(a, &chosen, b);
        session.mark_saved();
        assert!(!session.can_undo(), "saving ends the undo history");

        let chosen = ids(&session, a, &[0]);
        session.append(a, &chosen, UNUSED);
        session.discard();
        assert_eq!(shown(&session, a), ["a1", "a2"]);
        assert_eq!(shown(&session, b), ["b0", "a0"]);
        assert!(session.pages(UNUSED).is_empty());
        assert!(!session.is_dirty());
    }

    #[test]
    fn the_save_plan_touches_only_what_changed() {
        let (mut session, a, b) = session();
        let plan = session.save_plan();
        assert_eq!(plan.dir, PathBuf::from("out"));
        assert!(plan.writes.is_empty() && plan.renames.is_empty() && plan.removes.is_empty());

        session.rename(b, "C").unwrap();
        let plan = session.save_plan();
        assert_eq!(plan.renames, [("B.pdf".to_owned(), "C.pdf".to_owned())]);
        assert!(plan.writes.is_empty() && plan.removes.is_empty());
        let after: Vec<_> = plan.files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            after,
            ["A.pdf", "C.pdf"],
            "the record learns all files, also unchanged ones"
        );

        let chosen = ids(&session, a, &[0]);
        session.append(a, &chosen, b);
        let plan = session.save_plan();
        let names: Vec<_> = plan.writes.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["A.pdf", "C.pdf"]);
        assert_eq!(plan.removes, ["A.pdf", "B.pdf"]);
        assert!(plan.renames.is_empty());
    }

    #[test]
    fn an_emptied_file_is_removed_on_saving_and_a_new_empty_one_never_appears() {
        let (mut session, _, b) = session();
        let only = ids(&session, b, &[0]);
        session.append(b, &only, UNUSED);
        assert!(
            session.file(b).is_some(),
            "it stays in the list until saving"
        );
        let plan = session.save_plan();
        assert_eq!(plan.removes, ["B.pdf"]);
        assert!(plan.writes.is_empty());
        assert_eq!(
            plan.files.len(),
            1,
            "an emptied file is no longer in the folder"
        );
        session.mark_saved();
        assert!(session.file(b).is_none());

        let new = session
            .move_to_new_file(UNUSED, &HashSet::new(), "Leer")
            .unwrap();
        assert!(!session.is_changed(new));
        assert!(session.save_plan().writes.is_empty());
    }

    #[test]
    fn the_list_is_sorted_by_piece_then_name_with_umlauts_in_place() {
        let mut session = Session::new(Path::new("out"), Origin::Sorted);
        for name in [
            "Zwei - Horn.pdf",
            "Ein Stück - Trompete.pdf",
            "Ein Stück - Flöte.pdf",
            "lose Seite.pdf",
            "Ein Stück - Fagott.pdf",
        ] {
            session.add_file(name.to_owned(), Vec::new());
        }
        let names: Vec<_> = session
            .sorted_files()
            .iter()
            .map(|file| file.name().to_owned())
            .collect();
        assert_eq!(
            names,
            [
                "lose Seite",
                "Ein Stück - Fagott",
                "Ein Stück - Flöte",
                "Ein Stück - Trompete",
                "Zwei - Horn",
            ]
        );
    }
}
