use crate::PageImage;
use crate::analysis::{
    crosses_staff_lines, has_instrument_label, header_lines, title_in_text_score, title_similarity,
};
use crate::instrument_map::normalize_text;
use crate::ocr::OcrLine;
use log::{debug, info};
use std::collections::HashMap;

/// Two readings this similar are the same title.
const SAME_TITLE: f32 = 0.75;
/// A reading this close to the expected title is a misread of it, not another piece.
const MISREAD_OF_EXPECTED: f32 = 0.7;
/// Two titles this similar cannot be told apart by OCR, so they count as one piece.
const SAME_PIECE: f32 = 0.55;
/// The similarity at which a page shows a title of the folder. Stricter than the check for the
/// expected title alone: titles can share words, and half a title may fit the wrong piece better.
const TITLE_MATCH: f32 = 0.7;
/// Two header lines this similar with the same digits are the same printed line.
const SAME_HEADER_LINE: f32 = 0.8;
/// A heading that shows up together with the title on more than this share of its pages is a
/// composer or a subtitle. The title of another piece and the expected title exclude each other.
const MAX_SHARE_WITH_TITLE: f32 = 0.2;
const LARGE_HEADING_GAPS: f32 = 2.2;
/// A title block needs this share of the page above the first staff. Text above music that starts
/// higher up is a direction on a continuation page ("spielen", "a tempo").
const MIN_ROOM_FOR_TITLE: f32 = 0.09;
/// Titles of a folder come from the same kind of edition, so another title is not much smaller
/// than the expected one where that is read.
const MIN_SIZE_OF_EXPECTED_TITLE: f32 = 0.6;

/// Words of a credit line ("Musik u. Arr.: ...", "von ...").
const CREDIT_WORDS: &[&str] = &[
    "text", "musik", "music", "arr", "komp", "von", "by", "words", "verlag", "edition",
];
/// A form or tempo on its own is no title.
const FORM_WORDS: &[&str] = &[
    "marsch", "march", "polka", "walzer", "waltz", "ballade", "swing", "solo", "trio", "intro",
    "moderato", "allegro", "andante",
];

/// The piece a page belongs to, and whether its title (not just other header text) says so.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Evidence {
    pub piece: usize,
    pub from_title: bool,
}

#[derive(Default)]
struct Heading {
    readings: Vec<String>,
    with_expected_title: usize,
    first_page: usize,
}

/// Works out the pieces in a folder from the largest heading on each page. The expected title
/// always comes first; pages without any evidence stay with it.
pub fn discover(pages: &[PageImage], expected_title: &str) -> Vec<String> {
    let min_height = expected_title_height(pages, expected_title) * MIN_SIZE_OF_EXPECTED_TITLE;
    let mut headings: Vec<Heading> = Vec::new();
    for (page_index, page) in pages.iter().enumerate() {
        if page.analysis.is_blank {
            continue;
        }
        let Some(reading) = largest_heading(page, min_height) else {
            continue;
        };
        debug!("Page {}: largest heading = {reading:?}", page.global_order);
        if title_similarity(&reading, expected_title) >= MISREAD_OF_EXPECTED {
            continue;
        }
        let index = headings
            .iter()
            .position(|heading| title_similarity(&heading.readings[0], &reading) >= SAME_TITLE)
            .unwrap_or_else(|| {
                headings.push(Heading {
                    first_page: page_index,
                    ..Default::default()
                });
                headings.len() - 1
            });
        headings[index].readings.push(reading);
        if page.analysis.music_title.is_some() {
            headings[index].with_expected_title += 1;
        }
    }
    headings.sort_by_key(|heading| std::cmp::Reverse(heading.readings.len()));

    let mut pieces = vec![expected_title.to_owned()];
    let add_piece = |pieces: &mut Vec<String>, heading: &Heading| {
        let count = heading.readings.len();
        let share = heading.with_expected_title as f32 / count as f32;
        let name = piece_name(&heading.readings);
        let distinct = pieces
            .iter()
            .all(|piece| title_similarity(piece, &name) < SAME_PIECE);
        if share <= MAX_SHARE_WITH_TITLE && distinct {
            info!("Another piece in this folder: {name:?} ({count} page(s) with this heading).");
            pieces.push(name);
        }
    };
    let (repeated, single): (Vec<&Heading>, Vec<&Heading>) = headings
        .iter()
        .partition(|heading| heading.readings.len() >= 2);
    for heading in repeated {
        add_piece(&mut pieces, heading);
    }
    // OCR noise rarely repeats but can look like text, so a heading on a single page must read
    // cleanly and the page must not already belong to another piece.
    let evidence = assign(pages, &pieces);
    for heading in single {
        if evidence[heading.first_page].is_none() && is_clear_title(&heading.readings[0]) {
            add_piece(&mut pieces, heading);
        }
    }
    pieces
}

