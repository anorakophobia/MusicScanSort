use super::*;
use crate::session::tests::page;
use egui::{Event as InputEvent, PointerButton};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};

fn config() -> SortConfig {
    SortConfig {
        input_dir: PathBuf::from("scans"),
        title: "Stück".to_owned(),
        output_dir: PathBuf::from("out"),
        single_piece: false,
        dpi: 150,
        label_dpi: 300,
        blank_threshold: 0.005,
        workers: 1,
        dry_run: false,
    }
}

#[test]
fn marks_turn_with_the_page() {
    let shown = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(100.0, 200.0));
    // A box in the upper left corner of the scan: x 0.1..0.3, y 0.2..0.25.
    let area = [0.1, 0.2, 0.2, 0.05];
    let at = |turned| {
        let rect = mark_rect(area, None, turned, shown).unwrap();
        [rect.left(), rect.top(), rect.right(), rect.bottom()].map(|value| value.round())
    };
    assert_eq!(at(0), [20.0, 60.0, 40.0, 70.0]);
    // Turned clockwise the corner goes to the upper right.
    assert_eq!(at(90), [85.0, 40.0, 90.0, 80.0]);
    assert_eq!(at(180), [80.0, 170.0, 100.0, 180.0]);
    assert_eq!(at(270), [30.0, 160.0, 35.0, 200.0]);

    // The upper half of the scan shows the box twice as high; the lower half does not show it.
    let upper = Crop::of_shown(None, 0, [0.0, 0.0, 1.0, 0.5]);
    let rect = mark_rect(area, Some(upper), 0, shown).unwrap();
    assert_eq!(
        [rect.left(), rect.top(), rect.right(), rect.bottom()].map(|value| value.round()),
        [20.0, 100.0, 40.0, 120.0]
    );
    let lower = Crop::of_shown(None, 0, [0.0, 0.5, 1.0, 1.0]);
    assert!(mark_rect(area, Some(lower), 0, shown).is_none());
}

#[test]
fn duplicate_puts_the_copy_behind_the_page_and_marks_it() {
    let (mut harness, _commands, _events) = window();
    click(&mut harness, "Seite 2", Modifiers::NONE);
    click(&mut harness, "Duplizieren", Modifiers::NONE);
    harness.run_steps(3);
    assert_eq!(shown(harness.state(), "A"), ["a0", "a1", "a1", "a2"]);
    let a = file_named(harness.state(), "A");
    let third = harness.state().session.as_ref().unwrap().pages(a)[2].id;
    assert_eq!(harness.state().selected, HashSet::from([third]));
    harness.key_press_modifiers(Modifiers::COMMAND, Key::D);
    harness.run_steps(3);
    assert_eq!(shown(harness.state(), "A"), ["a0", "a1", "a1", "a1", "a2"]);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.run_steps(3);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.run_steps(3);
    assert_eq!(shown(harness.state(), "A"), ["a0", "a1", "a2"]);
}

#[test]
fn renaming_marks_what_follows_the_last_dash() {
    assert_eq!(rename_selection("Stück - Horn - Copy 1"), Some(15..21));
    assert_eq!(rename_selection("Stück - Horn"), Some(8..12));
    assert_eq!(rename_selection("Horn"), None);
    assert_eq!(rename_selection("Stück - "), None);

    let (mut harness, _commands, _events) = window();
    {
        let app = harness.state_mut();
        let session = app.session.as_mut().unwrap();
        let file = session.add_file("Stück - Horn - Copy 1.pdf".to_owned(), vec![page("c", 0)]);
        session.mark_saved();
        app.open_file = Some(file);
    }
    harness.run_steps(2);
    harness.key_press(Key::F2);
    harness.run_steps(3);
    // Typing replaces the marked part.
    harness.event(InputEvent::Text("2".to_owned()));
    harness.run_steps(2);
    let Some(Dialog::Rename { name, .. }) = &harness.state().dialog else {
        panic!("the rename dialog is open");
    };
    assert_eq!(name, "Stück - Horn - 2");
}

#[test]
fn the_split_dialog_suggests_the_widest_gap_between_ink() {
    // Ink at 10-40 % and 55-90 % of the height, with staff gaps of 2 % in between.
    let mut rows = vec![0.0; 100];
    for row in (10..40).chain(55..90) {
        rows[row] = if row % 5 == 0 { 0.0 } else { 0.2 };
    }
    let ink = Ink {
        rows,
        columns: vec![0.1; 70],
    };
    let cut = suggested_cut(&ink, 0.7).unwrap();
    assert!(!cut.down);
    assert!((cut.at - 0.475).abs() < 0.01, "{}", cut.at);
    // An empty page and a page without a gap get no line.
    let empty = Ink {
        rows: vec![0.0; 100],
        columns: vec![0.0; 70],
    };
    assert!(suggested_cut(&empty, 0.7).is_none());
    let full = Ink {
        rows: vec![0.1; 100],
        columns: vec![0.1; 70],
    };
    assert!(suggested_cut(&full, 0.7).is_none());
}

#[test]
fn a_line_drawn_across_a_page_splits_it_and_undo_joins_it() {
    let (mut harness, _commands, _events) = window();
    click(&mut harness, "Seite 2", Modifiers::NONE);
    harness.key_press(Key::T);
    harness.run_steps(3);
    assert!(matches!(harness.state().dialog, Some(Dialog::Split(_))));
    let image = harness.get_by_label("Seite zum Teilen").rect();
    // A drag mostly sideways draws a line across, at the height it starts.
    let from = image.left_top() + Vec2::new(0.1 * image.width(), 0.3 * image.height());
    let to = from + Vec2::new(0.6 * image.width(), 0.05 * image.height());
    drag(&mut harness, from, to);
    let Some(Dialog::Split(state)) = &harness.state().dialog else {
        panic!("the dialog stays open");
    };
    assert_eq!(state.cuts.len(), 1);
    assert!(!state.cuts[0].down && (state.cuts[0].at - 0.3).abs() < 0.02);
    harness.key_press(Key::Enter);
    harness.run_steps(3);
    assert!(harness.state().dialog.is_none());
    assert_eq!(shown(harness.state(), "A"), ["a0", "a1", "a1", "a2"]);
    assert_eq!(harness.state().selected.len(), 2, "both parts are marked");
    harness.get_by_label("Seite 3");
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.run_steps(3);
    assert_eq!(shown(harness.state(), "A"), ["a0", "a1", "a2"]);
}

