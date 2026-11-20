use crate::instrument_map::{self, normalize_instrument_with_fallback};
use crate::ocr::{Ocr, OcrLine};
use crate::{MarkLines, PageAnalysis, PageImage};
use anyhow::{Context, Result};
use image::imageops;
use image::{GrayImage, Luma, RgbImage};
use log::{debug, info, warn};
use std::cmp::Reverse;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

const ROTATIONS: [u16; 4] = [0, 90, 180, 270];
/// Pixels below this brightness are printed ink; show-through from the back stays lighter.
const DARK_LEVEL: u8 = 140;
/// A page with a smaller share of such pixels has no music on it, however much faint show-through
/// or scan edge it shows. The shortest printed page in the samples has 0.5%.
const MIN_DARK_SHARE: f32 = 0.0015;
/// Parts have few voices; a larger number is a catalogue or handwritten number.
const MAX_VOICE: u32 = 6;
/// Bar numbers and rest counts stand at most this many staff gaps above the first staff (up to
/// 5 in the samples); printed page numbers stand higher (6.6 and more).
const STAFF_NUMBER_GAPS: f32 = 5.5;
/// The most bars one system holds, to tell bar numbers from misreads.
const BAR_STEP: u32 = 40;
/// Pixel sizes in the detection are written for this resolution and scaled to the actual one.
const REFERENCE_DPI: f32 = 150.0;

fn scaled(pixels: u32, dpi_scale: f32) -> u32 {
    ((pixels as f32 * dpi_scale).round() as u32).max(1)
}

pub fn analyse_page(
    page: &mut PageImage,
    blank_threshold: f32,
    expected_title: &str,
    ocr: &Ocr,
    dpi: u32,
) -> Result<()> {
    let started = Instant::now();
    let dpi_scale = dpi as f32 / REFERENCE_DPI;
    if is_blank(&page.image, blank_threshold, dpi_scale) {
        page.analysis.is_blank = true;
        page.corrected_image = Arc::clone(&page.image);
        page.orientation = 0;
        debug!(
            "Page {} (from {}) is blank — skipping (blank check {:.2}s).",
            page.global_order,
            page.source_file.display(),
            started.elapsed().as_secs_f32()
        );
        return Ok(());
    }

    let scan_size = page.image.dimensions();
    let gray = image_to_gray(&page.image);
    let ink_darkness = scan_ink_darkness(&gray);
    let content_crop = find_content_crop(&gray);
    if let Some((left, top, width, height)) = content_crop {
        debug!(
            "Page {}: music area {left},{top} size {width}x{height}",
            page.global_order
        );
        page.image =
            Arc::new(imageops::crop_imm(&*page.image, left, top, width, height).to_image());
    }
    let orientation_started = Instant::now();
    let (corrected, rotation, orientation_from_text) =
        detect_orientation(&page.image, ocr, Some(expected_title), dpi_scale)?;
    let skew = estimate_skew_degrees(&image_to_gray(&corrected), dpi_scale);
    let corrected = if skew.abs() >= 0.1 {
        deskew(&corrected, skew)
    } else {
        corrected
    };
    debug!(
        "Page {}: skew correction = {skew:.2}°, ink darkness = {ink_darkness:.3}",
        page.global_order
    );
    page.corrected_image = Arc::new(corrected);
    page.image = Arc::clone(&page.corrected_image);
    page.orientation = rotation;
    debug!(
        "Page {}: orientation correction = {}° ({:.2}s)",
        page.global_order,
        rotation,
        orientation_started.elapsed().as_secs_f32()
    );
    page.analysis = PageAnalysis {
        orientation_from_text,
        skew_degrees: skew,
        ink_darkness,
        content_crop,
        scan_size,
        ..Default::default()
    };
    read_upright_page(page, expected_title, ocr, dpi_scale)
}

/// Reads a page again after `harmonize_orientation` turned it by 180°.
pub fn read_turned_page(
    page: &mut PageImage,
    expected_title: &str,
    ocr: &Ocr,
    dpi: u32,
) -> Result<()> {
    page.corrected_image = Arc::new(imageops::rotate180(&*page.corrected_image));
    page.image = Arc::clone(&page.corrected_image);
    read_upright_page(page, expected_title, ocr, dpi as f32 / REFERENCE_DPI)
}

/// Reads a page or part again once the title to look for is known.
pub fn read_again(page: &mut PageImage, title: &str, ocr: &Ocr, dpi: u32) -> Result<()> {
    read_upright_page(page, title, ocr, dpi as f32 / REFERENCE_DPI)
}

/// The lines above the first staff in the top of the page, where the title block is.
pub fn header_lines(page: &PageImage) -> Vec<&OcrLine> {
    let band = page.corrected_image.height() as f32 * 0.35;
    let limit = page.analysis.staff_top.map_or(f32::MAX, |top| top as f32);
    page.analysis
        .text_lines
        .iter()
        .filter(|line| line.top < band && line.top < limit)
        .collect()
}

/// Music read as text: at least two long rules run through the line. A title has one at most,
/// when it is underlined.
pub fn crosses_staff_lines(image: &RgbImage, line: &OcrLine) -> bool {
    let (width, height) = image.dimensions();
    let (left, right) = (
        (line.left.max(0.0) as u32).min(width),
        ((line.left + line.width).max(0.0) as u32).min(width),
    );
    let (top, bottom) = (
        (line.top.max(0.0) as u32).min(height),
        ((line.top + line.height).max(0.0) as u32).min(height),
    );
    let mut rules = 0;
    let mut in_rule = false;
    for y in top..bottom {
        let dark = (left..right)
            .filter(|x| {
                let pixel = image.get_pixel(*x, y);
                (u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2])) / 3
                    < u32::from(DARK_LEVEL)
            })
            .count() as u32;
        let is_rule = dark * 2 >= (right - left).max(1);
        if is_rule && !in_rule {
            rules += 1;
        }
        in_rule = is_rule;
    }
    rules >= 2
}