/// Median height of the lines that show the expected title; 0 when it is not read anywhere.
fn expected_title_height(pages: &[PageImage], expected_title: &str) -> f32 {
    let mut heights: Vec<f32> = pages
        .iter()
        .filter(|page| page.analysis.music_title.is_some())
        .filter_map(|page| {
            header_lines(page)
                .into_iter()
                .max_by(|left, right| {
                    title_in_text_score(&left.text, expected_title)
                        .total_cmp(&title_in_text_score(&right.text, expected_title))
                })
                .map(|line| line.height)
        })
        .collect();
    heights.sort_by(f32::total_cmp);
    heights.get(heights.len() / 2).copied().unwrap_or(0.0)
}

/// The tallest line with letters above the first staff, cleaned of numbers and stray marks, when
/// it is neither a part label nor a credit. Headings merged with such text are skipped: the title
/// is read clean on other pages, and `assign` finds it in the merged lines as well.
fn largest_heading(page: &PageImage, min_height: f32) -> Option<String> {
    let gap = page.analysis.staff_gap?;
    let room = page.analysis.staff_top? as f32 / page.corrected_image.height().max(1) as f32;
    if room < MIN_ROOM_FOR_TITLE {
        return None;
    }
    let lines = header_lines(page);
    let tallest = lines
        .iter()
        .filter(|line| {
            letters(&line.text) >= 5 && !crosses_staff_lines(&page.corrected_image, line)
        })
        .max_by(|left, right| left.height.total_cmp(&right.height))?;
    if tallest.height < min_height
        || tallest.height < LARGE_HEADING_GAPS * gap
        || has_instrument_label(std::slice::from_ref(*tallest))
    {
        return None;
    }
    clean_heading(&tallest.text)
}

fn clean_heading(text: &str) -> Option<String> {
    let normalized = normalize_text(text);
    let words: Vec<&str> = normalized.split_whitespace().collect();
    if words.iter().any(|word| CREDIT_WORDS.contains(word))
        || (words.len() == 1 && FORM_WORDS.contains(&words[0]))
    {
        return None;
    }
    let kept: Vec<&str> = text
        .split_whitespace()
        .filter(|token| letters(token) >= 2 && !token.to_lowercase().starts_with("nr"))
        .collect();
    let cleaned = kept.join(" ");
    // A title starts with a capital; directions and half-read titles usually don't.
    let capital_first = cleaned
        .chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(char::is_uppercase);
    (letters(&cleaned) >= 5 && capital_first).then_some(cleaned)
}

fn letters(text: &str) -> usize {
    text.chars().filter(|c| c.is_alphabetic()).count()
}

/// The reading to show in file names: the most frequent one, preferably with all letters read.
fn piece_name(readings: &[String]) -> String {
    let complete: Vec<&String> = readings.iter().filter(|text| !text.contains('?')).collect();
    let usable = if complete.is_empty() {
        readings.iter().collect()
    } else {
        complete
    };
    let mut counts: HashMap<&String, usize> = HashMap::new();
    for reading in &usable {
        *counts.entry(*reading).or_default() += 1;
    }
    // `rev`: of equally frequent readings `max_by_key` takes the last, so this is the first read.
    let best = usable
        .iter()
        .rev()
        .max_by_key(|reading| counts[**reading])
        .map_or(String::new(), |reading| reading.to_string());
    title_case_capitals(&best)
}

/// "THOMAS-POLKA" → "Thomas-Polka"; other spelling stays as printed.
fn title_case_capitals(text: &str) -> String {
    if text.chars().any(char::is_lowercase) {
        return text.to_owned();
    }
    let mut result = String::with_capacity(text.len());
    let mut word_start = true;
    for character in text.chars() {
        if word_start {
            result.push(character);
        } else {
            result.extend(character.to_lowercase());
        }
        word_start = !character.is_alphanumeric() && character != '?';
    }
    result
}