#[test]
fn the_marks_switch_starts_on_and_turns_with_the_button_and_m() {
    let (mut harness, _commands, _events) = window();
    assert!(harness.state().show_marks);
    click(&mut harness, "Markierungen", Modifiers::NONE);
    assert!(!harness.state().show_marks);
    harness.key_press(Key::M);
    harness.run_steps(2);
    assert!(harness.state().show_marks);
}

#[test]
fn an_undetected_page_shows_the_other_text_in_grey_until_the_marks_are_off() {
    use crate::record::{Mark, ScanPage};
    use crate::session::ScanRef;

    let reading = ScanPage {
        undetected: true,
        marks: vec![Mark {
            kind: MarkKind::Text,
            text: "Horn".to_owned(),
            area: [0.1, 0.1, 0.3, 0.1],
        }],
        ..ScanPage::default()
    };
    let shown = page("a", 0)
        .with_scan(Some(ScanRef {
            page: 0,
            turned: 0,
            area: None,
        }))
        .with_reading(Some(Arc::new(reading)));
    let (mut harness, _commands, _events) = window();
    let ctx = harness.ctx.clone();
    {
        let app = harness.state_mut();
        let session = app.session.as_mut().unwrap();
        let file = session.add_file("U.pdf".to_owned(), vec![shown.clone()]);
        session.mark_saved();
        app.open_file = Some(file);
        let white = egui::ColorImage::filled([60, 85], Color32::WHITE);
        app.add_texture(&ctx, PageKey::from(&shown), white);
    }
    harness.run_steps(3);
    let page_rect = harness.get_by_label("Seite 1").rect();
    let grey_pixels = |harness: &mut Harness<'_, App>| {
        let image = harness.render().unwrap();
        let scale = image.width() as f32 / harness.ctx.content_rect().width();
        let (width, height) = (page_rect.width(), page_rect.height() - CAPTION_HEIGHT);
        let mut count = 0;
        for y in (page_rect.top() + 0.05 * height) as u32..(page_rect.top() + 0.25 * height) as u32
        {
            for x in
                (page_rect.left() + 0.05 * width) as u32..(page_rect.left() + 0.45 * width) as u32
            {
                let pixel = image.get_pixel((x as f32 * scale) as u32, (y as f32 * scale) as u32);
                let grey =
                    (130..=170).contains(&pixel[0]) && pixel[0] == pixel[1] && pixel[1] == pixel[2];
                count += usize::from(grey);
            }
        }
        count
    };
    assert!(grey_pixels(&mut harness) > 50, "the grey frame");
    harness.key_press(Key::M);
    harness.run_steps(2);
    assert_eq!(grey_pixels(&mut harness), 0, "no frame with the marks off");
}