/// A page with no staff, label or title in the direction found may lie across: black stripes of
/// a bad scan look like staff lines and hide the other direction. When a title or label of the
/// folder can be read after turning it, the page is turned and true is returned.
pub fn turn_to_readable_text(page: &mut PageImage, pieces: &[String], ocr: &Ocr) -> Result<bool> {
    let analysis = &page.analysis;
    if analysis.is_blank
        || analysis.staff_top.is_some()
        || analysis.instrument_raw.is_some()
        || analysis.music_title.is_some()
    {
        return Ok(false);
    }
    for angle in [90u16, 180, 270] {
        let candidate = rotate(&page.corrected_image, angle);
        for piece in pieces {
            let (_, _, title, label) = orientation_ocr_scores(&candidate, ocr, Some(piece))?;
            if title >= 0.55 || label > 0.0 {
                debug!(
                    "Page {}: readable after turning by {angle}°.",
                    page.global_order
                );
                page.corrected_image = Arc::new(candidate);
                page.image = Arc::clone(&page.corrected_image);
                page.orientation = (page.orientation + angle) % 360;
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Top of a second part on the scan: two small originals, one above the other. Like the first
/// one it starts with a title next to a part label, in the middle of the page. Lyrics may repeat
/// the title there but have no label beside them, and footers lie lower.
fn second_part_top(page: &PageImage, pieces: &[String]) -> Option<u32> {
    let analysis = &page.analysis;
    let conductor = matches!(
        analysis.instrument_normalized.as_deref(),
        Some("Direktion" | "Partitur")
    );
    if analysis.is_blank || analysis.is_score_page || conductor {
        return None;
    }
    let lines = &analysis.text_lines;
    let height = page.corrected_image.height() as f32;
    let shows_title = |line: &OcrLine| {
        pieces
            .iter()
            .any(|piece| title_in_text_score(&line.text, piece) >= 0.7)
    };
    let is_label = |line: &OcrLine| has_instrument_label(std::slice::from_ref(line));
    let top = lines
        .iter()
        .filter(|line| (0.30..0.75).contains(&(line.top / height)) && shows_title(line))
        .filter(|title| {
            lines
                .iter()
                .any(|other| (other.top - title.top).abs() <= 3.0 * title.height && is_label(other))
        })
        .map(|title| title.top - 0.5 * title.height)
        .min_by(f32::total_cmp)?;
    let part_above = lines
        .iter()
        .any(|line| line.top < top - 0.1 * height && (is_label(line) || shows_title(line)));
    part_above.then_some(top as u32)
}

/// Gives each scan with two parts one entry per part, with the whole scan as output of both, so
/// that each part can be read on its own and sorted to its piece. Returns which entries are new.
pub fn split_double_parts(pages: &mut Vec<PageImage>, pieces: &[String]) -> Vec<bool> {
    let mut parts = Vec::with_capacity(pages.len());
    let mut is_part = Vec::with_capacity(pages.len());
    for page in pages.drain(..) {
        match second_part_top(&page, pieces) {
            Some(top) => {
                debug!("Page {}: second part from y = {top}.", page.global_order);
                let height = page.corrected_image.height();
                parts.push(part_of_page(&page, 0, top));
                parts.push(part_of_page(&page, top, height - top));
                is_part.extend([true, true]);
            }
            None => {
                parts.push(page);
                is_part.push(false);
            }
        }
    }
    *pages = parts;
    is_part
}

fn part_of_page(page: &PageImage, top: u32, height: u32) -> PageImage {
    let width = page.corrected_image.width();
    let crop =
        Arc::new(imageops::crop_imm(&*page.corrected_image, 0, top, width, height).to_image());
    let mut part = page.clone();
    part.image = Arc::clone(&crop);
    part.corrected_image = crop;
    part.analysis.part_region = Some((top, height));
    part.analysis.staff_top = None;
    // Each part is read again on its own.
    part.analysis.marks = MarkLines::default();
    part.analysis.text_lines = page
        .analysis
        .text_lines
        .iter()
        .filter(|line| line.top >= top as f32 && line.top < (top + height) as f32)
        .map(|line| OcrLine {
            top: line.top - top as f32,
            ..line.clone()
        })
        .collect();
    part
}

/// Where a line of the upright page lies on the scan page before it is turned: left, top, width
/// and height as shares of the scan page. Undoes the deskew, the turn and the crops of the
/// analysis.
pub fn on_scan(page: &PageImage, line: &OcrLine) -> [f32; 4] {
    let analysis = &page.analysis;
    let (scan_width, scan_height) = analysis.scan_size;
    if scan_width == 0 || scan_height == 0 {
        return [0.0; 4];
    }
    let (crop_left, crop_top, width, height) =
        analysis
            .content_crop
            .unwrap_or((0, 0, scan_width, scan_height));
    let (width, height) = (width as f32, height as f32);
    let turned_width = if page.orientation % 180 == 90 {
        height
    } else {
        width
    };
    let tangent = if analysis.skew_degrees.abs() >= 0.1 {
        analysis.skew_degrees.to_radians().tan()
    } else {
        0.0
    };
    let part_top = analysis.part_region.map_or(0.0, |(top, _)| top as f32);
    let (left, top) = (line.left, line.top + part_top);
    let (right, bottom) = (left + line.width, top + line.height);
    let corners = [(left, top), (right, top), (left, bottom), (right, bottom)].map(|(x, y)| {
        // The deskew moved each column by its distance from the middle.
        let y = y + (x - turned_width / 2.0) * tangent;
        let (x, y) = match page.orientation {
            90 => (y, height - x),
            180 => (width - x, height - y),
            270 => (width - y, x),
            _ => (x, y),
        };
        (
            ((x + crop_left as f32) / scan_width as f32).clamp(0.0, 1.0),
            ((y + crop_top as f32) / scan_height as f32).clamp(0.0, 1.0),
        )
    });
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (1.0f32, 1.0f32, 0.0f32, 0.0f32);
    for (x, y) in corners {
        (min_x, min_y) = (min_x.min(x), min_y.min(y));
        (max_x, max_y) = (max_x.max(x), max_y.max(y));
    }
    [min_x, min_y, max_x - min_x, max_y - min_y]
}

/// Everything read from a page once `corrected_image` is the right way up. Keeps the fields of
/// `page.analysis` that were measured before.
fn read_upright_page(
    page: &mut PageImage,
    expected_title: &str,
    ocr: &Ocr,
    dpi_scale: f32,
) -> Result<()> {
    let measured = std::mem::take(&mut page.analysis);
    let staff = find_staff_geometry(&page.corrected_image, dpi_scale);
    let staff_top = staff.map(|(top, _)| top);
    debug!(
        "Page {}: staff_top = {:?} ({:.3} of height)",
        page.global_order,
        staff_top,
        staff_top.map_or(-1.0, |top| top as f32
            / page.corrected_image.height().max(1) as f32)
    );
    let clef = detect_clef(&page.corrected_image, staff_top, dpi_scale);
    debug!("Page {}: clef = {}", page.global_order, clef);
    let lines = ocr
        .read_bounded(&page.corrected_image, 1800)
        .with_context(|| format!("OCR failed on page {}", page.global_order))?;
    debug!(
        "Page {}: OCR text = {:?}",
        page.global_order,
        lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
    );
    let clef = match labelled_clef(&lines) {
        Some(labelled) => {
            debug!("Page {}: clef from label = {labelled}", page.global_order);
            labelled
        }
        None => clef,
    };
    let (width, height) = page.corrected_image.dimensions();
    let title_found = title_line(&lines, expected_title, staff_top, height).cloned();
    let title = title_found.as_ref().map(|_| expected_title.to_owned());
    debug!("Page {}: title = {:?}", page.global_order, title);
    let page_number_found = page_number_line(&lines, width, height, staff);
    let page_number = page_number_found.map(|(number, _)| number);
    let page_number_found = page_number_found.map(|(_, line)| line.clone());
    debug!(
        "Page {}: page_number = {:?}",
        page.global_order, page_number
    );
    // First pages leave room above the first staff for the title block; continuation pages
    // start their music close to the top margin.
    let staff_ratio = staff_top.map(|top| top as f32 / height.max(1) as f32);
    // Parts have few pages: a higher number is handwritten, such as the number of the stack.
    let printed_later_page = page_number.is_some_and(|number| (2..=9).contains(&number));
    // On a full page OCR noise above the first staff can look like a heading as well.
    let heading = measured.content_crop.is_some()
        && staff.is_some_and(|(top, gap)| heading_above_staff(&lines, top, gap));
    debug!(
        "Page {}: heading above staff = {heading}",
        page.global_order
    );
    let mut is_part_start = page_number == Some(1)
        || (!printed_later_page
            && staff_ratio.is_some_and(|ratio| (0.115..=0.22).contains(&ratio)))
        || (title.is_some() && staff_top.is_some())
        || heading
        || (measured.piece_evidence
            && !printed_later_page
            && staff_ratio.is_none_or(|ratio| ratio >= 0.09));
    let zone = if staff_top.is_none() {
        Zone::Cover
    } else {
        Zone::Margins
    };
    let (raw_instrument, voice_number, in_band) = extract_instrument_in(
        &lines,
        title.as_deref(),
        &clef,
        staff_top,
        width,
        height,
        zone,
    );
    let (mut raw_instrument, mut voice_number) = (raw_instrument, voice_number);
    let mut label_lines = lines.clone();
    // The lines of the extra reads of the label corners, for the marks.
    let mut extra_lines: Vec<OcrLine> = Vec::new();
    if staff_top.is_none() && raw_instrument.is_some() {
        // A page with an instrument label but without staves is a cover page of a part.
        is_part_start = true;
    }
    if is_part_start {
        if raw_instrument.is_none() || voice_number.is_none() || !in_band {
            let header_lines = read_instrument_header(&page.corrected_image, ocr, staff_top)
                .with_context(|| format!("Header OCR failed on page {}", page.global_order))?;
            debug!(
                "Page {}: upper-left label OCR = {:?}",
                page.global_order,
                header_lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
            );
            let (header_raw, header_voice, header_band) = extract_instrument_in(
                &header_lines,
                title.as_deref(),
                &clef,
                staff_top,
                width,
                height,
                Zone::Any,
            );
            let header_detection = (header_raw, header_voice);
            if names_open_key(
                raw_instrument.as_deref(),
                header_detection.0.as_deref(),
                &clef,
            ) {
                debug!(
                    "Page {}: upper-left label OCR {:?} names the key of {:?}",
                    page.global_order, header_detection.0, raw_instrument
                );
                (raw_instrument, voice_number) =
                    (header_detection.0, header_detection.1.or(voice_number));
            } else if should_replace_instrument_detection(
                &raw_instrument,
                voice_number,
                &header_detection.0,
                header_detection.1,
            ) || (!in_band && header_band && header_detection.0.is_some())
            {
                debug!(
                    "Page {}: using upper-left label OCR result {:?}, voice={:?}",
                    page.global_order, header_detection.0, header_detection.1
                );
                (raw_instrument, voice_number) = (
                    keep_known_key(raw_instrument.as_deref(), header_detection.0, &clef),
                    header_detection.1,
                );
            }
            extra_lines.extend(header_lines.iter().cloned());
            label_lines.extend(header_lines);
        }
        if let Some(raw) = raw_instrument.as_deref()
            && key_is_ambiguous(raw, &clef)
            && let Some((refined, refined_voice)) = refine_label_key(
                &page.corrected_image,
                ocr,
                &label_lines,
                raw,
                title.as_deref(),
                &clef,
                dpi_scale,
            )?
        {
            debug!(
                "Page {}: close-up label OCR adds key {refined:?}, voice={refined_voice:?}",
                page.global_order
            );
            match refined {
                Some(refined) => {
                    raw_instrument = Some(refined);
                    voice_number = refined_voice.or(voice_number);
                }
                None => voice_number = voice_number.or(refined_voice),
            }
        }
        if voice_number.is_none() {
            let (footer_detection, footer_lines) =
                detect_footer_label(page, ocr, title.as_deref(), &clef, staff_top, width, height)?;
            extra_lines.extend(footer_lines);
            if should_replace_instrument_detection(
                &raw_instrument,
                voice_number,
                &footer_detection.0,
                footer_detection.1,
            ) {
                debug!(
                    "Page {}: using bottom-center label OCR result {:?}, voice={:?}",
                    page.global_order, footer_detection.0, footer_detection.1
                );
                (raw_instrument, voice_number) = (
                    keep_known_key(raw_instrument.as_deref(), footer_detection.0, &clef),
                    footer_detection.1,
                );
            }
        }
    } else {
        // Continuation pages carry their part label in the small footer line.
        let (footer_detection, footer_lines) =
            detect_footer_label(page, ocr, title.as_deref(), &clef, staff_top, width, height)?;
        extra_lines.extend(footer_lines);
        let merged = merge_footer_detection(
            (raw_instrument.clone(), voice_number),
            footer_detection,
            &clef,
        );
        (raw_instrument, voice_number) = merged;
    }
    let is_score_page = is_part_start && looks_like_score_page(&lines, &clef);
    if is_score_page {
        debug!(
            "Page {}: several instrument names listed — treating as score page, ignoring label.",
            page.global_order
        );
        raw_instrument = None;
        voice_number = None;
    }
    let label_line = raw_instrument
        .as_deref()
        .filter(|raw| is_part_start && key_is_ambiguous(raw, &clef))
        .and_then(|raw| find_label_line(&label_lines, raw))
        .cloned();
    let label_found = raw_instrument
        .as_deref()
        .and_then(|raw| label_mark_line(&lines, &extra_lines, raw).cloned());
    debug!(
        "Page {}: instrument_raw = {:?}, voice = {:?}",
        page.global_order, raw_instrument, voice_number
    );
    let normalized = raw_instrument
        .as_deref()
        .and_then(|raw| normalize_instrument_with_fallback(raw, Some(&clef)));
    debug!(
        "Page {}: instrument_normalized = {:?}",
        page.global_order, normalized
    );
    debug!(
        "Page {}: is_part_start = {}",
        page.global_order, is_part_start
    );
    let is_final_page = detect_final_page(&page.corrected_image, dpi_scale);
    debug!(
        "Page {}: is_final_page = {}",
        page.global_order, is_final_page
    );

    let has_title_evidence = title.is_some()
        || heading
        || measured.piece_evidence
        || page_number == Some(1)
        || staff_top.is_none()
        || !centered_header_label(&lines, staff_top, width, height);
    page.analysis = PageAnalysis {
        music_title: title,
        instrument_raw: raw_instrument,
        instrument_normalized: normalized,
        voice_number,
        clef: Some(clef),
        page_number,
        is_part_start,
        is_final_page,
        has_title_evidence,
        staff_top,
        is_score_page,
        label_line,
        marks: MarkLines {
            title: title_found,
            label: label_found,
            page_number: page_number_found,
            extra: extra_lines,
        },
        last_bar: {
            let numbers: Vec<String> = lines
                .iter()
                .filter(|line| line.text.trim().chars().all(|c| c.is_ascii_digit()))
                .map(|line| {
                    format!(
                        "{}@({:.2},{:.2})",
                        line.text.trim(),
                        line.left / width.max(1) as f32,
                        line.top / height.max(1) as f32
                    )
                })
                .collect();
            let last_bar = last_bar_number(&lines, width);
            debug!(
                "Page {}: last bar = {last_bar:?} from numbers {numbers:?}",
                page.global_order
            );
            last_bar
        },
        text_lines: lines,
        staff_gap: staff.map(|(_, gap)| gap),
        ..measured
    };
    Ok(())
}

/// Part of the scan that holds the music, when a small original lies on a larger scan. A side
/// is only cut when the ink leaves more than 40% of it empty; below 35% the page is just short.
fn find_content_crop(gray: &GrayImage) -> Option<(u32, u32, u32, u32)> {
    let (width, height) = gray.dimensions();
    let mut column_ink = vec![0u32; width as usize];
    let mut row_ink = vec![0u32; height as usize];
    for (x, y, pixel) in gray.enumerate_pixels() {
        if pixel[0] < DARK_LEVEL {
            column_ink[x as usize] += 1;
            row_ink[y as usize] += 1;
        }
    }
    let (left, crop_width) = ink_span(&column_ink, height);
    let (top, crop_height) = ink_span(&row_ink, width);
    if crop_width == width && crop_height == height {
        return None;
    }
    // The staves of a small original are short. Staves over the full page width with an empty
    // part below them are the last lines of a part on a normal page.
    let staves_run_along_x =
        horizontal_lines_with(gray, 12).1 >= horizontal_lines_with(&imageops::rotate90(gray), 12).1;
    let staves_are_short = if staves_run_along_x {
        crop_width < width
    } else {
        crop_height < height
    };
    staves_are_short.then_some((left, top, crop_width, crop_height))
}

/// Start and length of the part to keep along one side of the page.
fn ink_span(ink: &[u32], across: u32) -> (u32, u32) {
    let size = ink.len() as u32;
    let minimum = (across as f32 * 0.007) as u32;
    let has_ink = |count: &u32| *count > minimum;
    let (Some(first), Some(last)) = (ink.iter().position(has_ink), ink.iter().rposition(has_ink))
    else {
        return (0, size);
    };
    let share = (last + 1 - first) as f32 / size as f32;
    if !(0.35..0.60).contains(&share) {
        return (0, size);
    }
    let margin = size * 25 / 1000;
    let start = (first as u32).saturating_sub(margin);
    let end = (last as u32 + 1 + margin).min(size);
    (start, end - start)
}

/// True when the first read left a label open that a sharper render could still fill in.
pub fn needs_label_retry(page: &PageImage) -> bool {
    let analysis = &page.analysis;
    if analysis.is_blank || analysis.is_score_page || analysis.staff_top.is_none() {
        return false;
    }
    let clef = analysis.clef.as_deref().unwrap_or("treble");
    match analysis.instrument_raw.as_deref() {
        None => true,
        Some(raw) => analysis.label_line.is_some() && key_is_ambiguous(raw, clef),
    }
}

/// The page rendered at a higher resolution, cropped, turned and straightened as the first read
/// saw it. `zoom` is the ratio of the two resolutions.
fn upright_render(page: &PageImage, render: &RgbImage, zoom: f32) -> RgbImage {
    let analysis = &page.analysis;
    let rotated = match analysis.content_crop {
        Some((left, top, width, height)) => {
            let at_zoom = |pixels: u32| (pixels as f32 * zoom) as u32;
            let left = at_zoom(left).min(render.width());
            let top = at_zoom(top).min(render.height());
            let cropped = imageops::crop_imm(render, left, top, at_zoom(width), at_zoom(height));
            rotate(&cropped.to_image(), page.orientation)
        }
        None => rotate(render, page.orientation),
    };
    let image = if analysis.skew_degrees.abs() >= 0.1 {
        deskew(&rotated, analysis.skew_degrees)
    } else {
        rotated
    };
    match analysis.part_region {
        Some((top, height)) => {
            let top = ((top as f32 * zoom) as u32).min(image.height());
            let height = ((height as f32 * zoom) as u32).min(image.height() - top);
            imageops::crop_imm(&image, 0, top, image.width(), height).to_image()
        }
        None => image,
    }
}

/// True for a continuation page without a printed page number whose bar numbers the first read
/// could not make out (§6.2).
pub fn needs_bar_retry(page: &PageImage) -> bool {
    let analysis = &page.analysis;
    !analysis.is_blank
        && !analysis.is_part_start
        && !analysis.is_score_page
        && analysis.staff_top.is_some()
        && analysis.page_number.is_none()
        && analysis.last_bar.is_none()
}

/// Reads the bar numbers at the left of the staves again on `render`, the same page rendered at
/// `label_dpi`, where the small numbers are sharper. Only that strip is read, at full size.
pub fn retry_last_bar(
    page: &mut PageImage,
    render: &RgbImage,
    ocr: &Ocr,
    dpi: u32,
    label_dpi: u32,
) -> Result<()> {
    let zoom = label_dpi as f32 / dpi as f32;
    let image = upright_render(page, render, zoom);
    let (width, height) = image.dimensions();
    let strip_width = ((width as f32 * 0.15) as u32).clamp(1, width);
    let strip = imageops::crop_imm(&image, 0, 0, strip_width, height).to_image();
    let lines = ocr.read(&strip)?;
    let last_bar = last_bar_number(&lines, width);
    debug!(
        "Page {}: last bar at {label_dpi} DPI = {last_bar:?} from {:?}",
        page.global_order,
        lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
    );
    page.analysis.last_bar = last_bar;
    Ok(())
}

/// Reads an open label again on `render`, the same page rendered at `label_dpi`.
pub fn retry_label(
    page: &mut PageImage,
    render: &RgbImage,
    ocr: &Ocr,
    dpi: u32,
    label_dpi: u32,
) -> Result<()> {
    let analysis = &page.analysis;
    let zoom = label_dpi as f32 / dpi as f32;
    let image = upright_render(page, render, zoom);
    let (width, height) = image.dimensions();
    let staff_top = analysis.staff_top.map(|top| (top as f32 * zoom) as u32);
    let title = analysis.music_title.as_deref();
    let mut clef = analysis.clef.clone().unwrap_or_else(|| "treble".to_owned());
    let mut raw = analysis.instrument_raw.clone();
    let mut voice = analysis.voice_number;

    if let (Some(current), Some(line)) = (raw.as_deref(), &analysis.label_line)
        && key_is_ambiguous(current, &clef)
    {
        let line = OcrLine {
            left: line.left * zoom,
            top: line.top * zoom,
            height: line.height * zoom,
            width: line.width * zoom,
            ..line.clone()
        };
        let dpi_scale = label_dpi as f32 / REFERENCE_DPI;
        let refined = refine_label_key(
            &image,
            ocr,
            std::slice::from_ref(&line),
            current,
            title,
            &clef,
            dpi_scale,
        )?;
        if let Some((refined_raw, refined_voice)) = refined {
            let refined_raw = refined_raw.filter(|refined| same_label(current, refined));
            (raw, voice) = (refined_raw.or(raw), voice.or(refined_voice));
        }
    }

    // Lines of the corner reads at `dpi`, for the mark of a label found in them.
    let mut corner_lines = Vec::new();
    if raw.is_none() {
        let header = read_instrument_header(&image, ocr, staff_top)?;
        let footer = read_instrument_footer(&image, ocr)?;
        corner_lines.extend(header.iter().chain(&footer).map(|line| OcrLine {
            left: line.left / zoom,
            top: line.top / zoom,
            height: line.height / zoom,
            width: line.width / zoom,
            ..line.clone()
        }));
        if looks_like_score_page(&header, &clef) || looks_like_score_page(&footer, &clef) {
            return Ok(());
        }
        if let Some(labelled) = labelled_clef(&header).or_else(|| labelled_clef(&footer)) {
            clef = labelled;
        }
        let read = |lines: &[OcrLine]| {
            let (raw, voice, _) =
                extract_instrument_in(lines, title, &clef, staff_top, width, height, Zone::Margins);
            (raw, voice)
        };
        // Same preference as the first read: continuation pages carry their label in the footer.
        let reads = if analysis.is_part_start {
            [read(&header), read(&footer)]
        } else {
            [read(&footer), read(&header)]
        };
        if let Some(found) = reads.into_iter().find(|(raw, _)| raw.is_some()) {
            (raw, voice) = found;
        }
    }

    if raw == analysis.instrument_raw && voice == analysis.voice_number {
        return Ok(());
    }
    info!(
        "Page {}: label at {label_dpi} DPI: {:?}, voice {:?} → {:?}, voice {:?}",
        page.global_order, analysis.instrument_raw, analysis.voice_number, raw, voice
    );
    debug!(
        "Page {}: instrument_raw = {:?}, voice = {:?}",
        page.global_order, raw, voice
    );
    let normalized = raw
        .as_deref()
        .and_then(|raw| normalize_instrument_with_fallback(raw, Some(&clef)));
    debug!(
        "Page {}: instrument_normalized = {:?}",
        page.global_order, normalized
    );
    if let Some(raw) = raw.as_deref()
        && page.analysis.marks.label.is_none()
    {
        page.analysis.marks.label = label_mark_line(&corner_lines, &[], raw).cloned();
    }
    page.analysis.instrument_raw = raw;
    page.analysis.instrument_normalized = normalized;
    page.analysis.voice_number = voice;
    page.analysis.clef = Some(clef);
    Ok(())
}

/// One label is the other with or without its key: "HORN" and "Eb HORN", but not "F HORN" and
/// "Eb HORN" or the misread "HORNS".
fn same_label(left: &str, right: &str) -> bool {
    let spaced = |text: &str| format!(" {} ", instrument_map::normalize_text(text));
    let (left, right) = (spaced(left), spaced(right));
    left.contains(&right) || right.contains(&left)
}

/// Share of fully black pixels among all ink pixels. Over-darkened scans fill note heads and
/// let symbols bleed together; a clean scan keeps more grey anti-aliased edges.
fn scan_ink_darkness(gray: &GrayImage) -> f32 {
    let (mut dark, mut ink) = (0u64, 0u64);
    for pixel in gray.pixels() {
        let value = pixel[0];
        if value < 200 {
            ink += 1;
            if value < 80 {
                dark += 1;
            }
        }
    }
    if ink == 0 {
        0.0
    } else {
        dark as f32 / ink as f32
    }
}

fn is_blank(image: &RgbImage, threshold: f32, dpi_scale: f32) -> bool {
    let gray = image_to_gray(image);
    let (width, height) = gray.dimensions();
    if width == 0 || height == 0 {
        return true;
    }
    let dark_share = gray.pixels().filter(|pixel| pixel[0] < DARK_LEVEL).count() as f32
        / (width as f32 * height as f32);
    if dark_share < MIN_DARK_SHARE {
        debug!("Blank check: only faint marks, dark share {dark_share:.4}");
        return true;
    }
    let integral = integral_image(&gray);
    let radius = scaled(7, dpi_scale) as i32;
    let mut dark = 0u64;
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let local = rect_sum(
                &integral,
                width,
                height,
                x - radius,
                y - radius,
                x + radius + 1,
                y + radius + 1,
            );
            let x0 = (x - radius).max(0);
            let y0 = (y - radius).max(0);
            let x1 = (x + radius + 1).min(width as i32);
            let y1 = (y + radius + 1).min(height as i32);
            let count = ((x1 - x0) * (y1 - y0)).max(1) as u64;
            let mean = local / count;
            let value = gray.get_pixel(x as u32, y as u32)[0] as u64;
            dark += u64::from(value + 10 < mean);
        }
    }
    let ratio = dark as f32 / (width as f32 * height as f32);
    debug!("Blank check: dark_ratio={ratio:.4} (threshold={threshold:.4})");
    ratio < threshold
}

fn image_to_gray(image: &RgbImage) -> GrayImage {
    GrayImage::from_fn(image.width(), image.height(), |x, y| {
        let pixel = image.get_pixel(x, y);
        let luma = (0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32)
            .round() as u8;
        Luma([luma])
    })
}

fn integral_image(image: &GrayImage) -> Vec<u64> {
    let (width, height) = image.dimensions();
    let stride = width as usize + 1;
    let mut integral = vec![0u64; stride * (height as usize + 1)];
    for y in 0..height as usize {
        let mut row_sum = 0u64;
        for x in 0..width as usize {
            row_sum += image.get_pixel(x as u32, y as u32)[0] as u64;
            integral[(y + 1) * stride + x + 1] = integral[y * stride + x + 1] + row_sum;
        }
    }
    integral
}

fn rect_sum(
    integral: &[u64],
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
) -> u64 {
    let x0 = left.clamp(0, width as i32) as usize;
    let y0 = top.clamp(0, height as i32) as usize;
    let x1 = right.clamp(0, width as i32) as usize;
    let y1 = bottom.clamp(0, height as i32) as usize;
    let stride = width as usize + 1;
    integral[y1 * stride + x1] + integral[y0 * stride + x0]
        - integral[y0 * stride + x1]
        - integral[y1 * stride + x0]
}

fn rotate(image: &RgbImage, angle: u16) -> RgbImage {
    match angle {
        0 => image.clone(),
        90 => imageops::rotate90(image),
        180 => imageops::rotate180(image),
        270 => imageops::rotate270(image),
        _ => unreachable!("only supported rotations are used"),
    }
}

fn detect_orientation(
    image: &RgbImage,
    ocr: &Ocr,
    expected_title: Option<&str>,
    dpi_scale: f32,
) -> Result<(RgbImage, u16, bool)> {
    let mut raw: Vec<f32> = ROTATIONS
        .iter()
        .map(|angle| staff_line_score(&rotate(image, *angle), 4, dpi_scale))
        .collect();
    if raw.iter().all(|score| *score < 0.3) {
        // Dense scores break the staff lines with many barlines; accept shorter line segments.
        raw = ROTATIONS
            .iter()
            .map(|angle| staff_line_score(&rotate(image, *angle), 12, dpi_scale))
            .collect();
    }
    let scores: Vec<f32> = raw.iter().map(|score| score.min(1.0)).collect();
    debug!(
        "Orientation staff scores: {:?}",
        ROTATIONS
            .iter()
            .zip(&raw)
            .map(|(angle, score)| (angle, format!("{score:.3}")))
            .collect::<Vec<_>>()
    );
    let best_axis = if raw[0] >= raw[1] { 0 } else { 90 };
    let other_axis = if best_axis == 0 { 90 } else { 0 };
    let best_raw = raw[best_axis as usize / 90];
    let other_raw = raw[other_axis as usize / 90];
    let staff_axis_is_clear =
        best_raw >= 0.05 && best_raw - other_raw > 0.02 && best_raw >= other_raw * 1.2;
    let candidates: Vec<u16> = if staff_axis_is_clear {
        vec![best_axis, (best_axis + 180) % 360]
    } else {
        ROTATIONS.to_vec()
    };
    let mut scored = Vec::new();
    for angle in candidates {
        let candidate = rotate(image, angle);
        let (readability, layout, title_score, instrument_hit) =
            orientation_ocr_scores(&candidate, ocr, expected_title)?;
        let staff_score = scores[angle as usize / 90];
        debug!(
            "Orientation candidate {angle}°: staff {staff_score:.2}, readability {readability:.2}, layout {layout:.2}, title {title_score:.2}, label {instrument_hit:.0}"
        );
        scored.push((
            angle,
            staff_score,
            readability,
            layout,
            title_score,
            instrument_hit,
        ));
    }
    let use_title_score =
        expected_title.is_some() && scored.iter().any(|candidate| candidate.4 >= 0.55);
    let text_evidence = use_title_score || scored.iter().any(|candidate| candidate.5 > 0.0);
    let mut scored = scored
        .into_iter()
        .map(|(angle, staff, readability, layout, title, hit)| {
            let score = if use_title_score {
                0.35 * staff + 0.25 * readability + 0.10 * layout + 0.30 * title + 0.20 * hit
            } else {
                0.45 * staff + 0.40 * readability + 0.15 * layout + 0.30 * hit
            };
            (score, angle)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.0.total_cmp(&left.0));
    let (best, angle) = scored[0];
    let next = scored[1].0;
    if best <= 0.0 || best - next <= 0.02 {
        if staff_axis_is_clear {
            let angle = resolve_staff_direction(image, ocr, expected_title, best_axis)?;
            debug!("Ambiguous page direction resolved to {angle}° along the staff axis.");
            return Ok((rotate(image, angle), angle, false));
        }
        warn!(
            "Orientation evidence is ambiguous (best {best:.3}, next {next:.3}); keeping original orientation."
        );
        return Ok((image.clone(), 0, false));
    }
    debug!(
        "Selected orientation correction {}° with score {:.3}.",
        angle, best
    );
    Ok((rotate(image, angle), angle, text_evidence))
}

/// Scanners feed a stack in one direction. Up to three pages in a row whose direction rests only
/// on OCR noise are turned by 180° when the pages on both sides agree on the opposite direction.
/// Returns the indexes of the turned pages.
pub fn harmonize_orientation(pages: &mut [PageImage]) -> Vec<usize> {
    const LONGEST_RUN: usize = 3;
    let analysed: Vec<usize> = (0..pages.len())
        .filter(|index| !pages[*index].analysis.is_blank)
        .collect();
    let mut turned = Vec::new();
    let mut start = 1;
    while start + 1 < analysed.len() {
        let neighbour = pages[analysed[start - 1]].orientation;
        let own = pages[analysed[start]].orientation;
        let mut end = start;
        while end < analysed.len()
            && pages[analysed[end]].orientation == own
            && !pages[analysed[end]].analysis.orientation_from_text
        {
            end += 1;
        }
        if end > start
            && end < analysed.len()
            && end - start <= LONGEST_RUN
            && (own + 180) % 360 == neighbour
            && pages[analysed[end]].orientation == neighbour
        {
            for index in &analysed[start..end] {
                info!(
                    "Page {}: orientation {own}° has no text evidence; the pages on both sides use {neighbour}° → {neighbour}°.",
                    pages[*index].global_order
                );
                pages[*index].orientation = neighbour;
                turned.push(*index);
            }
        }
        start = end.max(start + 1);
    }
    turned
}

/// Chooses between `axis` and `axis + 180°` when the staves are horizontal but the header
/// text gives no clear answer (typical for continuation pages and their footer-only labels).
fn resolve_staff_direction(
    image: &RgbImage,
    ocr: &Ocr,
    expected_title: Option<&str>,
    axis: u16,
) -> Result<u16> {
    let opposite = (axis + 180) % 360;
    let upright = rotate(image, axis);
    let flipped = rotate(image, opposite);
    if let Some(title) = expected_title {
        let upright_score = footer_title_score(&upright, ocr, title)?;
        let flipped_score = footer_title_score(&flipped, ocr, title)?;
        debug!("Footer title score: {axis}°={upright_score:.2}, {opposite}°={flipped_score:.2}");
        if upright_score.max(flipped_score) >= 0.5 && (upright_score - flipped_score).abs() >= 0.2 {
            return Ok(if upright_score > flipped_score {
                axis
            } else {
                opposite
            });
        }
    }
    // Staves start with clef and key signature on the left but end with a plain barline.
    let upright_asymmetry = staff_start_asymmetry(&upright);
    let flipped_asymmetry = staff_start_asymmetry(&flipped);
    debug!(
        "Staff start asymmetry: {axis}°={upright_asymmetry:.2}, {opposite}°={flipped_asymmetry:.2}"
    );
    Ok(if upright_asymmetry >= flipped_asymmetry {
        axis
    } else {
        opposite
    })
}

fn footer_title_score(image: &RgbImage, ocr: &Ocr, expected_title: &str) -> Result<f32> {
    let (width, height) = image.dimensions();
    let crop_x = width * 15 / 100;
    let crop_y = height * 80 / 100;
    let crop = imageops::crop_imm(
        image,
        crop_x,
        crop_y,
        (width * 70 / 100).max(1),
        (height - crop_y).max(1),
    )
    .to_image();
    let scale = (1800.0 / crop.width().max(1) as f32).clamp(1.0, 3.0);
    let resized = if scale > 1.0 {
        imageops::resize(
            &crop,
            (crop.width() as f32 * scale).round() as u32,
            (crop.height() as f32 * scale).round() as u32,
            imageops::FilterType::Lanczos3,
        )
    } else {
        crop
    };
    let lines = ocr.read(&resized)?;
    Ok(lines
        .iter()
        .map(|line| title_in_text_score(&line.text, expected_title))
        .fold(0.0, f32::max))
}

/// Best similarity of the title against any run of words of a longer text line.
pub fn title_in_text_score(text: &str, expected_title: &str) -> f32 {
    let words = text.split_whitespace().collect::<Vec<_>>();
    let expected_words = expected_title.split_whitespace().count().max(1);
    let mut best = 0.0f32;
    for length in expected_words.saturating_sub(1).max(1)..=expected_words + 1 {
        for window in words.windows(length) {
            best = best.max(title_similarity(&window.join(" "), expected_title));
        }
    }
    best
}

/// Ratio of ink at the start versus the end of the staff systems.
fn staff_start_asymmetry(image: &RgbImage) -> f32 {
    let gray = image_to_gray(image);
    let (width, height) = gray.dimensions();
    let threshold = otsu_threshold(&gray);
    let (row_pixels, _) = horizontal_lines_with(&gray, 12);
    let minimum = (width as f32 * 0.2) as u32;
    let margin = (height / 100).max(3) as i64;
    let mut in_band = vec![false; height as usize];
    for (y, pixels) in row_pixels.iter().enumerate() {
        if *pixels >= minimum {
            for band_y in (y as i64 - margin).max(0)..=(y as i64 + margin).min(height as i64 - 1) {
                in_band[band_y as usize] = true;
            }
        }
    }
    let band_rows = in_band.iter().filter(|band| **band).count() as u32;
    let mut columns = vec![0u32; width as usize];
    for (y, band) in in_band.iter().enumerate() {
        if *band {
            for x in 0..width {
                if gray.get_pixel(x, y as u32)[0] <= threshold {
                    columns[x as usize] += 1;
                }
            }
        }
    }
    // The staff starts where several rows have ink: a stray mark does not count, and a scan edge
    // or paper border is ink in almost every row.
    let is_staff = |count: &u32| *count > band_rows / 20 && *count < band_rows * 85 / 100;
    let Some(first) = columns.iter().position(is_staff) else {
        return 0.0;
    };
    let last = columns.iter().rposition(is_staff).unwrap_or(first);
    for count in &mut columns {
        if *count >= band_rows * 85 / 100 {
            *count = 0;
        }
    }
    let span = ((last - first) as f32 * 0.10) as usize;
    if span == 0 {
        return 0.0;
    }
    let start: u64 = columns[first..first + span].iter().map(|c| *c as u64).sum();
    let end: u64 = columns[last - span..=last].iter().map(|c| *c as u64).sum();
    start as f32 / (end as f32 + 1.0)
}

fn staff_line_score(image: &RgbImage, run_divisor: u32, dpi_scale: f32) -> f32 {
    let gray = image_to_gray(image);
    let width = gray.width() as usize;
    let (_, line_pixels) = horizontal_lines_with(&gray, run_divisor);
    // Staff lines also get thicker with the resolution, so divide by more than the page width.
    line_pixels as f32 / (width as f32 * 4.0 * dpi_scale)
}

fn orientation_ocr_scores(
    image: &RgbImage,
    ocr: &Ocr,
    expected_title: Option<&str>,
) -> Result<(f32, f32, f32, f32)> {
    let header_height = ((image.height() as f32 * 0.35) as u32).max(1);
    let header = imageops::crop_imm(image, 0, 0, image.width(), header_height).to_image();
    let lines = ocr.read_bounded(&header, 1200)?;
    if lines.is_empty() {
        return Ok((0.0, 0.0, 0.0, 0.0));
    }
    let quality = lines
        .iter()
        .map(|line| {
            let meaningful = line
                .text
                .chars()
                .filter(|character| !character.is_whitespace())
                .count();
            if meaningful == 0 {
                return 0.0;
            }
            let alphanumeric = line
                .text
                .chars()
                .filter(|character| character.is_alphanumeric())
                .count();
            0.5 * alphanumeric as f32 / meaningful as f32 + 0.5 * (alphanumeric.min(8) as f32 / 8.0)
        })
        .sum::<f32>()
        / lines.len() as f32;
    let readability = 0.75 * quality + 0.25 * lines.len().min(4) as f32 / 4.0;
    let weighted = lines
        .iter()
        .map(|line| line.confidence * (1.0 - line.top / header_height as f32).max(0.0))
        .sum::<f32>();
    let layout = weighted
        / (lines
            .iter()
            .map(|line| line.confidence)
            .sum::<f32>()
            .max(0.001));
    let title_score = expected_title
        .map(|expected| {
            let line_score = lines
                .iter()
                .map(|line| title_similarity(&line.text, expected))
                .fold(0.0f32, f32::max);
            let joined = lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            line_score.max(title_similarity(&joined, expected))
        })
        .unwrap_or(0.0);
    let instrument_hit = if has_instrument_label(&lines) {
        1.0
    } else {
        0.0
    };
    Ok((readability, layout, title_score, instrument_hit))
}

/// Separates glued OCR tokens such as "TRUMPET1" or "FLUTE1TITLE" into word and number.
fn split_letter_digit(text: &str) -> String {
    let mut output = String::with_capacity(text.len() + 4);
    let mut previous: Option<char> = None;
    let mut digit_run = 0usize;
    for character in text.chars() {
        if let Some(prev) = previous {
            let boundary = (prev.is_ascii_alphabetic() && character.is_ascii_digit())
                || (prev.is_ascii_digit() && character.is_ascii_alphabetic() && digit_run <= 2);
            if boundary {
                output.push(' ');
            }
        }
        digit_run = if character.is_ascii_digit() {
            digit_run + 1
        } else {
            0
        };
        output.push(character);
        previous = Some(character);
    }
    output
}

/// Continuation pages of some editions print the part label centred above the music, while
/// first pages put it at the left under the title block.
fn centered_header_label(
    lines: &[OcrLine],
    staff_top: Option<u32>,
    width: u32,
    height: u32,
) -> bool {
    let Some(staff_top) = staff_top else {
        return false;
    };
    lines.iter().any(|line| {
        line.top < staff_top as f32 - height as f32 * 0.03
            && line.left > width as f32 * 0.30
            && line.left < width as f32 * 0.60
            && has_instrument_label(std::slice::from_ref(line))
    })
}

/// A large line of text above the first staff that is not a part label: the title of a piece,
/// also of a second piece in the folder, whose words the expected title does not match. The
/// part labels of continuation pages are smaller or name an instrument.
fn heading_above_staff(lines: &[OcrLine], staff_top: u32, staff_gap: f32) -> bool {
    lines.iter().any(|line| {
        line.top < staff_top as f32
            && line.height >= 2.2 * staff_gap
            && line.text.chars().filter(|c| c.is_alphabetic()).count() >= 5
            && !has_instrument_label(std::slice::from_ref(line))
    })
}

pub fn has_instrument_label(lines: &[OcrLine]) -> bool {
    lines.iter().any(|line| {
        let tokens = line.text.split_whitespace().collect::<Vec<_>>();
        (0..tokens.len()).any(|start| {
            (start + 1..=tokens.len().min(start + 4)).any(|end| {
                let (stripped, _) = strip_voice_number(&tokens[start..end].join(" "));
                !stripped.is_empty()
                    && instrument_map::starts_with_instrument_pattern(&stripped)
                    && instrument_map::normalize_instrument(&stripped, None).is_some()
            })
        })
    })
}

/// Estimates the page skew in degrees from the sharpness of the horizontal ink projection.
fn estimate_skew_degrees(gray: &GrayImage, dpi_scale: f32) -> f32 {
    let (width, height) = gray.dimensions();
    if width < 64 || height < 64 {
        return 0.0;
    }
    let center_x = width as f32 / 2.0;
    let mut points = Vec::new();
    for y in 0..height {
        for x in (0..width).step_by(2) {
            if gray.get_pixel(x, y)[0] < 215 {
                points.push((x as f32 - center_x, y as f32));
            }
        }
    }
    if (points.len() as f32) < 200.0 * dpi_scale * dpi_scale {
        return 0.0;
    }
    // Rows as wide as at the reference resolution, so that sharpness doesn't depend on the DPI.
    let rows = height as f32 / dpi_scale;
    let sharpness = |degrees: f32| -> f64 {
        let tangent = degrees.to_radians().tan();
        let mut histogram = vec![0u32; rows.ceil() as usize + 1];
        for (x, y) in &points {
            let row = ((*y - x * tangent) / dpi_scale).round().clamp(0.0, rows) as usize;
            histogram[row] += 1;
        }
        histogram
            .iter()
            .map(|count| (*count as f64) * (*count as f64))
            .sum()
    };
    let baseline = sharpness(0.0);
    let mut best = (0.0f32, baseline);
    let mut degrees = -2.0f32;
    while degrees <= 2.001 {
        let score = sharpness(degrees);
        if score > best.1 {
            best = (degrees, score);
        }
        degrees += 0.25;
    }
    let coarse = best.0;
    let mut degrees = coarse - 0.2;
    while degrees <= coarse + 0.201 {
        let score = sharpness(degrees);
        if score > best.1 {
            best = (degrees, score);
        }
        degrees += 0.05;
    }
    if best.1 > baseline * 1.02 {
        best.0
    } else {
        0.0
    }
}

/// Removes a small skew by shifting columns vertically; only used for analysis, never for output.
fn deskew(image: &RgbImage, degrees: f32) -> RgbImage {
    let (width, height) = image.dimensions();
    let tangent = degrees.to_radians().tan();
    let center_x = width as f32 / 2.0;
    let mut output = RgbImage::from_pixel(width, height, image::Rgb([255, 255, 255]));
    for x in 0..width {
        let shift = (x as f32 - center_x) * tangent;
        for y in 0..height {
            let source = y as f32 + shift;
            if source < 0.0 || source > (height - 1) as f32 {
                continue;
            }
            let low = source.floor() as u32;
            let high = (low + 1).min(height - 1);
            let weight = source - low as f32;
            let a = image.get_pixel(x, low);
            let b = image.get_pixel(x, high);
            let mixed = [0, 1, 2].map(|channel| {
                (a[channel] as f32 * (1.0 - weight) + b[channel] as f32 * weight).round() as u8
            });
            output.put_pixel(x, y, image::Rgb(mixed));
        }
    }
    output
}

/// First staff of the page: y of its top line and the distance between lines.
fn find_staff_geometry(image: &RgbImage, dpi_scale: f32) -> Option<(u32, f32)> {
    let gray = image_to_gray(image);
    let (width, height) = gray.dimensions();
    // Bar lines split the staff lines into short segments; accept 1/16 page width segments.
    let (row_pixels, _) = horizontal_lines_with(&gray, 16);
    let row_minimum = (width as f32 * 0.30) as u32;
    let mut centers: Vec<(u32, u32)> = Vec::new();
    let same_line = scaled(2, dpi_scale);
    for (y, dark) in row_pixels
        .iter()
        .copied()
        .enumerate()
        .take((height as f32 * 0.65) as usize)
    {
        if dark >= row_minimum {
            let y = y as u32;
            if let Some(last) = centers.last_mut()
                && y.saturating_sub(last.0) <= same_line
            {
                last.0 = y;
                last.1 += 1;
                continue;
            }
            centers.push((y, 1));
        }
    }
    let line_centers = centers
        .into_iter()
        .map(|(last_y, count)| last_y - count / 2)
        .collect::<Vec<_>>();
    let min_gap = (height as f32 * 0.0025).max(3.0 * dpi_scale);
    let max_gap = height as f32 * 0.03;
    for window in line_centers.windows(5) {
        let gaps = window
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect::<Vec<_>>();
        let mean = gaps.iter().sum::<u32>() as f32 / gaps.len() as f32;
        if mean >= min_gap
            && mean <= max_gap
            && gaps
                .iter()
                .all(|gap| (*gap as f32 - mean).abs() <= (mean * 0.35).max(2.0 * dpi_scale))
        {
            return Some((window[0], mean));
        }
    }
    None
}

fn horizontal_lines_with(gray: &GrayImage, run_divisor: u32) -> (Vec<u32>, u64) {
    let (width, height) = gray.dimensions();
    let threshold = otsu_threshold(gray).max(205);
    let minimum_run = (width / run_divisor).max(15);
    let mut row_pixels = vec![0u32; height as usize];
    let mut total_pixels = 0u64;
    for y in 0..height {
        let mut run_start = None;
        for x in 0..=width {
            let is_dark = x < width && gray.get_pixel(x, y)[0] <= threshold;
            if is_dark {
                run_start.get_or_insert(x);
            } else if let Some(start) = run_start.take() {
                let run_length = x - start;
                if run_length >= minimum_run {
                    row_pixels[y as usize] += run_length;
                    total_pixels += run_length as u64;
                }
            }
        }
    }
    (row_pixels, total_pixels)
}

fn otsu_threshold(gray: &GrayImage) -> u8 {
    let mut histogram = [0u64; 256];
    for pixel in gray.pixels() {
        histogram[pixel[0] as usize] += 1;
    }
    let total = gray.width() as u64 * gray.height() as u64;
    let sum_all = histogram
        .iter()
        .enumerate()
        .map(|(value, count)| value as f64 * *count as f64)
        .sum::<f64>();
    let mut background_count = 0u64;
    let mut background_sum = 0f64;
    let mut best_variance = -1f64;
    let mut threshold = 0u8;
    for (value, count) in histogram.iter().enumerate() {
        background_count += count;
        if background_count == 0 {
            continue;
        }
        let foreground_count = total.saturating_sub(background_count);
        if foreground_count == 0 {
            break;
        }
        background_sum += value as f64 * *count as f64;
        let background_mean = background_sum / background_count as f64;
        let foreground_mean = (sum_all - background_sum) / foreground_count as f64;
        let variance = background_count as f64
            * foreground_count as f64
            * (background_mean - foreground_mean).powi(2);
        if variance > best_variance {
            best_variance = variance;
            threshold = value as u8;
        }
    }
    threshold
}

/// Parts printed for both clefs mark the label, e.g. "Bb Tuba (BC)" or "Bb Trombone 1 (TC)".
fn labelled_clef(lines: &[OcrLine]) -> Option<String> {
    lines.iter().find_map(|line| {
        let text = instrument_map::normalize_text(&line.text);
        let tokens: Vec<&str> = text.split_whitespace().collect();
        let clef = if tokens.contains(&"bc") {
            "bass"
        } else if tokens.contains(&"tc") {
            "treble"
        } else {
            return None;
        };
        (!instrument_map::match_candidates(&text).is_empty()).then(|| clef.to_owned())
    })
}

fn detect_clef(image: &RgbImage, staff_top: Option<u32>, dpi_scale: f32) -> String {
    if let Some(clef) = detect_clef_by_extent(image, dpi_scale) {
        return clef;
    }
    detect_clef_by_weight(image, staff_top, dpi_scale)
}

/// A treble clef reaches above the top and below the bottom staff line; a bass clef stays inside.
fn detect_clef_by_extent(image: &RgbImage, dpi_scale: f32) -> Option<String> {
    let (top, gap) = find_staff_geometry(image, dpi_scale)?;
    let gray = image_to_gray(image);
    let (width, height) = gray.dimensions();
    let top = top as f32;
    let lines: Vec<u32> = (0..5)
        .map(|k| (top + k as f32 * gap).round() as u32)
        .collect();
    let is_ink = |x: u32, y: u32| gray.get_pixel(x, y)[0] <= 205;
    let tolerance = scaled(1, dpi_scale);
    let has_staff_ink = |x: u32| {
        lines
            .iter()
            .filter(|line| {
                (line.saturating_sub(tolerance)..=(*line + tolerance).min(height - 1))
                    .any(|y| is_ink(x, y))
            })
            .count()
            >= 4
    };
    // A wide border is also ink between the lines; a barline or clef stem is only a thin stretch.
    let is_solid = |x: u32| {
        (0..4).all(|k| {
            let y = (top + (k as f32 + 0.5) * gap).round() as u32;
            y < height && is_ink(x, y)
        })
    };
    // A scan edge also has ink on the lines at high DPI, but only the staff continues sideways.
    let minimum_run = ((2.0 * gap) as u32).max(1);
    let staff_start = (0..width.saturating_sub(minimum_run)).find(|x| {
        let run = *x..*x + minimum_run;
        !is_solid(*x)
            && run.clone().all(&has_staff_ink)
            && run.filter(|x| is_solid(*x)).count() as u32 <= minimum_run / 2
    })?;
    let from = staff_start + scaled(2, dpi_scale);
    let to = (staff_start + (5.0 * gap) as u32).min(width);
    let count = |y0: f32, y1: f32| -> u64 {
        let y0 = y0.max(0.0) as u32;
        let y1 = (y1.max(0.0) as u32).min(height);
        let mut total = 0u64;
        for y in y0..y1 {
            for x in from..to {
                total += u64::from(is_ink(x, y));
            }
        }
        total
    };
    let bottom = top + 4.0 * gap;
    let above = count(top - 2.0 * gap, top - 0.5 * gap);
    let inner = count(top, bottom);
    let below = count(bottom + 0.5 * gap, bottom + 2.0 * gap);
    if inner == 0 {
        return None;
    }
    let extent = (above + below) as f32 / inner as f32;
    debug!("Clef extent ratio {extent:.3} (above {above}, below {below}, inner {inner})");
    Some(if extent > 0.08 { "treble" } else { "bass" }.to_owned())
}

fn detect_clef_by_weight(image: &RgbImage, staff_top: Option<u32>, dpi_scale: f32) -> String {
    let Some(staff_top) = staff_top else {
        return "treble".to_owned();
    };
    let (width, height) = image.dimensions();
    let top = staff_top.saturating_sub(scaled(10, dpi_scale));
    let bottom = (staff_top + scaled(60, dpi_scale).max(height / 20)).min(height);
    let region_width = (width as f32 * 0.12) as u32;
    let middle = top + (bottom - top) / 2;
    let mut top_dark = 0usize;
    let mut bottom_dark = 0usize;
    for y in top..bottom {
        for x in 0..region_width {
            let pixel = image.get_pixel(x, y);
            let gray = 0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32;
            if gray < 150.0 {
                if y < middle {
                    top_dark += 1;
                } else {
                    bottom_dark += 1;
                }
            }
        }
    }
    if top_dark + bottom_dark > 0 && top_dark as f32 / (top_dark + bottom_dark) as f32 > 0.70 {
        "bass".to_owned()
    } else {
        "treble".to_owned()
    }
}

/// The title stands above the first staff; below it the same words are lyrics or a footer.
#[cfg(test)]
fn extract_title(
    lines: &[OcrLine],
    expected_title: &str,
    staff_top: Option<u32>,
    page_height: u32,
) -> Option<String> {
    title_line(lines, expected_title, staff_top, page_height).map(|_| expected_title.to_owned())
}

fn title_line<'a>(
    lines: &'a [OcrLine],
    expected_title: &str,
    staff_top: Option<u32>,
    page_height: u32,
) -> Option<&'a OcrLine> {
    let limit = staff_top.map_or(f32::MAX, |top| top as f32);
    lines
        .iter()
        .filter(|line| line.top < page_height as f32 * 0.35 && line.top < limit)
        // OCR may merge the title with the part label and credits on one line.
        .map(|line| (title_in_text_score(&line.text, expected_title), line))
        .max_by(|left, right| left.0.total_cmp(&right.0))
        .filter(|(similarity, _)| *similarity >= 0.55)
        .map(|(_, line)| line)
}

pub fn title_similarity(candidate: &str, expected: &str) -> f32 {
    let candidate = instrument_map::normalize_text(candidate);
    let expected = instrument_map::normalize_text(expected);
    if candidate.is_empty() || expected.is_empty() {
        return 0.0;
    }
    if candidate.contains(&expected) || expected.contains(&candidate) {
        return candidate.len().min(expected.len()) as f32
            / candidate.len().max(expected.len()) as f32;
    }
    let distance = levenshtein(&candidate, &expected);
    1.0 - distance as f32 / candidate.len().max(expected.len()) as f32
}

fn levenshtein(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, left_char) in left.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(previous + usize::from(left_char != *right_char));
            previous = old;
        }
    }
    row[right.len()]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Zone {
    Any,
    /// Page header above the first staff or the footer strip; used for continuation pages
    /// where body text such as "3. Horn spielen" must not be mistaken for a part label.
    Margins,
    /// Upper third of a page without staff lines (cover pages).
    Cover,
}