/// Several words of plain letters with capitals only in front, like "Am Schönen Fluss". Music read
/// as text mixes capitals and signs ("Ar#e", "plloeeR") or comes out in capitals ("POST SNLS").
fn is_clear_title(name: &str) -> bool {
    let clear_word = |word: &&str| {
        word.chars()
            .all(|c| c.is_alphabetic() || "-'.?,".contains(c))
            && word.chars().skip(1).all(|c| !c.is_uppercase())
    };
    let words: Vec<&str> = name.split_whitespace().collect();
    words.len() >= 2
        && letters(name) >= 8
        && words.iter().all(|word| clear_word(word))
        && words.iter().filter(|word| letters(word) >= 3).count() >= 2
}

/// For each page, the piece its header names. The title decides first; a page whose title was not
/// read takes the piece whose pages print the same other header lines, such as a catalogue number
/// or a credit that only this piece has.
pub fn assign(pages: &[PageImage], pieces: &[String]) -> Vec<Option<Evidence>> {
    let mut evidence: Vec<Option<Evidence>> = pages
        .iter()
        .map(|page| {
            if page.analysis.is_blank {
                return None;
            }
            shown_title(&header_lines(page), pieces).map(|piece| Evidence {
                piece,
                from_title: true,
            })
        })
        .collect();
    if pieces.len() < 2 {
        return evidence;
    }

    let printed: Vec<Vec<String>> = pages.iter().map(printed_lines).collect();
    let by_piece = |piece: usize| -> Vec<&Vec<String>> {
        evidence
            .iter()
            .zip(&printed)
            .filter(|(found, _)| found.is_some_and(|found| found.piece == piece))
            .map(|(_, lines)| lines)
            .collect()
    };
    let known: Vec<Vec<&Vec<String>>> = (0..pieces.len()).map(by_piece).collect();
    let from_lines: Vec<Option<usize>> = printed
        .iter()
        .zip(&evidence)
        .map(|(lines, found)| {
            if found.is_some() {
                None
            } else {
                piece_of_lines(lines, &known)
            }
        })
        .collect();
    for (index, piece) in from_lines.into_iter().enumerate() {
        if let Some(piece) = piece {
            debug!(
                "Page {}: header lines belong to piece {:?}.",
                pages[index].global_order, pieces[piece]
            );
            evidence[index] = Some(Evidence {
                piece,
                from_title: false,
            });
        }
    }
    evidence
}

/// The best matching title among the header lines.
fn shown_title(lines: &[&OcrLine], pieces: &[String]) -> Option<usize> {
    let score = |piece: &String| {
        lines
            .iter()
            .map(|line| title_in_text_score(&line.text, piece))
            .fold(0.0, f32::max)
    };
    pieces
        .iter()
        .map(score)
        .enumerate()
        .filter(|(_, score)| *score >= TITLE_MATCH)
        .fold(
            None,
            |best: Option<(usize, f32)>, (piece, score)| match best {
                Some((_, top)) if top >= score => best,
                _ => Some((piece, score)),
            },
        )
        .map(|(piece, _)| piece)
}

/// Header lines that can tell pieces apart: no part labels, and with words or a number to compare.
fn printed_lines(page: &PageImage) -> Vec<String> {
    if page.analysis.is_blank {
        return Vec::new();
    }
    header_lines(page)
        .into_iter()
        .filter(|line| !has_instrument_label(std::slice::from_ref(*line)))
        .map(|line| normalize_text(&line.text))
        .filter(|text| letters(text) >= 5 || digits(text).len() >= 3)
        .collect()
}