/// The window with files A (three pages) and B (one page), and the two ends of its worker.
fn window() -> (Harness<'static, App>, Receiver<Command>, Sender<Event>) {
    let (commands, command_receiver) = mpsc::channel();
    let (event_sender, events) = mpsc::channel();
    let worker = Worker {
        commands,
        events,
        cancel: Arc::new(AtomicBool::new(false)),
    };
    let mut app = App::new(worker, config());
    let mut session = Session::new(Path::new("out"), Origin::Sorted);
    let a = session.add_file(
        "A.pdf".to_owned(),
        vec![page("a", 0), page("a", 1), page("a", 2)],
    );
    session.add_file("B.pdf".to_owned(), vec![page("b", 0)]);
    session.mark_saved();
    app.session = Some(session);
    app.open_file = Some(a);
    let mut harness = Harness::builder()
        .with_size(Vec2::new(2200.0, 800.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    harness.run_steps(3);
    (harness, command_receiver, event_sender)
}

fn center_of(harness: &Harness<'_, App>, label: &str) -> Pos2 {
    harness.get_by_label(label).rect().center()
}

fn press(harness: &mut Harness<'_, App>, pos: Pos2, pressed: bool, modifiers: Modifiers) {
    harness.event(InputEvent::ModifiersChanged(modifiers));
    harness.event(InputEvent::PointerMoved(pos));
    harness.event(InputEvent::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers,
    });
    harness.run_steps(2);
}

fn click(harness: &mut Harness<'_, App>, label: &str, modifiers: Modifiers) {
    let pos = center_of(harness, label);
    press(harness, pos, true, modifiers);
    press(harness, pos, false, modifiers);
    harness.event(InputEvent::ModifiersChanged(Modifiers::NONE));
    harness.run_steps(2);
}

/// Presses at the start, moves in steps and lets go at the end, as a mouse does.
fn drag(harness: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    press(harness, from, true, Modifiers::NONE);
    for step in 1..=4 {
        let pos = from + (to - from) * (step as f32 / 4.0);
        harness.event(InputEvent::PointerMoved(pos));
        harness.run_steps(2);
    }
    press(harness, to, false, Modifiers::NONE);
    harness.run_steps(2);
}

fn file_named(app: &App, name: &str) -> FileId {
    let session = app.session.as_ref().unwrap();
    let files = session.sorted_files();
    files.iter().find(|file| file.name() == name).unwrap().id
}

/// The pages of a file as source name and index, like "a0".
fn shown(app: &App, name: &str) -> Vec<String> {
    let id = file_named(app, name);
    app.session
        .as_ref()
        .unwrap()
        .pages(id)
        .iter()
        .map(|page| format!("{}{}", page.source.display(), page.index))
        .collect()
}

fn selected_pages(app: &App) -> Vec<usize> {
    let session = app.session.as_ref().unwrap();
    let pages = session.pages(app.open_file.unwrap());
    let mut positions: Vec<usize> = app
        .selected
        .iter()
        .map(|id| pages.iter().position(|page| page.id == *id).unwrap())
        .collect();
    positions.sort();
    positions
}

#[test]
fn a_click_in_the_list_opens_that_file() {
    let (mut harness, _commands, _events) = window();
    click(&mut harness, "B", Modifiers::NONE);
    let b = file_named(harness.state(), "B");
    assert_eq!(harness.state().open_file, Some(b));
    harness.get_by_label("Seite 1");
}

#[test]
fn plain_ctrl_and_shift_clicks_select_like_in_the_explorer() {
    let (mut harness, _commands, _events) = window();
    click(&mut harness, "Seite 1", Modifiers::NONE);
    assert_eq!(selected_pages(harness.state()), [0]);
    click(&mut harness, "Seite 3", Modifiers::COMMAND);
    assert_eq!(selected_pages(harness.state()), [0, 2]);
    click(&mut harness, "Seite 3", Modifiers::COMMAND);
    assert_eq!(selected_pages(harness.state()), [0]);
    click(&mut harness, "Seite 1", Modifiers::NONE);
    click(&mut harness, "Seite 3", Modifiers::SHIFT);
    assert_eq!(selected_pages(harness.state()), [0, 1, 2]);
    click(&mut harness, "Seite 2", Modifiers::NONE);
    assert_eq!(selected_pages(harness.state()), [1]);
}

#[test]
fn dragging_a_page_inside_the_file_changes_the_order() {
    let (mut harness, _commands, _events) = window();
    let third = harness.get_by_label("Seite 3").rect();
    let first = harness.get_by_label("Seite 1").rect();
    drag(
        &mut harness,
        third.center(),
        Pos2::new(first.left() + 5.0, first.center().y),
    );
    assert_eq!(shown(harness.state(), "A"), ["a2", "a0", "a1"]);
    assert!(harness.state().is_dirty());
}

#[test]
fn dragging_marked_pages_onto_another_file_appends_them_and_the_file_stays_open() {
    let (mut harness, _commands, _events) = window();
    click(&mut harness, "Seite 1", Modifiers::NONE);
    click(&mut harness, "Seite 3", Modifiers::COMMAND);
    let from = center_of(&harness, "Seite 3");
    let onto = center_of(&harness, "B");
    drag(&mut harness, from, onto);
    assert_eq!(shown(harness.state(), "A"), ["a1"]);
    assert_eq!(shown(harness.state(), "B"), ["b0", "a0", "a2"]);
    let a = file_named(harness.state(), "A");
    assert_eq!(harness.state().open_file, Some(a));
    assert!(harness.state().selected.is_empty());
}

#[test]
fn dragging_a_page_that_is_not_marked_moves_only_that_page() {
    let (mut harness, _commands, _events) = window();
    click(&mut harness, "Seite 1", Modifiers::NONE);
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    drag(&mut harness, from, onto);
    assert_eq!(shown(harness.state(), "A"), ["a0", "a2"]);
    assert_eq!(shown(harness.state(), "B"), ["b0", "a1"]);
}

#[test]
fn a_drop_on_the_new_file_row_asks_for_the_name() {
    let (mut harness, _commands, _events) = window();
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "+ Neues PDF (Seiten hierher ziehen)");
    drag(&mut harness, from, onto);
    assert_eq!(
        shown(harness.state(), "A"),
        ["a0", "a1", "a2"],
        "not before the name"
    );
    let Some(Dialog::NewFile { name, .. }) = &mut harness.state_mut().dialog else {
        panic!("the dialog asks for the name");
    };
    *name = "Lyra".to_owned();
    harness.run_steps(2);
    harness.key_press(Key::Enter);
    harness.run_steps(3);
    assert_eq!(shown(harness.state(), "Lyra"), ["a1"]);
    assert_eq!(shown(harness.state(), "A"), ["a0", "a2"]);
}

#[test]
fn delete_moves_pages_to_the_unused_row_and_undo_brings_them_back() {
    let (mut harness, _commands, _events) = window();
    click(&mut harness, "Seite 2", Modifiers::NONE);
    harness.key_press(Key::Delete);
    harness.run_steps(3);
    assert_eq!(shown(harness.state(), "A"), ["a0", "a2"]);
    harness.get_by_label("Nicht verwendet");
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.run_steps(3);
    assert_eq!(shown(harness.state(), "A"), ["a0", "a1", "a2"]);
    assert!(!harness.state().is_dirty());
}

#[test]
fn delete_file_empties_it_into_the_unused_row_and_opens_the_next_file() {
    let (mut harness, _commands, _events) = window();
    let a = file_named(harness.state(), "A");
    click(&mut harness, "Löschen", Modifiers::NONE);
    harness.run_steps(3);
    assert!(shown(harness.state(), "A").is_empty());
    let session = harness.state().session.as_ref().unwrap();
    assert_eq!(session.pages(UNUSED).len(), 3);
    // Saving moves the emptied file to the Recycle Bin.
    assert_eq!(session.save_plan().removes, ["A.pdf"]);
    assert_eq!(
        harness.state().open_file,
        Some(file_named(harness.state(), "B"))
    );
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.run_steps(3);
    assert_eq!(shown(harness.state(), "A"), ["a0", "a1", "a2"]);
    // Entf with no page marked deletes the open file too.
    harness.state_mut().open_file = Some(a);
    harness.run_steps(2);
    harness.key_press(Key::Delete);
    harness.run_steps(3);
    assert!(shown(harness.state(), "A").is_empty());
}