fn extract_instrument(
    lines: &[OcrLine],
    expected_title: Option<&str>,
    clef: &str,
    staff_top: Option<u32>,
    width: u32,
    height: u32,
) -> (Option<String>, Option<u32>) {
    let (raw, voice, _) = extract_instrument_in(
        lines,
        expected_title,
        clef,
        staff_top,
        width,
        height,
        Zone::Any,
    );
    (raw, voice)
}

fn line_in_zone(line: &OcrLine, zone: Zone, staff_top: Option<u32>, height: u32) -> bool {
    let height = height as f32;
    match zone {
        Zone::Any => true,
        Zone::Cover => line.top < height * 0.35,
        Zone::Margins => {
            let header_limit = staff_top.map_or(height * 0.2, |top| top as f32 + height * 0.02);
            line.top < header_limit || line.top > height * 0.85
        }
    }
}

fn extract_instrument_in(
    lines: &[OcrLine],
    expected_title: Option<&str>,
    clef: &str,
    staff_top: Option<u32>,
    width: u32,
    height: u32,
    zone: Zone,
) -> (Option<String>, Option<u32>, bool) {
    if staff_top.is_none() && zone == Zone::Any {
        return (None, None, false);
    }
    let mut candidates = Vec::new();
    for line in lines
        .iter()
        .filter(|line| line_in_zone(line, zone, staff_top, height))
    {
        let spaced = split_letter_digit(&line.text);
        let tokens = spaced.split_whitespace().collect::<Vec<_>>();
        // "Glockenspiel, Xylophon" under a "Mallets" label lists the instruments of the part.
        let in_list = line.text.contains(',');
        for start in 0..tokens.len() {
            for end in start + 1..=tokens.len().min(start + 6) {
                let candidate = tokens[start..end].join(" ");
                if let Some(expected) = expected_title
                    && (title_similarity(&candidate, expected) >= 0.55
                        || runs_into_title(&tokens[start..end], expected))
                {
                    continue;
                }
                let (stripped, part) = strip_voice_number(&candidate);
                if stripped.is_empty() || !instrument_map::starts_with_instrument_pattern(&stripped)
                {
                    continue;
                }
                let Some(normalized) = normalize_instrument_with_fallback(&stripped, Some(clef))
                else {
                    continue;
                };
                let matching_length = instrument_map::match_candidates(&stripped)
                    .first()
                    .map(|item| item.0)
                    .unwrap_or(0);
                let intonation = stripped.to_lowercase().contains(" in ") || stripped.contains('♭');
                let left_score = (1.0 - line.left / width.max(1) as f32).max(0.0);
                let staff_score = staff_top.map_or(0.0, |staff_top| {
                    (1.0 - ((line.top - staff_top as f32).abs() / (height as f32 * 0.6))).max(0.0)
                });
                let label_band = staff_top.is_none_or(|top| {
                    line.top < top as f32 - height as f32 * 0.03 || line.top > height as f32 * 0.85
                });
                // A conductor's part may also name the instrument it is written for.
                let conductor = normalized == "Direktion";
                let rank = (
                    (label_band, !in_list, conductor),
                    part.is_some(),
                    intonation,
                    matching_length,
                    Reverse(candidate.len()),
                    left_score * 0.15 + staff_score * 0.10,
                    line.confidence,
                );
                candidates.push((rank, stripped, part, normalized));
            }
        }
    }
    candidates.sort_by(|left, right| {
        right
            .0
            .0
            .cmp(&left.0.0)
            .then_with(|| right.0.1.cmp(&left.0.1))
            .then_with(|| right.0.2.cmp(&left.0.2))
            .then_with(|| right.0.3.cmp(&left.0.3))
            .then_with(|| right.0.4.cmp(&left.0.4))
            .then_with(|| right.0.5.total_cmp(&left.0.5))
            .then_with(|| right.0.6.total_cmp(&left.0.6))
    });
    for candidate in candidates.iter().take(3) {
        debug!(
            "instrument candidate {:?} voice={:?} band={}",
            candidate.1, candidate.2, candidate.0.0.0
        );
    }
    let best_band = candidates.first().is_some_and(|candidate| candidate.0.0.0);
    let (raw, voice) = candidates
        .into_iter()
        .next()
        .map(|(_, raw, voice, _)| (Some(raw), voice))
        .unwrap_or((None, None));
    (raw, voice, best_band)
}