fn digits(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

fn same_line(left: &str, right: &str) -> bool {
    digits(left) == digits(right) && title_similarity(left, right) >= SAME_HEADER_LINE
}

/// A line votes for a piece when at least two of its pages print it and no other piece does.
fn piece_of_lines(lines: &[String], known: &[Vec<&Vec<String>>]) -> Option<usize> {
    let mut votes = vec![0usize; known.len()];
    for line in lines {
        let printing: Vec<usize> = known
            .iter()
            .enumerate()
            .filter(|(_, pages)| {
                pages
                    .iter()
                    .filter(|page| page.iter().any(|other| same_line(line, other)))
                    .count()
                    >= 2
            })
            .map(|(piece, _)| piece)
            .collect();
        if let [piece] = printing[..] {
            votes[piece] += 1;
        }
    }
    let top = *votes.iter().max()?;
    let mut winners = votes.iter().enumerate().filter(|(_, count)| **count == top);
    match (top, winners.next(), winners.next()) {
        (1.., Some((piece, _)), None) => Some(piece),
        _ => None,
    }
}

/// A page without evidence belongs to the piece before it, or the one after it at the start.
/// A piece seen on one page only is a stray page in the stack: nothing around it belongs to it.
pub fn fill_gaps(evidence: &[Option<Evidence>]) -> Vec<usize> {
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for found in evidence.iter().flatten() {
        *counts.entry(found.piece).or_default() += 1;
    }
    let mut stack = evidence
        .iter()
        .flatten()
        .find(|found| counts[&found.piece] >= 2)
        .map_or(0, |found| found.piece);
    evidence
        .iter()
        .map(|found| match found {
            Some(found) => {
                if counts[&found.piece] >= 2 {
                    stack = found.piece;
                }
                found.piece
            }
            None => stack,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PageAnalysis;
    use image::{Rgb, RgbImage};
    use std::sync::Arc;

    fn line(text: &str, top: f32, height: f32) -> OcrLine {
        OcrLine {
            text: text.to_owned(),
            confidence: 0.8,
            left: 40.0,
            top,
            height,
            width: 100.0,
        }
    }

    fn page(lines: Vec<OcrLine>, with_expected_title: bool) -> PageImage {
        let image = Arc::new(RgbImage::from_pixel(100, 1000, Rgb([255, 255, 255])));
        PageImage {
            source_file: "test.pdf".into(),
            source_page_index: 0,
            global_order: 0,
            id: 0,
            image: Arc::clone(&image),
            corrected_image: image,
            orientation: 0,
            analysis: PageAnalysis {
                text_lines: lines,
                staff_top: Some(300),
                staff_gap: Some(10.0),
                music_title: with_expected_title.then(|| "Ein Stück".to_owned()),
                ..Default::default()
            },
        }
    }

    fn titled(title: &str) -> PageImage {
        page(vec![line(title, 50.0, 40.0)], false)
    }

    #[test]
    fn a_heading_on_several_pages_is_a_second_piece() {
        let pages = [
            titled("ANDERE-WEISE"),
            titled("ANDERE WEISE"),
            titled("ANDERE-WEISE"),
            page(vec![line("Ein Stück", 50.0, 40.0)], true),
        ];
        assert_eq!(discover(&pages, "Ein Stück"), ["Ein Stück", "Andere-Weise"]);
    }

    #[test]
    fn misreads_of_the_title_are_no_second_piece() {
        let pages = [
            titled("Ein Stuck"),
            titled("Fin Stück"),
            titled("Ein Stick"),
        ];
        assert_eq!(discover(&pages, "Ein Stück"), ["Ein Stück"]);
    }

    #[test]
    fn a_composer_above_the_title_is_no_second_piece() {
        let pages = [
            page(vec![line("Hans Meierhof", 50.0, 40.0)], true),
            page(vec![line("Hans Meierhof", 50.0, 40.0)], true),
        ];
        assert_eq!(discover(&pages, "Ein Stück"), ["Ein Stück"]);
    }

    #[test]
    fn labels_credits_forms_and_small_lines_are_no_headings() {
        for text in ["Flügelhorn", "Musik: H. Meierhof", "Marsch"] {
            assert_eq!(
                discover(&[titled(text), titled(text)], "Ein Stück").len(),
                1
            );
        }
        let small = page(vec![line("Andere Weise", 50.0, 15.0)], false);
        assert_eq!(discover(&[small.clone(), small], "Ein Stück").len(), 1);
    }

    #[test]
    fn a_single_page_only_counts_with_a_clear_title() {
        assert_eq!(
            discover(&[titled("Am Grünen Fluss a,")], "Ein Stück"),
            ["Ein Stück", "Am Grünen Fluss"]
        );
        assert_eq!(discover(&[titled("Xqwzr")], "Ein Stück").len(), 1);
        assert_eq!(
            discover(&[titled("TapeWRik flloee*")], "Ein Stück").len(),
            1
        );
        assert_eq!(discover(&[titled("POST SNLS")], "Ein Stück").len(), 1);
    }

    #[test]
    fn directions_on_continuation_pages_are_no_second_piece() {
        // Lowercase, also when a clean title is not read anywhere else.
        let lowercase = titled("spielen");
        assert_eq!(
            discover(&[lowercase.clone(), lowercase], "Ein Stück").len(),
            1
        );
        // No room for a title block above the first staff.
        let mut direction = titled("Refrain");
        direction.analysis.staff_top = Some(60);
        direction.analysis.text_lines = vec![line("Refrain", 20.0, 30.0)];
        assert_eq!(
            discover(&[direction.clone(), direction], "Ein Stück").len(),
            1
        );
    }

    #[test]
    fn a_heading_much_smaller_than_the_title_is_no_second_piece() {
        let small = page(vec![line("Refrain", 50.0, 25.0)], false);
        let title = page(vec![line("Ein Stück", 50.0, 60.0)], true);
        assert_eq!(
            discover(&[title, small.clone(), small], "Ein Stück").len(),
            1
        );
    }

    #[test]
    fn a_heading_with_staff_lines_through_it_is_music() {
        let mut ruled = titled("Andere Weise");
        let mut image = RgbImage::from_pixel(200, 1000, Rgb([255, 255, 255]));
        for y in [55, 65, 75] {
            for x in 40..140 {
                image.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        ruled.corrected_image = Arc::new(image);
        assert_eq!(discover(&[ruled.clone(), ruled], "Ein Stück").len(), 1);
    }

    #[test]
    fn the_name_prefers_complete_readings_and_tames_capitals() {
        let readings = ["ANDERE ?EISE", "ANDERE ?EISE", "ANDERE WEISE"].map(str::to_owned);
        assert_eq!(piece_name(&readings), "Andere Weise");
        assert_eq!(title_case_capitals("Am Fluss"), "Am Fluss");
    }

    #[test]
    fn the_best_title_decides_also_in_merged_lines() {
        let pieces = ["Gruß vom Berg".to_owned(), "Mein Berg".to_owned()];
        let pages = [
            page(
                vec![line("Direktion Mein ?erg 22 Nr 62", 50.0, 30.0)],
                false,
            ),
            page(vec![line("Gruß vom Berg 7", 50.0, 30.0)], false),
        ];
        let found: Vec<usize> = assign(&pages, &pieces)
            .into_iter()
            .map(|found| found.unwrap().piece)
            .collect();
        assert_eq!(found, [1, 0]);
    }

    #[test]
    fn half_a_title_decides_nothing() {
        let pieces = ["Gruß vom Grünland".to_owned(), "Mein Grünland".to_owned()];
        let pages = [page(vec![line("iape w em grnlang", 50.0, 30.0)], false)];
        assert_eq!(assign(&pages, &pieces), [None]);
    }

    #[test]
    fn a_catalogue_number_names_the_piece_when_the_title_is_unread() {
        let pieces = ["Gruß vom Berg".to_owned(), "Mein Tal".to_owned()];
        let numbered = |title: &str, number: &str| {
            page(
                vec![line(title, 50.0, 30.0), line(number, 20.0, 12.0)],
                false,
            )
        };
        let pages = [
            numbered("Gruß vom Berg", "Nr. 624"),
            numbered("Gruß vom Berg", "Nr 624"),
            numbered("Mein Tal", "Nr. 623"),
            numbered("Mein Tal", "Nr. 623"),
            numbered("xx", "Nr. 623"),
            numbered("xx", "Nr. 624"),
            numbered("xx", "Nr. 999"),
        ];
        let found = assign(&pages, &pieces);
        assert_eq!(
            found[4],
            Some(Evidence {
                piece: 1,
                from_title: false
            })
        );
        assert_eq!(
            found[5],
            Some(Evidence {
                piece: 0,
                from_title: false
            })
        );
        assert_eq!(found[6], None);
    }

    #[test]
    fn pages_without_evidence_take_the_piece_before_them() {
        let found = |piece| {
            Some(Evidence {
                piece,
                from_title: true,
            })
        };
        assert_eq!(
            fill_gaps(&[None, found(1), None, found(1), found(0), found(0), None]),
            [1, 1, 1, 1, 0, 0, 0]
        );
        // A piece on one page only does not take the pages after it.
        assert_eq!(
            fill_gaps(&[found(1), found(1), found(2), None]),
            [1, 1, 2, 1]
        );
    }
}