#[test]
fn the_wheel_scrolls_sideways_and_ctrl_wheel_zooms() {
    let (mut harness, _commands, _events) = window();
    harness.set_size(Vec2::new(1000.0, 800.0));
    harness.run_steps(3);
    let start = harness.get_by_label("Seite 2").rect().left();
    let over_pages = center_of(&harness, "Seite 1");
    harness.event(InputEvent::PointerMoved(over_pages));
    harness.event(InputEvent::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: Vec2::new(0.0, -300.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(30);
    assert!(
        harness.get_by_label("Seite 2").rect().left() < start,
        "turning the wheel down moves the pages to the left"
    );
    assert!((harness.state().zoom - 1.0).abs() < 0.001);

    harness.event(InputEvent::ModifiersChanged(Modifiers::COMMAND));
    harness.event(InputEvent::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: Vec2::new(0.0, 100.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::COMMAND,
    });
    harness.run_steps(5);
    harness.event(InputEvent::ModifiersChanged(Modifiers::NONE));
    assert!(harness.state().zoom > 1.0);
}

#[test]
fn save_sends_the_plan_and_a_sorted_folder_counts_as_saved_afterwards() {
    let (mut harness, commands, events) = window();
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    drag(&mut harness, from, onto);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::S);
    harness.run_steps(2);
    let plan = commands
        .try_iter()
        .find_map(|command| match command {
            Command::Save(plan) => Some(plan),
            _ => None,
        })
        .expect("a save command");
    assert_eq!(plan.writes.len(), 2);
    assert_eq!(plan.removes, ["A.pdf", "B.pdf"]);
    assert!(!harness.state().can_edit(), "no edits while it saves");

    events.send(Event::Saved).unwrap();
    harness.run_steps(2);
    assert!(!harness.state().is_dirty());
    assert!(harness.state().can_edit());
}

#[test]
fn a_failed_save_keeps_the_changes_and_shows_the_reason() {
    let (mut harness, _commands, events) = window();
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    drag(&mut harness, from, onto);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::S);
    harness.run_steps(2);
    events
        .send(Event::SaveFailed {
            message: "A.pdf ist offen".to_owned(),
            folder_changed: false,
        })
        .unwrap();
    harness.run_steps(2);
    assert!(harness.state().is_dirty());
    assert!(harness.state().can_edit());
    harness.get_by_label("A.pdf ist offen");
}

#[test]
fn a_save_that_failed_halfway_shows_what_the_folder_holds_and_cannot_run_again() {
    let (mut harness, commands, events) = window();
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    drag(&mut harness, from, onto);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::S);
    harness.run_steps(2);
    events
        .send(Event::SaveFailed {
            message: "nur teilweise".to_owned(),
            folder_changed: true,
        })
        .unwrap();
    harness.run_steps(2);
    assert!(!harness.state().can_edit(), "the folder is read first");
    assert!(
        commands
            .try_iter()
            .any(|command| matches!(command, Command::Open(_)))
    );
    events
        .send(Event::Opened {
            dir: PathBuf::from("out"),
            files: vec![("A.pdf".to_owned(), vec![page("out/A.pdf", 0)])],
            unreadable: Vec::new(),
        })
        .unwrap();
    harness.run_steps(3);
    assert!(!harness.state().is_dirty());
    assert_eq!(
        harness.state().session.as_ref().unwrap().origin,
        Origin::Opened
    );
    harness.get_by_label("nur teilweise");
}

#[test]
fn saving_is_not_possible_in_the_middle_of_a_drag() {
    let (mut harness, commands, _events) = window();
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    press(&mut harness, from, true, Modifiers::NONE);
    harness.event(InputEvent::PointerMoved(onto));
    harness.run_steps(3);
    assert!(harness.state().drag.is_some());
    harness.key_press_modifiers(Modifiers::COMMAND, Key::S);
    harness.run_steps(2);
    assert!(
        !commands
            .try_iter()
            .any(|command| matches!(command, Command::Save(_)))
    );
    press(&mut harness, onto, false, Modifiers::NONE);
    harness.run_steps(2);
    assert!(
        harness.state().is_dirty(),
        "the drop still counts as unsaved"
    );
}

#[test]
fn closing_in_the_middle_of_a_drag_ends_the_drag() {
    let (mut harness, _commands, _events) = window();
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    press(&mut harness, from, true, Modifiers::NONE);
    harness.event(InputEvent::PointerMoved(onto));
    harness.run_steps(3);
    harness.state_mut().dialog = Some(Dialog::ConfirmDiscard);
    harness.run_steps(2);
    press(&mut harness, onto, false, Modifiers::NONE);
    harness.run_steps(2);
    assert_eq!(shown(harness.state(), "A"), ["a0", "a1", "a2"]);
}

#[test]
fn a_run_that_fails_goes_back_to_its_folder_and_names_the_incomplete_one() {
    let (mut harness, commands, events) = window();
    let app = harness.state_mut();
    app.session = Some(Session::new(Path::new("out-3"), Origin::Sorted));
    app.open_file = None;
    app.job = Some(Job::Sorting {
        stage: Stage::Writing,
        done: 1,
        total: 2,
        started: Instant::now(),
    });
    events
        .send(Event::FileWritten {
            file_name: "Neu.pdf".to_owned(),
            pages: vec![page("scan", 0)],
        })
        .unwrap();
    events
        .send(Event::Failed("Platte voll".to_owned()))
        .unwrap();
    harness.run_steps(3);
    assert!(harness.state().session.is_none(), "nothing of it to edit");
    assert!(
        commands.try_iter().any(
            |command| matches!(command, Command::Open(folder) if folder == Path::new("scans"))
        )
    );
    events
        .send(Event::Opened {
            dir: PathBuf::from("scans"),
            files: vec![("Scan.pdf".to_owned(), vec![page("scans/Scan.pdf", 0)])],
            unreadable: Vec::new(),
        })
        .unwrap();
    harness.run_steps(3);
    let app = harness.state();
    assert_eq!(app.session.as_ref().unwrap().dir, Path::new("scans"));
    assert!(
        matches!(&app.status, Status::Error(text) if text.contains("Platte voll") && text.contains("out-3"))
    );
}

#[test]
fn a_stopped_run_goes_back_to_its_folder_and_says_so() {
    let (mut harness, commands, events) = window();
    harness.state_mut().begin_sort(config());
    events.send(Event::SortCancelled).unwrap();
    harness.run_steps(3);
    assert!(
        commands.try_iter().any(
            |command| matches!(command, Command::Open(folder) if folder == Path::new("scans"))
        )
    );
    events
        .send(Event::Opened {
            dir: PathBuf::from("scans"),
            files: vec![("Scan.pdf".to_owned(), vec![page("scans/Scan.pdf", 0)])],
            unreadable: Vec::new(),
        })
        .unwrap();
    harness.run_steps(3);
    harness.get_by_label("Abgebrochen.");
    harness.get_by_label("Scan");
}