fn read_instrument_header(
    image: &RgbImage,
    ocr: &Ocr,
    staff_top: Option<u32>,
) -> Result<Vec<OcrLine>> {
    let Some(staff_top) = staff_top else {
        return Ok(Vec::new());
    };
    let (width, height) = image.dimensions();
    let crop_width = (width * 55 / 100).max(1);
    let crop_bottom = staff_top.saturating_add(height / 40).clamp(1, height);
    let crop = imageops::crop_imm(image, 0, 0, crop_width, crop_bottom).to_image();
    let largest = crop.width().max(crop.height());
    let scale = (1800.0 / largest as f32).min(3.0).max(1.0);
    let resized = if scale > 1.0 {
        imageops::resize(
            &crop,
            (crop.width() as f32 * scale).round() as u32,
            (crop.height() as f32 * scale).round() as u32,
            imageops::FilterType::Lanczos3,
        )
    } else {
        crop
    };
    let mut lines = ocr.read(&resized)?;
    for line in &mut lines {
        line.left /= scale;
        line.top /= scale;
        line.height /= scale;
        line.width /= scale;
    }
    Ok(lines)
}

/// A fallback read that only adds the voice number ("HORN 4") keeps the key of the first
/// read of the same instrument ("Eb HORN").
fn keep_known_key(current: Option<&str>, fallback: Option<String>, clef: &str) -> Option<String> {
    let fallback = fallback?;
    let Some(current) = current else {
        return Some(fallback);
    };
    let keyed = instrument_map::normalize_text(current);
    let plain = instrument_map::normalize_text(&fallback);
    if key_is_ambiguous(&fallback, clef)
        && !key_is_ambiguous(current, clef)
        && keyed.ends_with(&format!(" {plain}"))
    {
        Some(current.to_owned())
    } else {
        Some(fallback)
    }
}