/// A folder of its own with a file named like a PDF, and with these `out-N` folders.
fn scan_folder(name: &str, runs: &[&str]) -> PathBuf {
    let folder =
        std::env::temp_dir().join(format!("musicscansort-start-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("Scan.pdf"), b"").unwrap();
    for run in runs {
        std::fs::create_dir(folder.join(run)).unwrap();
    }
    folder
}

fn app_for(folder: &Path) -> (App, Receiver<Command>, Sender<Event>) {
    let (commands, command_receiver) = mpsc::channel();
    let (event_sender, events) = mpsc::channel();
    let worker = Worker {
        commands,
        events,
        cancel: Arc::new(AtomicBool::new(false)),
    };
    let app = App::new(worker, config().for_folder(folder));
    (app, command_receiver, event_sender)
}

fn harness_for(app: App) -> Harness<'static, App> {
    let mut harness = Harness::builder()
        .with_size(Vec2::new(2200.0, 800.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    harness.run_steps(3);
    harness
}

#[test]
fn the_window_starts_by_showing_the_folder_and_sorts_only_when_asked() {
    let folder = scan_folder("show", &[]);
    let (mut app, commands, events) = app_for(&folder);
    app.start(Start::ShowFolder);
    let sent: Vec<Command> = commands.try_iter().collect();
    assert!(matches!(&sent[..], [Command::Open(opened)] if *opened == folder));
    assert!(!folder.join("out-1").exists());

    let mut harness = harness_for(app);
    events
        .send(Event::Opened {
            dir: folder.clone(),
            files: vec![("Scan.pdf".to_owned(), vec![page("Scan", 0)])],
            unreadable: Vec::new(),
        })
        .unwrap();
    harness.run_steps(3);
    click(&mut harness, "Sortieren", Modifiers::NONE);
    let sort = commands
        .try_iter()
        .find_map(|command| match command {
            Command::Sort(settings) => Some(settings),
            _ => None,
        })
        .expect("the button starts the run");
    assert_eq!(sort.input_dir, folder);
    assert_eq!(sort.output_dir, folder.join("out-1"));
    std::fs::remove_dir_all(&folder).unwrap();
}

#[test]
fn the_run_switch_sorts_at_once_unless_there_is_nothing_to_sort() {
    let folder = scan_folder("run", &[]);
    let (mut app, commands, _events) = app_for(&folder);
    app.start(Start::SortFolder);
    assert!(
        commands
            .try_iter()
            .any(|command| matches!(command, Command::Sort(_)))
    );

    std::fs::remove_file(folder.join("Scan.pdf")).unwrap();
    let (mut app, commands, _events) = app_for(&folder);
    app.start(Start::SortFolder);
    assert!(
        commands
            .try_iter()
            .all(|command| matches!(command, Command::Open(_)))
    );
    std::fs::remove_dir_all(&folder).unwrap();
}

#[test]
fn title_and_out_of_the_command_line_count_for_their_folder_and_out_only_once() {
    let (mut harness, _commands, _events) = window();
    let app = harness.state_mut();
    let first = app.sort_settings(Path::new("scans"));
    assert_eq!(first.output_dir, Path::new("out"));
    assert_eq!(first.title, "Stück");
    let second = app.sort_settings(Path::new("scans"));
    assert_eq!(second.output_dir, Path::new("scans").join("out-1"));
    assert_eq!(second.title, "Stück");
    let other = app.sort_settings(Path::new("andere"));
    assert_eq!(other.output_dir, Path::new("andere").join("out-1"));
    assert_eq!(other.title, "andere");
}

#[test]
fn a_finished_run_shows_its_result_and_the_choice_leads_back_to_the_scans() {
    let folder = scan_folder("choice", &["out-1", "out-2"]);
    let (mut app, commands, events) = app_for(&folder);
    app.start(Start::ShowFolder);
    let mut harness = harness_for(app);
    events
        .send(Event::Opened {
            dir: folder.clone(),
            files: vec![("Scan.pdf".to_owned(), vec![page("Scan", 0)])],
            unreadable: Vec::new(),
        })
        .unwrap();
    harness.run_steps(3);
    click(&mut harness, "Sortieren", Modifiers::NONE);
    std::fs::create_dir(folder.join("out-3")).unwrap();
    events
        .send(Event::FileWritten {
            file_name: "Neu.pdf".to_owned(),
            pages: vec![page("Scan", 0)],
        })
        .unwrap();
    events
        .send(Event::SortFinished {
            unused: Vec::new(),
            summary: SortSummary {
                other_pieces: Vec::new(),
                skipped: Vec::new(),
                file_count: 1,
                undetected_count: 0,
                duplicated_count: 0,
                blank_count: 0,
            },
        })
        .unwrap();
    harness.run_steps(3);
    assert_eq!(harness.state().folder, folder.join("out-3"));
    let names: Vec<String> = harness
        .state()
        .places()
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert_eq!(names, ["Scans", "out-3 (neuestes)", "out-2", "out-1"]);

    let choice = harness.get_by_role(Role::ComboBox).rect().center();
    press(&mut harness, choice, true, Modifiers::NONE);
    press(&mut harness, choice, false, Modifiers::NONE);
    harness.run_steps(3);
    click(&mut harness, "Scans", Modifiers::NONE);
    assert!(
        commands
            .try_iter()
            .any(|command| matches!(command, Command::Open(opened) if opened == folder))
    );
    assert_eq!(harness.state().home, folder);
    std::fs::remove_dir_all(&folder).unwrap();
}

#[test]
fn a_result_folder_cannot_be_sorted_but_its_scans_are_one_choice_away() {
    let folder = scan_folder("result", &["out-1"]);
    let result = folder.join("out-1");
    std::fs::write(result.join("Teil.pdf"), b"").unwrap();
    let (mut app, commands, events) = app_for(&result);
    app.start(Start::ShowFolder);
    assert_eq!(app.home, folder);
    let mut harness = harness_for(app);
    events
        .send(Event::Opened {
            dir: result.clone(),
            files: vec![("Teil.pdf".to_owned(), vec![page("Teil", 0)])],
            unreadable: Vec::new(),
        })
        .unwrap();
    harness.run_steps(3);
    click(&mut harness, "Sortieren", Modifiers::NONE);
    assert!(
        !commands
            .try_iter()
            .any(|command| matches!(command, Command::Sort(_)))
    );
    assert_eq!(harness.state().places().len(), 2);
    std::fs::remove_dir_all(&folder).unwrap();
}

#[test]
fn changing_the_folder_with_changes_asks_first() {
    let (mut harness, commands, _events) = window();
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    drag(&mut harness, from, onto);
    let _ = commands.try_iter().count();
    let ctx = harness.ctx.clone();
    harness
        .state_mut()
        .request(&ctx, Next::Open(PathBuf::from("scans/out-1")));
    assert!(matches!(
        harness.state().dialog,
        Some(Dialog::Unsaved(Next::Open(_)))
    ));
    assert!(
        !commands
            .try_iter()
            .any(|command| matches!(command, Command::Open(_)))
    );
}

#[test]
fn a_folder_with_nothing_to_show_gets_the_start_page() {
    let (mut harness, _commands, events) = window();
    harness.state_mut().begin_open(PathBuf::from("leer"));
    events
        .send(Event::Opened {
            dir: PathBuf::from("leer"),
            files: Vec::new(),
            unreadable: Vec::new(),
        })
        .unwrap();
    harness.run_steps(3);
    assert!(harness.state().session.is_none());
    harness.get_by_label("Ordner mit PDFs öffnen…");
    harness.get_by_label("Keine PDFs in leer");
}

#[test]
fn closing_the_folder_asks_about_changes_and_shows_the_start_page() {
    let (mut harness, _commands, _events) = window();
    click(&mut harness, "Seite 2", Modifiers::NONE);
    harness.key_press(Key::Delete);
    harness.run_steps(3);
    click(&mut harness, "Schließen", Modifiers::NONE);
    harness.run_steps(2);
    assert!(matches!(
        harness.state().dialog,
        Some(Dialog::Unsaved(Next::CloseFolder))
    ));
    click(&mut harness, "Nicht speichern", Modifiers::NONE);
    harness.run_steps(3);
    assert!(harness.state().session.is_none());
    assert!(!harness.state().may_close, "the window stays open");
    harness.get_by_label("Ordner mit PDFs öffnen…");
    assert!(harness.get_by_label("Sortieren").accesskit_node().is_disabled());
    assert!(harness.get_by_label("Schließen").accesskit_node().is_disabled());
}

#[test]
fn a_dead_pdf_thread_stops_editing_and_does_not_keep_the_window_open() {
    let (mut harness, commands, events) = window();
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    drag(&mut harness, from, onto);
    harness.state_mut().job = Some(Job::Saving);
    drop(events);
    drop(commands);
    harness.run_steps(2);
    assert!(harness.state().job.is_none());
    assert!(!harness.state().can_edit());
    close_window(&mut harness);
    assert!(harness.state().dialog.is_none());
}

#[test]
fn stopping_a_run_that_has_just_ended_simply_closes() {
    let (mut harness, _commands, _events) = window();
    harness.state_mut().dialog = Some(Dialog::StopRun);
    harness.run_steps(4);
    click(&mut harness, "Abbrechen und schließen", Modifiers::NONE);
    assert!(harness.state().may_close);
    assert!(!harness.state().close_when_stopped);
}

#[test]
fn a_new_run_does_not_inherit_an_old_stop() {
    let (mut harness, _commands, _events) = window();
    harness.state().worker.cancel.store(true, Ordering::Relaxed);
    let config = config();
    harness.state_mut().begin_sort(config);
    assert!(!harness.state().worker.cancel.load(Ordering::Relaxed));
}

#[test]
fn a_list_row_scrolled_out_of_view_takes_no_drop() {
    let (mut harness, _commands, _events) = window();
    {
        let session = harness.state_mut().session.as_mut().unwrap();
        for number in 0..60 {
            session.add_file(format!("Z{number:02}.pdf"), vec![page("z", number)]);
        }
        session.mark_saved();
    }
    harness.run_steps(3);
    let hidden = harness.get_by_label("Z59").rect();
    let list_bottom = harness
        .get_by_label("+ Neues PDF (Seiten hierher ziehen)")
        .rect()
        .top();
    assert!(
        hidden.top() > list_bottom,
        "the row is below the visible list"
    );
    let from = center_of(&harness, "Seite 2");
    // Over the status bar, in the column of the list.
    let onto = Pos2::new(hidden.center().x, 790.0);
    drag(&mut harness, from, onto);
    assert_eq!(shown(harness.state(), "A"), ["a0", "a1", "a2"]);
    assert!(!harness.state().is_dirty());
}

#[test]
fn saving_an_opened_folder_reads_it_again_and_keeps_the_file_open() {
    let (mut harness, commands, events) = window();
    harness.state_mut().session.as_mut().unwrap().origin = Origin::Opened;
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    drag(&mut harness, from, onto);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::S);
    harness.run_steps(2);
    events.send(Event::Saved).unwrap();
    harness.run_steps(2);
    assert!(
        commands
            .try_iter()
            .any(|command| matches!(command, Command::Open(_)))
    );
    events
        .send(Event::Opened {
            dir: PathBuf::from("out"),
            files: vec![
                ("A.pdf".to_owned(), vec![page("out/A.pdf", 0)]),
                ("B.pdf".to_owned(), vec![page("out/B.pdf", 0)]),
            ],
            unreadable: Vec::new(),
        })
        .unwrap();
    harness.run_steps(2);
    let app = harness.state();
    assert_eq!(app.open_file, Some(file_named(app, "A")));
    assert!(!app.is_dirty());
}

#[test]
fn files_appear_in_the_list_while_the_run_writes_them() {
    let (mut harness, _commands, events) = window();
    let app = harness.state_mut();
    app.session = Some(Session::new(Path::new("out"), Origin::Sorted));
    app.open_file = None;
    app.job = Some(Job::Sorting {
        stage: Stage::Reading,
        done: 0,
        total: 0,
        started: Instant::now(),
    });
    events
        .send(Event::Progress {
            stage: Stage::Analysing,
            done: 5,
            total: 10,
        })
        .unwrap();
    harness.run_steps(2);
    assert!(!harness.state().can_edit());
    events
        .send(Event::FileWritten {
            file_name: "Neu.pdf".to_owned(),
            pages: vec![page("scan", 0)],
        })
        .unwrap();
    harness.run_steps(2);
    harness.get_by_label("Neu *");

    events
        .send(Event::SortFinished {
            unused: vec![page("leer", 0)],
            summary: SortSummary {
                other_pieces: Vec::new(),
                skipped: Vec::new(),
                file_count: 1,
                undetected_count: 0,
                duplicated_count: 0,
                blank_count: 1,
            },
        })
        .unwrap();
    harness.run_steps(3);
    let app = harness.state();
    assert!(app.can_edit() && !app.is_dirty());
    assert_eq!(app.open_file, Some(file_named(app, "Neu")));
    harness.get_by_label("Nicht verwendet");
}

#[test]
fn the_progress_never_goes_back_from_stage_to_stage() {
    let stages = [
        Stage::Reading,
        Stage::Analysing,
        Stage::Pieces,
        Stage::Labels,
        Stage::Writing,
    ];
    let mut last = 0.0;
    for stage in stages {
        for (done, total) in [(0, 10), (10, 10)] {
            let share = overall_progress(stage, done, total);
            assert!(share >= last - 0.001);
            last = share;
        }
    }
    assert!((last - 1.0).abs() < 0.001);
}

/// The whole way with the real PDF thread on a copy of real scans: sort, show, edit, save, open the
/// result, edit, save. It moves the old file versions of that copy to the Recycle Bin.
///
/// Set `MUSICSCANSORT_E2E_SCANS` to the copy and `MUSICSCANSORT_E2E_PICTURES` to a folder for the
/// pictures of the window, then run `tools\eval\build.cmd test --release -- --ignored --nocapture
/// real_run`.
#[test]
#[ignore = "needs a folder of real scans"]
fn real_run_from_scans_to_edited_files() {
    let scans = PathBuf::from(std::env::var("MUSICSCANSORT_E2E_SCANS").unwrap());
    let pictures = PathBuf::from(std::env::var("MUSICSCANSORT_E2E_PICTURES").unwrap());
    std::fs::create_dir_all(&pictures).unwrap();
    let start_config = config().for_folder(&scans);
    let output_dir = start_config.output_dir.clone();
    let folder_count = || std::fs::read_dir(&scans).unwrap().count();
    let folders_at_start = folder_count();

    let mut app = App::new(worker::spawn(egui::Context::default()), start_config);
    app.start(Start::ShowFolder);
    let mut harness = Harness::builder()
        .with_size(Vec2::new(1920.0, 1040.0))
        .with_step_dt(1.0 / 60.0)
        .with_render_every_step(true)
        .wgpu()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    let picture = |harness: &mut Harness<'_, App>, name: &str| {
        harness.step();
        harness
            .render()
            .unwrap()
            .save(pictures.join(format!("{name}.png")))
            .unwrap();
    };
    let wait_until = |harness: &mut Harness<'_, App>, what: &str, done: &dyn Fn(&App) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(600);
        while !done(harness.state()) {
            assert!(Instant::now() < deadline, "waited too long for {what}");
            harness.step();
            std::thread::sleep(Duration::from_millis(30));
        }
        harness.run_steps(3);
    };
    let pages_shown = |app: &App| {
        let open = app.open_file.unwrap();
        let pages = app.session.as_ref().unwrap().pages(open);
        pages
            .iter()
            .all(|page| app.textures.contains_key(&PageKey::from(page)))
    };

    wait_until(&mut harness, "the scans", &|app| {
        app.job.is_none() && app.session.is_some()
    });
    wait_until(&mut harness, "their pages", &pages_shown);
    picture(&mut harness, "0-scans");
    assert_eq!(
        harness.state().session.as_ref().unwrap().origin,
        Origin::Opened
    );
    assert_eq!(folder_count(), folders_at_start, "no analysis yet");
    click(&mut harness, "Sortieren", Modifiers::NONE);
    wait_until(
        &mut harness,
        "the analysis",
        &|app| matches!(app.job, Some(Job::Sorting { stage: Stage::Analysing, done, .. }) if done > 0),
    );
    picture(&mut harness, "1-running");
    wait_until(&mut harness, "the end of the run", &|app| app.job.is_none());
    wait_until(&mut harness, "the pages", &pages_shown);
    picture(&mut harness, "2-sorted");
    assert!(matches!(harness.state().status, Status::Plain(_)));
    assert!(!harness.state().is_dirty());

    // Move the only page of the first file onto the second one.
    let names: Vec<String> = {
        let session = harness.state().session.as_ref().unwrap();
        let files = session.sorted_files();
        files.iter().map(|file| file.name().to_owned()).collect()
    };
    println!("sorted files: {names:#?}");
    let on_disk = |name: &str| output_dir.join(format!("{name}.pdf"));
    assert!(names.iter().all(|name| on_disk(name).exists()));
    let (first, second) = (names[0].clone(), names[1].clone());
    let second_count = shown(harness.state(), &second).len();
    let from = center_of(&harness, "Seite 1");
    let onto = center_of(&harness, &second);
    press(&mut harness, from, true, Modifiers::NONE);
    for step in 1..=4 {
        harness.event(InputEvent::PointerMoved(
            from + (onto - from) * (step as f32 / 4.0),
        ));
        harness.run_steps(2);
    }
    picture(&mut harness, "3-dragging");
    press(&mut harness, onto, false, Modifiers::NONE);
    harness.run_steps(2);
    assert_eq!(
        shown(harness.state(), &second).len(),
        second_count + shown(harness.state(), &first).len().max(1)
    );
    picture(&mut harness, "4-moved");

    harness.key_press_modifiers(Modifiers::COMMAND, Key::S);
    harness.run_steps(2);
    wait_until(&mut harness, "the save", &|app| app.job.is_none());
    assert!(
        matches!(harness.state().status, Status::Plain(_)),
        "saving failed"
    );
    assert!(!harness.state().is_dirty());
    if shown(harness.state(), &second).len() > second_count {
        println!("first file gone: {}", !on_disk(&first).exists());
    }

    // Now the same folder again, as a folder of finished files.
    harness.state_mut().begin_open(output_dir.clone());
    wait_until(&mut harness, "the folder", &|app| {
        app.job.is_none() && app.session.is_some()
    });
    let reopened = file_named(harness.state(), &second);
    harness.state_mut().open(reopened);
    wait_until(&mut harness, "its pages", &pages_shown);
    picture(&mut harness, "5-opened");
    let before = shown(harness.state(), &second);
    assert!(before.len() >= 2);

    // Swap the first two pages, turn one, give the file another name.
    let second_page = harness.get_by_label("Seite 2").rect();
    let first_page = harness.get_by_label("Seite 1").rect();
    // Near its left edge, as the page may reach beyond the window.
    drag(
        &mut harness,
        Pos2::new(second_page.left() + 50.0, second_page.center().y),
        Pos2::new(first_page.left() + 5.0, first_page.center().y),
    );
    let after = shown(harness.state(), &second);
    assert_eq!(after[0], before[1], "the second page is now the first");
    assert_eq!(after[1], before[0]);
    let first_page = harness.get_by_label("Seite 1").rect();
    press(
        &mut harness,
        first_page.left_center() + Vec2::X * 50.0,
        true,
        Modifiers::NONE,
    );
    press(
        &mut harness,
        first_page.left_center() + Vec2::X * 50.0,
        false,
        Modifiers::NONE,
    );
    harness.run_steps(2);
    harness.key_press(Key::R);
    harness.run_steps(3);
    harness.state_mut().ask_rename();
    harness.run_steps(2);
    if let Some(Dialog::Rename { name, .. }) = &mut harness.state_mut().dialog {
        *name = "Neuer Name".to_owned();
    }
    picture(&mut harness, "6-rename");
    harness.key_press(Key::Enter);
    harness.run_steps(3);
    picture(&mut harness, "7-edited");
    assert!(harness.state().is_dirty());

    harness.key_press_modifiers(Modifiers::COMMAND, Key::S);
    harness.run_steps(2);
    wait_until(&mut harness, "the second save", &|app| {
        app.job.is_none() && !app.is_dirty()
    });
    wait_until(&mut harness, "the saved pages", &pages_shown);
    picture(&mut harness, "8-saved");
    assert!(!on_disk(&second).exists(), "the old name is gone");
    assert!(on_disk("Neuer Name").exists());
    let app = harness.state();
    assert_eq!(app.open_file, Some(file_named(app, "Neuer Name")));
    let left_over: Vec<_> = std::fs::read_dir(&output_dir)
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp"))
        .collect();
    assert!(left_over.is_empty());
    println!("second file had {} pages: {before:?}", before.len());
    println!("output folder: {}", output_dir.display());

    // A run that is stopped writes nothing and leaves the window usable.
    let folders_before = folder_count();
    let again = harness.state().config.for_folder(&scans);
    harness.state_mut().begin_sort(again);
    wait_until(
        &mut harness,
        "the second run",
        &|app| matches!(app.job, Some(Job::Sorting { done, .. }) if done > 0),
    );
    harness.state().worker.cancel.store(true, Ordering::Relaxed);
    wait_until(&mut harness, "the stop", &|app| app.job.is_none());
    assert_eq!(harness.state().session.as_ref().unwrap().dir, scans);
    assert!(matches!(&harness.state().status, Status::Plain(text) if text == "Abgebrochen."));
    assert_eq!(folder_count(), folders_before);
    picture(&mut harness, "9-stopped");
}

fn close_window(harness: &mut Harness<'_, App>) {
    harness
        .input_mut()
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .events
        .push(egui::ViewportEvent::Close);
    // A dialog only becomes visible in the frame after its first one.
    harness.run_steps(4);
}

#[test]
fn closing_with_changes_asks_first_and_can_go_ahead_without_saving() {
    let (mut harness, commands, _events) = window();
    close_window(&mut harness);
    assert!(harness.state().dialog.is_none(), "nothing to lose yet");

    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    drag(&mut harness, from, onto);
    close_window(&mut harness);
    assert!(matches!(
        harness.state().dialog,
        Some(Dialog::Unsaved(Next::Close))
    ));

    click(&mut harness, "Abbrechen", Modifiers::NONE);
    assert!(harness.state().dialog.is_none());
    assert!(!harness.state().may_close);

    close_window(&mut harness);
    click(&mut harness, "Nicht speichern", Modifiers::NONE);
    assert!(harness.state().may_close);
    assert!(
        !commands
            .try_iter()
            .any(|command| matches!(command, Command::Save(_)))
    );
}

#[test]
fn closing_with_changes_can_save_first_and_then_closes() {
    let (mut harness, commands, events) = window();
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    drag(&mut harness, from, onto);
    close_window(&mut harness);
    // The toolbar has a button of the same name; the dialog's comes last.
    let in_dialog = harness
        .get_all_by_label("Speichern")
        .last()
        .unwrap()
        .rect()
        .center();
    press(&mut harness, in_dialog, true, Modifiers::NONE);
    press(&mut harness, in_dialog, false, Modifiers::NONE);
    harness.run_steps(2);
    assert!(!harness.state().may_close, "not before it is saved");
    assert!(
        commands
            .try_iter()
            .any(|command| matches!(command, Command::Save(_)))
    );
    events.send(Event::Saved).unwrap();
    harness.run_steps(2);
    assert!(harness.state().may_close);
}