const KEY_TOKENS: [&str; 5] = ["b", "es", "f", "c", "e"];

/// True when the label names no key but a key would change the output part, e.g. "HORN"
/// (F or Es) or "TUBA" (C, B or Es). Small "B♭"/"E♭" signs are often lost at page scale.
fn key_is_ambiguous(raw: &str, clef: &str) -> bool {
    let normalized = instrument_map::normalize_text(raw);
    if normalized
        .split_whitespace()
        .next()
        .is_none_or(|first| KEY_TOKENS.contains(&first))
    {
        return false;
    }
    let base = normalize_instrument_with_fallback(raw, Some(clef));
    ["b", "es", "f", "c"].iter().any(|key| {
        let keyed = instrument_map::normalize_instrument(&format!("{key} {raw}"), Some(clef));
        keyed.is_some() && keyed != base
    })
}

/// Re-reads the label enlarged, with room on its left for a key sign. `chars` is the label's
/// character span within the OCR line, which may also hold the title and credits.
fn read_label_closeup(
    image: &RgbImage,
    ocr: &Ocr,
    line: &OcrLine,
    chars: (usize, usize),
    dpi_scale: f32,
) -> Result<Vec<OcrLine>> {
    let (width, height) = image.dimensions();
    let text_height = line.height.max(8.0 * dpi_scale);
    let char_width = line.width / line.text.chars().count().max(1) as f32;
    let label_left = line.left + chars.0 as f32 * char_width;
    let label_right = line.left + chars.1 as f32 * char_width;
    let left = (label_left - 4.0 * text_height).max(0.0) as u32;
    let right = ((label_right + 2.0 * text_height) as u32).min(width);
    let top = (line.top - 0.6 * text_height).max(0.0) as u32;
    let bottom = ((line.top + 1.6 * text_height) as u32).min(height);
    if right <= left || bottom <= top {
        return Ok(Vec::new());
    }
    let crop = imageops::crop_imm(image, left, top, right - left, bottom - top).to_image();
    let scale = (64.0 / text_height).clamp(1.0, 6.0);
    let resized = imageops::resize(
        &crop,
        (crop.width() as f32 * scale).round() as u32,
        (crop.height() as f32 * scale).round() as u32,
        imageops::FilterType::Lanczos3,
    );
    // White margin: the text detector misses words that touch the image border.
    let mut padded = RgbImage::from_pixel(
        resized.width() + 40,
        resized.height() + 40,
        image::Rgb([255, 255, 255]),
    );
    imageops::replace(&mut padded, &resized, 20, 20);
    ocr.read(&padded)
}

fn find_label_line<'a>(lines: &'a [OcrLine], raw: &str) -> Option<&'a OcrLine> {
    let wanted = instrument_map::normalize_text(raw);
    lines.iter().find(|line| {
        instrument_map::normalize_text(&split_letter_digit(&line.text)).contains(&wanted)
    })
}

/// The line a label was read from, in the page lines or else in the lines of the corner reads. The
/// label is cleaned up ("Trompete in B" from "Trompete Il in B"), so failing the exact text a line
/// with the first word of the label, misread by two letters at most, counts.
fn label_mark_line<'a>(
    lines: &'a [OcrLine],
    corner_lines: &'a [OcrLine],
    raw: &str,
) -> Option<&'a OcrLine> {
    let normalized = instrument_map::normalize_text(raw);
    let first = normalized.split_whitespace().next()?;
    let similar = |lines: &'a [OcrLine]| {
        lines
            .iter()
            .filter_map(|line| {
                let text = instrument_map::normalize_text(&split_letter_digit(&line.text));
                let distance = text
                    .split_whitespace()
                    .map(|word| levenshtein(word, first))
                    .min()?;
                (distance <= 2).then_some((distance, line))
            })
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, line)| line)
    };
    find_label_line(lines, raw)
        .or_else(|| find_label_line(corner_lines, raw))
        .or_else(|| {
            (first.chars().count() >= 4)
                .then(|| similar(lines))
                .flatten()
        })
        .or_else(|| {
            (first.chars().count() >= 4)
                .then(|| similar(corner_lines))
                .flatten()
        })
}

/// Looks for the key of an unkeyed label ("HORN 2" → "Eb HORN 2") in a close-up of its line.
/// Returns the keyed label, or `None` with the voice number when the close-up reads the same
/// label without a key but with a voice ("Trombone" → "Trombone 1").
fn refine_label_key(
    image: &RgbImage,
    ocr: &Ocr,
    lines: &[OcrLine],
    raw: &str,
    expected_title: Option<&str>,
    clef: &str,
    dpi_scale: f32,
) -> Result<Option<(Option<String>, Option<u32>)>> {
    let wanted = instrument_map::normalize_text(raw);
    let Some(line) = find_label_line(lines, raw) else {
        return Ok(None);
    };
    // Locate the instrument word in the line; keep room for a voice number after the label.
    let lower = line.text.to_lowercase();
    let anchor = raw
        .split_whitespace()
        .filter_map(|word| lower.find(&word.to_lowercase()))
        .min()
        .unwrap_or(0);
    let start = lower[..anchor].chars().count();
    let chars = (start, start + raw.chars().count() + 3);
    let closeup = read_label_closeup(image, ocr, line, chars, dpi_scale)?;
    debug!(
        "close-up label OCR of {:?} = {:?}",
        line.text,
        closeup
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
    );
    let (width, height) = image.dimensions();
    let (refined, voice) =
        extract_instrument(&closeup, expected_title, clef, Some(0), width, height);
    let Some(refined) = refined else {
        return Ok(None);
    };
    if !key_is_ambiguous(&refined, clef) {
        return Ok(Some((Some(refined), voice)));
    }
    let same_label = instrument_map::normalize_text(&refined) == wanted;
    Ok((same_label && voice.is_some()).then_some((None, voice)))
}

/// Full scores list many different instrument names in their left margin.
fn looks_like_score_page(lines: &[OcrLine], clef: &str) -> bool {
    let mut names = HashSet::new();
    for line in lines {
        let (stripped, _) = strip_voice_number(&line.text);
        if !stripped.is_empty()
            && instrument_map::starts_with_instrument_pattern(&stripped)
            && let Some(name) = normalize_instrument_with_fallback(&stripped, Some(clef))
        {
            names.insert(name);
        }
    }
    names.len() >= 4
}

fn detect_footer_label(
    page: &PageImage,
    ocr: &Ocr,
    title: Option<&str>,
    clef: &str,
    staff_top: Option<u32>,
    width: u32,
    height: u32,
) -> Result<((Option<String>, Option<u32>), Vec<OcrLine>)> {
    let footer_lines = read_instrument_footer(&page.corrected_image, ocr)
        .with_context(|| format!("Footer OCR failed on page {}", page.global_order))?;
    debug!(
        "Page {}: bottom-center label OCR = {:?}",
        page.global_order,
        footer_lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
    );
    let (raw, voice, _) = extract_instrument_in(
        &footer_lines,
        title,
        clef,
        staff_top,
        width,
        height,
        Zone::Margins,
    );
    Ok(((raw, voice), footer_lines))
}

/// Prefers the footer label; the voice number may be taken over from the matching main detection.
fn merge_footer_detection(
    main: (Option<String>, Option<u32>),
    footer: (Option<String>, Option<u32>),
    clef: &str,
) -> (Option<String>, Option<u32>) {
    let Some(footer_raw) = &footer.0 else {
        return main;
    };
    let normalize = |raw: &str| normalize_instrument_with_fallback(raw, Some(clef));
    let same_instrument = main
        .0
        .as_deref()
        .is_some_and(|main_raw| normalize(main_raw) == normalize(footer_raw));
    if footer.1.is_none() && same_instrument {
        return (footer.0, main.1);
    }
    footer
}

fn read_instrument_footer(image: &RgbImage, ocr: &Ocr) -> Result<Vec<OcrLine>> {
    let (width, height) = image.dimensions();
    let crop_x = width * 25 / 100;
    let crop_width = (width * 50 / 100).max(1).min(width - crop_x);
    let crop_y = height * 76 / 100;
    let crop_height = (height - crop_y).max(1);
    let crop = imageops::crop_imm(image, crop_x, crop_y, crop_width, crop_height).to_image();
    let largest = crop.width().max(crop.height());
    let scale = (1800.0 / largest as f32).min(3.0).max(1.0);
    let resized = if scale > 1.0 {
        imageops::resize(
            &crop,
            (crop.width() as f32 * scale).round() as u32,
            (crop.height() as f32 * scale).round() as u32,
            imageops::FilterType::Lanczos3,
        )
    } else {
        crop
    };
    let mut lines = ocr.read(&resized)?;
    for line in &mut lines {
        line.left = line.left / scale + crop_x as f32;
        line.top = line.top / scale + crop_y as f32;
        line.height /= scale;
        line.width /= scale;
    }
    Ok(lines)
}

/// The first read left the key open ("Klarinette"), and the read of the label corner names it
/// for the same instrument ("Klarinette in Eb").
fn names_open_key(current: Option<&str>, corner: Option<&str>, clef: &str) -> bool {
    let (Some(current), Some(corner)) = (current, corner) else {
        return false;
    };
    key_is_ambiguous(current, clef) && !key_is_ambiguous(corner, clef) && same_label(corner, current)
}

/// A label line that runs into the title, e.g. "Klarinette in Bl?tenwalzer": the first letters
/// of the title word would read as a key ("Bl" for B♭).
fn runs_into_title(tokens: &[&str], title: &str) -> bool {
    let long = |word: &str| word.chars().filter(|c| c.is_alphanumeric()).count() >= 4;
    let title_words: Vec<&str> = title.split_whitespace().filter(|word| long(word)).collect();
    tokens.iter().skip(1).filter(|token| long(token)).any(|token| {
        title_words
            .iter()
            .any(|word| title_similarity(token, word) >= 0.7)
    })
}

fn should_replace_instrument_detection(
    current_instrument: &Option<String>,
    current_voice: Option<u32>,
    fallback_instrument: &Option<String>,
    fallback_voice: Option<u32>,
) -> bool {
    fallback_instrument.is_some()
        && (current_instrument.is_none() || (current_voice.is_none() && fallback_voice.is_some()))
}

/// A number that can be a voice. OCR reads the Roman "II" and "III" as "11" and "111".
fn voice_from_number(number: u32) -> Option<u32> {
    match number {
        11 => Some(2),
        111 => Some(3),
        1..=MAX_VOICE => Some(number),
        _ => None,
    }
}

/// A Roman voice after the instrument name ("Trompete II in B"), also as OCR reads it ("Il").
fn roman_voice(token: &str) -> Option<u32> {
    match token.trim_end_matches('.') {
        "I" | "l" | "|" => Some(1),
        "II" | "Il" | "lI" | "ll" => Some(2),
        "III" | "IIl" | "Ill" | "lll" => Some(3),
        "IV" | "lV" => Some(4),
        _ => None,
    }
}

fn parse_voice_token(token: &str) -> Option<u32> {
    let digits = token.trim_end_matches(['.', ')']);
    if let Ok(number) = digits.parse::<u32>() {
        return voice_from_number(number);
    }
    // OCR often reads the stroke of "1." or "2." as the letters l, I or |.
    (token.ends_with('.')
        && !digits.is_empty()
        && digits.chars().count() <= 3
        && digits
            .chars()
            .all(|character| matches!(character, 'l' | 'I' | '|')))
    .then(|| digits.chars().count() as u32)
}

/// An English ordinal voice ("1st", "2nd", "3rd", "4th"), also split by OCR or the letter-digit
/// split ("2 nd"), glued to the key ("2nd.C") or with the "1" read as a letter ("Ist").
/// Returns the index of the first token after the ordinal and the voice.
fn english_ordinal(tokens: &[String]) -> Option<(usize, u32)> {
    let suffix = |token: &str, number: u32| {
        let token = token.trim_end_matches('.').to_lowercase();
        let expected = match number {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        };
        token == expected
    };
    for index in 0..tokens.len() {
        let token = tokens[index].as_str();
        let first_token = index == 0;
        // "1st" written in one token, also "Ist" or "lst" at the start of a label.
        let glued = token
            .char_indices()
            .find(|(_, character)| !character.is_ascii_digit())
            .map(|(split, _)| token.split_at(split));
        if let Some((digits, rest)) = glued
            && !digits.is_empty()
            && let Ok(number) = digits.parse::<u32>()
            && (1..=MAX_VOICE).contains(&number)
            && suffix(rest, number)
        {
            return Some((index + 1, number));
        }
        if first_token && matches!(token.trim_end_matches('.'), "Ist" | "lst" | "1st") {
            return Some((index + 1, 1));
        }
        if let Ok(number) = token.parse::<u32>()
            && (1..=MAX_VOICE).contains(&number)
            && tokens
                .get(index + 1)
                .is_some_and(|next| suffix(next, number))
        {
            return Some((index + 2, number));
        }
    }
    None
}

fn strip_voice_number(text: &str) -> (String, Option<u32>) {
    let cleaned = text.replace(['(', ')', '[', ']'], "").trim().to_owned();
    let mut tokens = cleaned
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return (String::new(), None);
    }
    // "2nd.C Flute": OCR glues the key to the ordinal through its dot.
    let unglued = tokens
        .iter()
        .flat_map(|token| match token.split_once('.') {
            Some((head, tail))
                if !tail.is_empty()
                    && head.len() <= 4
                    && ["st", "nd", "rd", "th"]
                        .iter()
                        .any(|ending| head.to_lowercase().ends_with(ending)) =>
            {
                vec![head.to_owned(), tail.to_owned()]
            }
            _ => vec![token.clone()],
        })
        .collect::<Vec<_>>();
    if let Some((end, voice)) = english_ordinal(&unglued) {
        // "4. The Last Dance 3rd Bb Trombone": a section heading may stand before the label.
        let rest = unglued[end..].join(" ");
        if !rest.is_empty() {
            return (rest, Some(voice));
        }
    }
    let leading = tokens.first().and_then(|token| parse_voice_token(token));
    if let Some(number) = leading
        && tokens.len() > 1
        && (tokens[0].ends_with('.') || tokens[0].ends_with(')'))
    {
        tokens.remove(0);
        return (tokens.join(" "), Some(number));
    }
    if let Some((prefix, remainder)) = tokens[0].split_once('.')
        && let Some(number) = parse_voice_token(&format!("{prefix}."))
        && !remainder.is_empty()
    {
        tokens[0] = remainder.to_owned();
        return (tokens.join(" "), Some(number));
    }
    let trailing = tokens.last().and_then(|token| token.parse::<u32>().ok());
    if trailing.is_some() && tokens.len() > 1 {
        tokens.pop();
        return (tokens.join(" "), trailing.and_then(voice_from_number));
    }
    let roman = (1..tokens.len()).find_map(|index| {
        let followed_by_key_or_nothing = tokens.get(index + 1).is_none_or(|next| {
            let next = next.to_lowercase();
            next == "in" || next.starts_with("nr") || next.starts_with("no")
        });
        followed_by_key_or_nothing
            .then(|| roman_voice(&tokens[index]).map(|voice| (index, voice)))
            .flatten()
    });
    if let Some((index, voice)) = roman {
        tokens.remove(index);
        return (tokens.join(" "), Some(voice));
    }
    (tokens.join(" "), trailing.and_then(voice_from_number))
}

#[cfg(test)]
fn extract_page_number(lines: &[OcrLine], width: u32, height: u32) -> Option<u32> {
    page_number_line(lines, width, height, None).map(|(number, _)| number)
}

/// The highest trusted bar number of a page, to order pages by. Each system prints the number
/// of its first bar at the left of its staff. Single digits there are mostly time signatures.
/// OCR often loses a digit (`36` for `136`), which only makes a number smaller, and sometimes
/// reads a number that is no bar (`331`). So a number of two or three digits counts only when a
/// number above it is up to `BAR_STEP` bars smaller or one below it up to `BAR_STEP` bars larger,
/// and the highest of at least two such numbers is the page's bar. The bars of one page all lie
/// before those of the next page, so the highest ones order the pages.
fn last_bar_number(lines: &[OcrLine], width: u32) -> Option<u32> {
    let mut numbers: Vec<(f32, u32)> = lines
        .iter()
        .filter(|line| line.left < width as f32 * 0.15)
        .filter_map(|line| {
            let text = line.text.trim();
            ((2..=3).contains(&text.len())
                && !text.starts_with('0')
                && text.chars().all(|c| c.is_ascii_digit()))
            .then(|| text.parse::<u32>().ok())
            .flatten()
            .map(|number| (line.top, number))
        })
        .collect();
    numbers.sort_by(|left, right| left.0.total_cmp(&right.0));
    let step = |lower: u32, higher: u32| (1..=BAR_STEP).contains(&higher.wrapping_sub(lower));
    let supported: Vec<u32> = numbers
        .iter()
        .enumerate()
        .filter(|(index, (_, number))| {
            numbers[..*index].iter().any(|(_, above)| step(*above, *number))
                || numbers[index + 1..].iter().any(|(_, below)| step(*number, *below))
        })
        .map(|(_, (_, number))| *number)
        .collect();
    (supported.len() >= 2)
        .then(|| supported.iter().copied().max())
        .flatten()
}

/// A number right above or beside the first staff is a bar number or the count of a
/// multi-bar rest, not a page number. `staff` is the top of the first staff and its line gap.
fn at_first_staff(line: &OcrLine, staff: Option<(u32, f32)>) -> bool {
    staff.is_some_and(|(top, gap)| {
        let top = top as f32;
        let bottom = line.top + line.height;
        bottom > top - STAFF_NUMBER_GAPS * gap && line.top < top + 4.0 * gap
    })
}