#[test]
fn closing_during_a_run_asks_and_then_stops_it_first() {
    let (mut harness, _commands, events) = window();
    harness.state_mut().job = Some(Job::Sorting {
        stage: Stage::Analysing,
        done: 1,
        total: 9,
        started: Instant::now(),
    });
    close_window(&mut harness);
    assert!(matches!(harness.state().dialog, Some(Dialog::StopRun)));
    click(&mut harness, "Abbrechen und schließen", Modifiers::NONE);
    assert!(harness.state().worker.cancel.load(Ordering::Relaxed));
    assert!(!harness.state().may_close, "the run must stop first");
    events.send(Event::SortCancelled).unwrap();
    harness.run_steps(2);
    assert!(harness.state().may_close);
}

#[test]
fn discarding_asks_and_returns_to_the_saved_state() {
    let (mut harness, _commands, _events) = window();
    let from = center_of(&harness, "Seite 2");
    let onto = center_of(&harness, "B");
    drag(&mut harness, from, onto);
    click(&mut harness, "Alles verwerfen", Modifiers::NONE);
    assert!(harness.state().is_dirty(), "not before the answer");
    click(&mut harness, "Verwerfen", Modifiers::NONE);
    assert!(!harness.state().is_dirty());
    assert_eq!(shown(harness.state(), "A"), ["a0", "a1", "a2"]);
}