fn page_number_line(
    lines: &[OcrLine],
    width: u32,
    height: u32,
    staff: Option<(u32, f32)>,
) -> Option<(u32, &OcrLine)> {
    for line in lines {
        let text = line.text.trim();
        // OCR may merge a page number with the label next to it: "2 Posaune 3".
        let merged = text
            .split_once(' ')
            .map(|(first, _)| first)
            .filter(|first| matches!(*first, "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
        let text = merged.unwrap_or(text);
        if text.is_empty() || text.len() > 3 || !text.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let number = text.parse::<u32>().ok()?;
        if number == 0 {
            continue;
        }
        let center_x = line.left / width.max(1) as f32;
        let center_y = line.top / height.max(1) as f32;
        let in_margin = center_y < 0.08 || center_y > 0.92;
        let in_allowed_column =
            center_x < 0.20 || (0.40..0.60).contains(&center_x) || center_x > 0.80;
        if in_margin && in_allowed_column {
            if let Some((top, gap)) = staff {
                debug!(
                    "page number candidate {number}: {:.1} staff gaps above the first staff",
                    (top as f32 - line.top - line.height) / gap.max(0.1)
                );
            }
            if at_first_staff(line, staff) {
                continue;
            }
            return Some((number, line));
        }
    }
    None
}

fn detect_final_page(image: &RgbImage, dpi_scale: f32) -> bool {
    let (width, height) = image.dimensions();
    let x_start = (width as f32 * 0.70) as u32;
    let y_start = (height as f32 * 0.85) as u32;
    let region_height = height - y_start;
    let min_component_height = (region_height as f32 * 0.30) as u32;
    let min_run = (region_height / 4).max(scaled(20, dpi_scale));
    let mut candidate_columns = Vec::new();
    for x in x_start..width {
        let mut longest = 0u32;
        let mut current = 0u32;
        for y in y_start..height {
            let pixel = image.get_pixel(x, y);
            let gray = 0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32;
            if gray < 150.0 {
                current += 1;
                longest = longest.max(current);
            } else {
                current = 0;
            }
        }
        if longest >= min_run.max(min_component_height) {
            candidate_columns.push(x);
        }
    }
    let mut component_centers = Vec::new();
    let mut start = None;
    let mut previous = 0;
    for x in candidate_columns
        .into_iter()
        .chain(std::iter::once(width.saturating_add(1)))
    {
        if start.is_none() {
            start = Some(x);
        } else if x > previous + 1 {
            let left = start.take().expect("component start exists");
            let component_width = previous - left + 1;
            if component_width <= scaled(8, dpi_scale) {
                component_centers.push(left + component_width / 2);
            }
            start = Some(x);
        }
        previous = x;
    }
    let close_pair = scaled(2, dpi_scale)..=scaled(30, dpi_scale);
    component_centers.windows(2).any(|pair| {
        let gap = pair[1] - pair[0];
        close_pair.contains(&gap) && gap as f32 / (width.max(1) as f32) < 0.03
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn oriented_page(order: usize, orientation: u16, from_text: bool) -> PageImage {
        let image = Arc::new(RgbImage::from_pixel(4, 4, Rgb([255, 255, 255])));
        PageImage {
            source_file: "test.pdf".into(),
            source_page_index: order,
            global_order: order,
            id: order,
            image: Arc::clone(&image),
            corrected_image: image,
            orientation,
            analysis: PageAnalysis {
                orientation_from_text: from_text,
                ..Default::default()
            },
        }
    }

    #[test]
    fn a_page_without_text_evidence_follows_its_neighbours_direction() {
        let mut pages = vec![
            oriented_page(0, 180, false),
            oriented_page(1, 0, false),
            oriented_page(2, 180, false),
            oriented_page(3, 0, true),
            oriented_page(4, 180, false),
        ];
        harmonize_orientation(&mut pages);
        let orientations: Vec<u16> = pages.iter().map(|page| page.orientation).collect();
        assert_eq!(orientations, vec![180, 180, 180, 0, 180]);
    }

    #[test]
    fn several_pages_in_a_row_follow_their_neighbours_and_are_reported() {
        let mut pages = vec![
            oriented_page(0, 270, false),
            oriented_page(1, 90, false),
            oriented_page(2, 90, false),
            oriented_page(3, 270, false),
        ];
        assert_eq!(harmonize_orientation(&mut pages), vec![1, 2]);
        assert!(pages.iter().all(|page| page.orientation == 270));

        // A long stack in the other direction is not a mistake.
        let mut pages: Vec<PageImage> = [270, 90, 90, 90, 90, 270]
            .iter()
            .enumerate()
            .map(|(order, angle)| oriented_page(order, *angle, false))
            .collect();
        assert!(harmonize_orientation(&mut pages).is_empty());
    }

    #[test]
    fn an_instrumentation_list_does_not_outrank_the_part_label() {
        let line = |text: &str, top: f32| OcrLine {
            text: text.to_owned(),
            confidence: 0.8,
            left: 40.0,
            top,
            height: 14.0,
            width: 100.0,
        };
        let lines = [
            line("Mallets", 100.0),
            line("Glockenspiel, Xylophon", 120.0),
        ];
        let (raw, _) = extract_instrument(&lines, None, "treble", Some(300), 600, 800);
        assert_eq!(raw.as_deref(), Some("Mallets"));
    }

    #[test]
    fn voice_numbers_are_read_as_roman_numerals_and_kept_small() {
        let voice = |text: &str| strip_voice_number(text).1;
        assert_eq!(voice("Trompete II in B"), Some(2));
        assert_eq!(voice("Trompete Il in B"), Some(2));
        assert_eq!(voice("Tenorhorn I Nr."), Some(1));
        assert_eq!(voice("Klarinette III"), Some(3));
        assert_eq!(voice("Trompete 11"), Some(2));
        assert_eq!(
            strip_voice_number("Trompete II in B").0,
            "Trompete in B",
            "the numeral is not part of the name"
        );
        // Catalogue and handwritten numbers.
        assert_eq!(voice("Tenorhorn 506"), None);
        assert_eq!(voice("Direktion 10"), None);
        assert_eq!(voice("Trompete 8"), None);
        assert_eq!(voice("20. Trompete"), None);
        // A key or a word is no numeral.
        assert_eq!(voice("Horn in F"), None);
        assert_eq!(voice("I"), None);
    }

    #[test]
    fn a_conductor_label_outranks_the_instrument_it_is_written_for() {
        let line = |text: &str, top: f32| OcrLine {
            text: text.to_owned(),
            confidence: 0.8,
            left: 40.0,
            top,
            height: 14.0,
            width: 100.0,
        };
        let lines = [line("Direktion", 100.0), line("1. Flügelhorn B", 120.0)];
        let (raw, voice) = extract_instrument(&lines, None, "treble", Some(300), 600, 800);
        assert_eq!(raw.as_deref(), Some("Direktion"));
        assert_eq!(voice, None);
    }

    #[test]
    fn the_title_only_counts_above_the_first_staff() {
        let line = |text: &str, top: f32| OcrLine {
            text: text.to_owned(),
            confidence: 0.8,
            left: 40.0,
            top,
            height: 14.0,
            width: 100.0,
        };
        let title = "Ein Stück";
        assert!(extract_title(&[line("Ein Stück", 50.0)], title, Some(100), 800).is_some());
        // The same word in the lyrics under the first staff.
        assert!(extract_title(&[line("Ein Stück", 150.0)], title, Some(100), 800).is_none());
        assert!(extract_title(&[line("Ein Stück", 150.0)], title, None, 800).is_some());
    }

    #[test]
    fn a_large_heading_above_the_staff_marks_a_first_page() {
        let line = |text: &str, top: f32, height: f32| OcrLine {
            text: text.to_owned(),
            confidence: 0.8,
            left: 40.0,
            top,
            height,
            width: 100.0,
        };
        let heading = |lines: &[OcrLine]| heading_above_staff(lines, 200, 10.0);
        assert!(heading(&[line("Andere Weise", 50.0, 30.0)]));
        // Too small, below the staff, a part label, or only a stack number.
        assert!(!heading(&[line("von Hans Meierhof", 50.0, 15.0)]));
        assert!(!heading(&[line("Andere Weise", 300.0, 30.0)]));
        assert!(!heading(&[line("Flügelhorn", 50.0, 30.0)]));
        assert!(!heading(&[line("27", 50.0, 30.0)]));
    }

    #[test]
    fn faint_show_through_is_a_blank_page() {
        let mut page = RgbImage::from_pixel(400, 600, Rgb([250, 250, 250]));
        // Lots of contrast for the local check, but nothing dark: lines of grey show-through.
        for y in (20..580).step_by(6) {
            for x in 20..380 {
                page.put_pixel(x, y, Rgb([190, 190, 190]));
            }
        }
        assert!(is_blank(&page, 0.005, 1.0));
        for y in (20..580).step_by(6) {
            for x in 20..380 {
                page.put_pixel(x, y, Rgb([40, 40, 40]));
            }
        }
        assert!(!is_blank(&page, 0.005, 1.0));
    }

    fn page_with_ink(left: u32, top: u32, right: u32, bottom: u32) -> GrayImage {
        let mut page = GrayImage::from_pixel(1000, 1400, Luma([255]));
        for y in (top..bottom).step_by(4) {
            for x in left..right {
                page.put_pixel(x, y, Luma([0]));
            }
        }
        page
    }

    #[test]
    fn a_small_original_on_a_large_scan_is_cut_out() {
        // A5 in the middle of an A4 scan: only the sides are empty.
        let (left, top, width, height) =
            find_content_crop(&page_with_ink(240, 30, 760, 1370)).expect("narrow music");
        assert_eq!((top, height), (0, 1400));
        assert!(left < 240 && left + width > 760 && width < 600);
        // A5 in a corner: both directions are cut.
        let (left, top, width, height) =
            find_content_crop(&page_with_ink(480, 0, 990, 790)).expect("music in a corner");
        assert!(left < 480 && width < 600);
        assert!(top == 0 && height < 900);
        // A full page and the sample layouts stay as they are.
        assert_eq!(find_content_crop(&page_with_ink(50, 50, 950, 1350)), None);
        assert_eq!(find_content_crop(&page_with_ink(190, 50, 950, 1250)), None);
        // A short last page of a part: one staff at the top of a full-width page.
        assert_eq!(find_content_crop(&page_with_ink(50, 100, 950, 300)), None);
        // The same with half a page of music, also on a scan that lies on its side.
        let half_page = page_with_ink(50, 100, 950, 800);
        assert_eq!(find_content_crop(&half_page), None);
        assert_eq!(find_content_crop(&imageops::rotate90(&half_page)), None);
    }

    impl OcrLine {
        fn with_text(self, text: &str) -> Self {
            Self {
                text: text.to_owned(),
                ..self
            }
        }
    }

    /// The box around the dark pixels of an image, as an OCR line.
    fn dark_box(image: &RgbImage) -> OcrLine {
        let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
        for (x, y, pixel) in image.enumerate_pixels() {
            if pixel[0] < 128 {
                (left, top) = (left.min(x), top.min(y));
                (right, bottom) = (right.max(x + 1), bottom.max(y + 1));
            }
        }
        OcrLine {
            text: String::new(),
            confidence: 1.0,
            left: left as f32,
            top: top as f32,
            width: (right - left) as f32,
            height: (bottom - top) as f32,
        }
    }

    #[test]
    fn a_line_of_the_upright_page_is_found_again_on_the_scan() {
        // A label far from the middle of the scan, so that a wrong deskew would move it.
        let (box_left, box_top, box_right, box_bottom) = (300.0, 450.0, 360.0, 480.0);
        let mut scan = RgbImage::from_pixel(400, 600, Rgb([255, 255, 255]));
        for y in box_top as u32..box_bottom as u32 {
            for x in box_left as u32..box_right as u32 {
                scan.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        let crop = (20, 40, 360, 520);
        for orientation in ROTATIONS {
            for skew in [0.0, 2.0] {
                for part_top in [None, Some(10)] {
                    let cropped = imageops::crop_imm(&scan, crop.0, crop.1, crop.2, crop.3);
                    let turned = rotate(&cropped.to_image(), orientation);
                    let upright = if skew > 0.0 {
                        deskew(&turned, skew)
                    } else {
                        turned
                    };
                    let shown = match part_top {
                        Some(top) => imageops::crop_imm(
                            &upright,
                            0,
                            top,
                            upright.width(),
                            upright.height() - top,
                        )
                        .to_image(),
                        None => upright.clone(),
                    };
                    let page = PageImage {
                        analysis: PageAnalysis {
                            scan_size: (400, 600),
                            content_crop: Some(crop),
                            skew_degrees: skew,
                            part_region: part_top.map(|top| (top, upright.height() - top)),
                            ..Default::default()
                        },
                        ..oriented_page(0, orientation, false)
                    };
                    let [left, top, width, height] = on_scan(&page, &dark_box(&shown));
                    let (left, width) = (left * 400.0, width * 400.0);
                    let (top, height) = (top * 600.0, height * 600.0);
                    let case = format!("turn {orientation}, skew {skew}, part {part_top:?}");
                    let center = (left + width / 2.0, top + height / 2.0);
                    let expected = ((box_left + box_right) / 2.0, (box_top + box_bottom) / 2.0);
                    assert!((center.0 - expected.0).abs() < 2.5, "{case}: x {center:?}");
                    assert!((center.1 - expected.1).abs() < 2.5, "{case}: y {center:?}");
                    assert!(
                        (width - (box_right - box_left)).abs() < 8.0,
                        "{case}: {width}"
                    );
                    assert!(
                        (height - (box_bottom - box_top)).abs() < 8.0,
                        "{case}: {height}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_mark_of_a_cleaned_up_label_finds_its_line() {
        let line = |text: &str, top: f32| {
            OcrLine {
                top,
                ..dark_box(&RgbImage::from_pixel(1, 1, Rgb([0, 0, 0])))
            }
            .with_text(text)
        };
        let lines = [line("Marsch", 10.0), line("Trompete Il in B", 20.0)];
        let corner = [line("Fl?gelhorn inB", 30.0)];
        let top = |raw| label_mark_line(&lines, &corner, raw).map(|line| line.top);
        assert_eq!(top("Trompete Il in B"), Some(20.0));
        assert_eq!(top("Trompete in B"), Some(20.0));
        assert_eq!(top("Fligelhorn in B"), Some(30.0));
        assert_eq!(top("Posaune"), None);
        // "Tuba" is three letters away from "inB".
        assert_eq!(top("Tuba"), None);
    }

    #[test]
    fn a_page_number_can_start_the_label_line() {
        let line = |text: &str| OcrLine {
            text: text.to_owned(),
            confidence: 0.8,
            left: 40.0,
            top: 20.0,
            height: 14.0,
            width: 200.0,
        };
        let number = |text: &str| extract_page_number(&[line(text)], 1000, 1400);
        assert_eq!(number("2 Posaune 3, Hkl."), Some(2));
        assert_eq!(number("2"), Some(2));
        // A voice with a dot, a first page and running text are no page numbers.
        assert_eq!(number("2. Posaune"), None);
        assert_eq!(number("1 Posaune"), None);
        assert_eq!(number("Posaune 2"), None);
    }

    #[test]
    fn a_number_at_the_first_staff_is_no_page_number() {
        let at = |top: f32| OcrLine {
            text: "107".to_owned(),
            confidence: 0.8,
            left: 40.0,
            top,
            height: 14.0,
            width: 30.0,
        };
        // First staff at y = 80 with a line gap of 10 px.
        let staff = Some((80, 10.0));
        let number = |line: OcrLine| page_number_line(&[line], 1000, 1400, staff).map(|(n, _)| n);
        // A bar number or rest count just above the staff, and a bar number beside it.
        assert_eq!(number(at(56.0)), None);
        assert_eq!(number(at(90.0)), None);
        // A page number in the margin well above the music.
        assert_eq!(number(at(10.0)), Some(107));
    }

    #[test]
    fn the_bar_number_of_a_page_needs_support_from_its_neighbours() {
        let at = |text: &str, left: f32, top: f32| OcrLine {
            text: text.to_owned(),
            confidence: 0.8,
            left,
            top,
            height: 10.0,
            width: 20.0,
        };
        // Bar numbers with a time signature "4", a lost digit "36" (136), a stray "331" and a
        // rest count far right.
        let lines = [
            at("36", 60.0, 40.0),
            at("4", 60.0, 60.0),
            at("146", 60.0, 300.0),
            at("331", 60.0, 460.0),
            at("162", 60.0, 710.0),
            at("8", 500.0, 120.0),
        ];
        assert_eq!(last_bar_number(&lines, 1000), Some(162));
        // A single number has no neighbour to support it.
        assert_eq!(last_bar_number(&lines[..2], 1000), None);
        let falling = [at("161", 30.0, 100.0), at("53", 30.0, 300.0), at("10", 30.0, 500.0)];
        assert_eq!(last_bar_number(&falling, 1000), None);
    }

    #[test]
    fn reads_english_ordinal_voices() {
        let read = |text: &str| strip_voice_number(&split_letter_digit(text));
        assert_eq!(read("2nd C Flute"), ("C Flute".to_owned(), Some(2)));
        assert_eq!(read("1st Bb Trumpet"), ("Bb Trumpet".to_owned(), Some(1)));
        assert_eq!(read("2nd.C Flute"), ("C Flute".to_owned(), Some(2)));
        assert_eq!(read("1 st C Trombone"), ("C Trombone".to_owned(), Some(1)));
        assert_eq!(read("Ist Eh Horn"), ("Eh Horn".to_owned(), Some(1)));
        assert_eq!(read("4th F Horn"), ("F Horn".to_owned(), Some(4)));
        assert_eq!(
            read("4. The Last Dance 3rd Bh Trombone"),
            ("Bh Trombone".to_owned(), Some(3))
        );
        // German labels keep their rules.
        assert_eq!(
            read("1. Trompete in B"),
            ("Trompete in B".to_owned(), Some(1))
        );
    }

    #[test]
    fn a_keyed_label_must_be_the_same_label() {
        assert!(same_label("HORN", "Eb HORN"));
        assert!(same_label("Trombone", "Bb Trombone"));
        assert!(!same_label("Eb HORN", "F HORN"));
        assert!(!same_label("HORN", "TUBA"));
        assert!(!same_label("Trombone", "Trombonel"));
    }

    #[test]
    fn label_retry_only_reads_pages_with_an_open_label() {
        let page = |analysis: PageAnalysis| PageImage {
            analysis: PageAnalysis {
                clef: Some("treble".to_owned()),
                staff_top: Some(100),
                ..analysis
            },
            ..oriented_page(0, 0, false)
        };
        let label = |raw: &str| Some(raw.to_owned());
        let line = OcrLine {
            text: "Horn".to_owned(),
            confidence: 0.8,
            left: 40.0,
            top: 100.0,
            height: 14.0,
            width: 60.0,
        };

        assert!(needs_label_retry(&page(PageAnalysis::default())));
        assert!(needs_label_retry(&page(PageAnalysis {
            instrument_raw: label("Horn"),
            label_line: Some(line.clone()),
            ..Default::default()
        })));

        // The key was read, or there is no line to take a close-up of.
        assert!(!needs_label_retry(&page(PageAnalysis {
            instrument_raw: label("Eb Horn"),
            label_line: Some(line),
            ..Default::default()
        })));
        assert!(!needs_label_retry(&page(PageAnalysis {
            instrument_raw: label("Horn"),
            ..Default::default()
        })));
        // A missing voice alone is not worth a second read; grouping fills it in.
        assert!(!needs_label_retry(&page(PageAnalysis {
            instrument_raw: label("Fagott"),
            is_part_start: true,
            ..Default::default()
        })));
        assert!(!needs_label_retry(&page(PageAnalysis {
            is_blank: true,
            ..Default::default()
        })));
        assert!(!needs_label_retry(&page(PageAnalysis {
            is_score_page: true,
            ..Default::default()
        })));
        let mut cover = page(PageAnalysis::default());
        cover.analysis.staff_top = None;
        assert!(!needs_label_retry(&cover));
    }

    #[test]
    fn a_fallback_label_without_key_keeps_the_known_key() {
        assert_eq!(
            keep_known_key(Some("Eb HORN"), Some("HORN".to_owned()), "treble").as_deref(),
            Some("Eb HORN")
        );
        assert_eq!(
            keep_known_key(Some("Eb HORN"), Some("TUBA".to_owned()), "treble").as_deref(),
            Some("TUBA")
        );
        assert_eq!(
            keep_known_key(None, Some("HORN".to_owned()), "treble").as_deref(),
            Some("HORN")
        );
    }

    #[test]
    fn finds_staff_after_header_but_not_titleless_music_body() {
        let mut page = RgbImage::from_pixel(600, 800, Rgb([255, 255, 255]));
        for y in [350, 360, 370, 380, 390] {
            for x in 20..580 {
                page.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        assert_eq!(
            find_staff_geometry(&page, 1.0).map(|(top, _)| top),
            Some(350)
        );
    }

    #[test]
    fn clef_extent_ignores_a_scan_edge_left_of_the_staff() {
        let mut page = RgbImage::from_pixel(1000, 1000, Rgb([255, 255, 255]));
        for y in 0..1000 {
            page.put_pixel(100, y, Rgb([190, 190, 190]));
        }
        for top in [200, 220, 240, 260, 280] {
            for y in top..top + 3 {
                for x in 300..900 {
                    page.put_pixel(x, y, Rgb([0, 0, 0]));
                }
            }
        }
        assert_eq!(detect_clef_by_extent(&page, 1.0).as_deref(), Some("bass"));

        for y in 160..330 {
            for x in 320..330 {
                page.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        assert_eq!(detect_clef_by_extent(&page, 1.0).as_deref(), Some("treble"));
    }

    #[test]
    fn clef_extent_ignores_a_solid_border_left_of_the_staff() {
        for border in [100..160, 240..300] {
            let mut page = RgbImage::from_pixel(1000, 1000, Rgb([255, 255, 255]));
            for y in 0..1000 {
                for x in border.clone() {
                    page.put_pixel(x, y, Rgb([0, 0, 0]));
                }
            }
            for top in [200, 220, 240, 260, 280] {
                for y in top..top + 3 {
                    for x in 300..900 {
                        page.put_pixel(x, y, Rgb([0, 0, 0]));
                    }
                }
            }
            assert_eq!(detect_clef_by_extent(&page, 1.0).as_deref(), Some("bass"));
        }
    }

    #[test]
    fn clef_extent_keeps_a_barline_at_the_staff_start() {
        let mut page = RgbImage::from_pixel(1000, 1000, Rgb([255, 255, 255]));
        for top in [200, 220, 240, 260, 280] {
            for y in top..top + 3 {
                for x in 300..900 {
                    page.put_pixel(x, y, Rgb([0, 0, 0]));
                }
            }
        }
        for y in 200..283 {
            for x in 300..303 {
                page.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        assert_eq!(detect_clef_by_extent(&page, 1.0).as_deref(), Some("bass"));

        for y in 160..330 {
            for x in 320..330 {
                page.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        assert_eq!(detect_clef_by_extent(&page, 1.0).as_deref(), Some("treble"));
    }

    #[test]
    fn centered_header_label_is_continuation_evidence() {
        let line = |text: &str, left: f32| OcrLine {
            text: text.to_owned(),
            confidence: 0.8,
            left,
            top: 40.0,
            height: 14.0,
            width: 100.0,
        };
        assert!(centered_header_label(
            &[line("Bb Tuba (BC)", 300.0)],
            Some(300),
            800,
            1000
        ));
        assert!(!centered_header_label(
            &[line("Bb Tuba (BC)", 60.0)],
            Some(300),
            800,
            1000
        ));
    }

    #[test]
    fn splits_glued_label_tokens() {
        assert_eq!(split_letter_digit("TRUMPET1"), "TRUMPET 1");
        assert_eq!(split_letter_digit("FLUTE1TITLE"), "FLUTE 1 TITLE");
        assert_eq!(split_letter_digit("No. 1234D"), "No. 1234D");
    }

    #[test]
    fn deskew_restores_horizontal_staff_lines() {
        let mut page = RgbImage::from_pixel(800, 600, Rgb([255, 255, 255]));
        for x in 0..800u32 {
            let tilt = ((x as f32 - 400.0) * 0.01) as i32;
            for line in 0..5i32 {
                let y = 200 + line * 12 + tilt;
                page.put_pixel(x, y as u32, Rgb([0, 0, 0]));
            }
        }
        let skew = estimate_skew_degrees(&image_to_gray(&page), 1.0);
        assert!((skew.abs() - 0.57).abs() < 0.2, "skew {skew}");
        let straight = deskew(&page, skew);
        assert!(find_staff_geometry(&straight, 1.0).is_some());
    }

    /// A printed page drawn at the reference resolution and at twice that resolution.
    fn page_at_both_resolutions(draw: impl Fn(&mut RgbImage)) -> (RgbImage, RgbImage) {
        let mut page = RgbImage::from_pixel(1240, 1754, Rgb([255, 255, 255]));
        draw(&mut page);
        let double = imageops::resize(&page, 2480, 3508, imageops::FilterType::Nearest);
        (page, double)
    }

    fn draw_staff(page: &mut RgbImage, top: u32) {
        for line in 0..5 {
            for y in top + line * 12..top + line * 12 + 2 {
                for x in 100..1140 {
                    page.put_pixel(x, y, Rgb([0, 0, 0]));
                }
            }
        }
    }

    #[test]
    fn staff_and_clef_do_not_depend_on_the_dpi() {
        let (page, double) = page_at_both_resolutions(|page| draw_staff(page, 300));
        let mut double_with_gap = double.clone();
        // A scanned line can break up into rows that are further apart at a higher resolution.
        for line in 0..5 {
            for y in [601 + line * 24, 602 + line * 24] {
                for x in 200..2280 {
                    double_with_gap.put_pixel(x, y, Rgb([255, 255, 255]));
                }
            }
        }
        assert_eq!(find_staff_geometry(&double_with_gap, 1.0), None);
        let (top, gap) = find_staff_geometry(&page, 1.0).unwrap();
        let (double_top, double_gap) = find_staff_geometry(&double_with_gap, 2.0).unwrap();
        assert!(double_top.abs_diff(top * 2) <= 3, "{top} {double_top}");
        assert!((double_gap - gap * 2.0).abs() <= 1.5, "{gap} {double_gap}");
        assert_eq!(
            detect_clef_by_extent(&page, 1.0),
            detect_clef_by_extent(&double, 2.0)
        );
    }

    #[test]
    fn staff_line_score_does_not_depend_on_the_dpi() {
        let (page, double) = page_at_both_resolutions(|page| draw_staff(page, 300));
        let score = staff_line_score(&page, 4, 1.0);
        let double_score = staff_line_score(&double, 4, 2.0);
        assert!(
            (score - double_score).abs() < 0.001,
            "{score} {double_score}"
        );
        assert!(staff_line_score(&double, 4, 1.0) > score * 1.5);
    }

    #[test]
    fn skew_does_not_depend_on_the_dpi() {
        let (page, double) = page_at_both_resolutions(|page| {
            for x in 100..1140u32 {
                let tilt = ((x as f32 - 620.0) * 0.01) as i32;
                for line in 0..5i32 {
                    let y = 300 + line * 12 + tilt;
                    page.put_pixel(x, y as u32, Rgb([0, 0, 0]));
                }
            }
        });
        let skew = estimate_skew_degrees(&image_to_gray(&page), 1.0);
        let double_skew = estimate_skew_degrees(&image_to_gray(&double), 2.0);
        assert!(skew.abs() > 0.3, "skew {skew}");
        assert!((skew - double_skew).abs() <= 0.1, "{skew} {double_skew}");
    }

    #[test]
    fn final_barline_does_not_depend_on_the_dpi() {
        // 20 pixels apart at the reference resolution: past a fixed limit of 30 when doubled.
        let (page, double) = page_at_both_resolutions(|page| {
            for x in [1000, 1020] {
                for y in 1550..1720 {
                    page.put_pixel(x, y, Rgb([0, 0, 0]));
                }
            }
        });
        assert!(detect_final_page(&page, 1.0));
        assert!(detect_final_page(&double, 2.0));
        assert!(!detect_final_page(&double, 1.0));
    }

    #[test]
    fn pixel_sizes_stay_the_same_at_the_reference_dpi() {
        for pixels in [1, 2, 7, 8, 10, 20, 30, 60] {
            assert_eq!(scaled(pixels, 1.0), pixels);
            assert_eq!(scaled(pixels, 2.0), pixels * 2);
        }
        assert_eq!(scaled(1, 0.5), 1);
    }

    #[test]
    fn strips_voice_numbers_before_and_after_instrument() {
        assert_eq!(
            strip_voice_number("1. Trompete in B"),
            ("Trompete in B".to_owned(), Some(1))
        );
        assert_eq!(
            strip_voice_number("Trompete 2"),
            ("Trompete".to_owned(), Some(2))
        );
        assert_eq!(
            strip_voice_number("1.Tuba in C"),
            ("Tuba in C".to_owned(), Some(1))
        );
    }

    #[test]
    fn title_comparison_ignores_diacritics_and_ocr_typos() {
        assert!(title_similarity("Ein grosses Stück", "Ein großes Stück") > 0.8);
    }

    #[test]
    fn final_barline_requires_a_close_pair_of_narrow_vertical_strokes() {
        let mut page = RgbImage::from_pixel(1000, 1000, Rgb([255, 255, 255]));
        for x in [800, 820] {
            for y in 900..980 {
                page.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        assert!(detect_final_page(&page, 1.0));
        for y in 900..980 {
            page.put_pixel(820, y, Rgb([255, 255, 255]));
        }
        assert!(!detect_final_page(&page, 1.0));
    }

    #[test]
    fn finds_mapped_instrument_with_voice_number_before_name() {
        let lines = [OcrLine {
            text: "1. Trompete in B".to_owned(),
            confidence: 0.8,
            left: 40.0,
            top: 350.0,
            height: 20.0,
            width: 100.0,
        }];
        let (raw, voice) = extract_instrument(&lines, None, "treble", Some(350), 600, 800);
        assert_eq!(raw.as_deref(), Some("Trompete in B"));
        assert_eq!(voice, Some(1));
    }

    #[test]
    fn reads_ocr_header_typo_as_instrument() {
        let lines = [OcrLine {
            text: "1. Fl?te".to_owned(),
            confidence: 0.8,
            left: 40.0,
            top: 320.0,
            height: 20.0,
            width: 100.0,
        }];
        let (raw, voice) = extract_instrument(&lines, None, "treble", Some(350), 600, 800);
        assert_eq!(raw.as_deref(), Some("Fl?te"));
        assert_eq!(voice, Some(1));
    }

    #[test]
    fn ignores_page_number_and_noise_before_an_instrument_label() {
        let lines = [OcrLine {
            text: "22. x:1. Horn".to_owned(),
            confidence: 0.8,
            left: 40.0,
            top: 350.0,
            height: 20.0,
            width: 100.0,
        }];
        let (raw, voice) = extract_instrument(&lines, None, "treble", Some(350), 600, 800);
        assert_eq!(raw.as_deref(), Some("Horn"));
        assert_eq!(voice, None);
    }

    #[test]
    fn reads_stroke_lookalikes_as_voice_numbers() {
        assert_eq!(
            strip_voice_number("l. Klarinette in B"),
            ("Klarinette in B".to_owned(), Some(1))
        );
        assert_eq!(
            strip_voice_number("II. Posaune"),
            ("Posaune".to_owned(), Some(2))
        );
        assert_eq!(
            strip_voice_number("l.Trompete"),
            ("Trompete".to_owned(), Some(1))
        );
    }

    #[test]
    fn finds_title_inside_a_longer_footer_line() {
        assert!(
            title_in_text_score(
                "Ein grosses Stück - 1. Altsaxophon in Eb",
                "Ein großes Stück"
            ) > 0.8
        );
        assert!(title_in_text_score("Altsaxophon in Eb", "Ein großes Stück") < 0.5);
    }

    #[test]
    fn continuation_pages_ignore_body_text_but_read_the_footer() {
        let lines = [
            OcrLine {
                text: "2.x: 3. Horn spielen".to_owned(),
                confidence: 0.8,
                left: 40.0,
                top: 400.0,
                height: 20.0,
                width: 100.0,
            },
            OcrLine {
                text: "Ein großes Stück - 1. Altsaxophon in Eb".to_owned(),
                confidence: 0.8,
                left: 200.0,
                top: 770.0,
                height: 12.0,
                width: 100.0,
            },
        ];
        let body =
            extract_instrument_in(&lines, None, "treble", Some(100), 600, 800, Zone::Margins);
        assert_eq!(body.0.as_deref(), Some("Altsaxophon in Eb"));
        assert_eq!(body.1, Some(1));
    }

    #[test]
    fn footer_label_keeps_the_voice_of_the_matching_main_detection() {
        let merged = merge_footer_detection(
            (Some("Altsaxophon in Eb".to_owned()), Some(2)),
            (Some("Altsaxophon in Eb".to_owned()), None),
            "treble",
        );
        assert_eq!(merged.1, Some(2));
        let other = merge_footer_detection(
            (Some("Horn".to_owned()), Some(3)),
            (Some("Altsaxophon in Eb".to_owned()), None),
            "treble",
        );
        assert_eq!(other.1, None);
    }

    #[test]
    fn prefers_a_fallback_label_that_recovers_the_voice_number() {
        assert!(should_replace_instrument_detection(
            &Some("Horn".to_owned()),
            None,
            &Some("Horn in F".to_owned()),
            Some(1)
        ));
        assert!(!should_replace_instrument_detection(
            &Some("Horn in F".to_owned()),
            Some(2),
            &Some("Horn in F".to_owned()),
            Some(1)
        ));
        assert!(should_replace_instrument_detection(
            &None,
            None,
            &Some("Klarinette".to_owned()),
            None
        ));
    }

    #[test]
    fn a_label_line_running_into_the_title_gives_no_key() {
        let tokens = |text: &'static str| text.split_whitespace().collect::<Vec<_>>();
        let title = "Blumenpolka";
        assert!(runs_into_title(&tokens("Klarinette in Bl?menpolka"), title));
        assert!(!runs_into_title(&tokens("Klarinette in Bb"), title));
        // A key before a title that starts with the same letter stays a key.
        assert!(!runs_into_title(&tokens("Trompete in B"), "Bunter Abend"));
        let line = OcrLine {
            text: "Klarinette in Bl?menpolka Max Mustermann".to_owned(),
            confidence: 0.8,
            left: 40.0,
            top: 40.0,
            height: 14.0,
            width: 600.0,
        };
        let (raw, _, _) = extract_instrument_in(
            &[line],
            Some(title),
            "treble",
            Some(300),
            1000,
            1400,
            Zone::Any,
        );
        assert_eq!(raw.as_deref(), Some("Klarinette"));
    }

    #[test]
    fn the_corner_read_names_the_key_the_first_read_left_open() {
        assert!(names_open_key(Some("Klarinette"), Some("Klarinette In Eb"), "treble"));
        assert!(!names_open_key(Some("Klarinette in Bb"), Some("Klarinette In Eb"), "treble"));
        assert!(!names_open_key(Some("Klarinette"), Some("Trompete in Bb"), "treble"));
    }
}
