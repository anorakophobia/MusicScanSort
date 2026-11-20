use crate::instrument_map;
use crate::{GroupKey, InstrumentGroup, PageImage};
use log::{debug, info, warn};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

pub const UNKNOWN_NAME: &str = "UNKNOWN UNDETECTED";
/// Holds the scans of a page beyond the complete sets of its part (`sort_out_repeated_scans`).
pub const DUPLICATED_NAME: &str = "DUPLICATED";
const SCORE_VOICE_MATCH: f32 = 3.0;
const SCORE_NAME_MATCH: f32 = 2.0;
const SCORE_CONFLICT: f32 = -1.5;
const SCORE_MIRROR_CHAIN: f32 = 1.2;
/// Two scans of the same page share at least this many words and this share of all words.
const SAME_PAGE_MIN_WORDS: usize = 4;
const SAME_PAGE_SHARE: f32 = 0.3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Front,
    Continuation,
    Blank,
}

/// When the piece title is read on a large share of the pages, a page without title evidence
/// cannot be a first page, however much room there is above its first staff.
fn apply_title_rule(pages: &mut [PageImage]) {
    let analysed = pages.iter().filter(|page| !page.analysis.is_blank).count();
    let titled = pages
        .iter()
        .filter(|page| page.analysis.music_title.is_some())
        .count();
    if analysed == 0 || (titled as f32) < analysed as f32 * 0.30 {
        return;
    }
    for page in pages.iter_mut() {
        if page.analysis.is_part_start && !page.analysis.has_title_evidence {
            debug!(
                "Page {}: no title evidence although titles are readable → continuation page.",
                page.global_order
            );
            page.analysis.is_part_start = false;
        }
    }
}

fn label_of(page: &PageImage) -> Option<(String, Option<u32>)> {
    page.analysis
        .instrument_normalized
        .clone()
        .map(|name| (name, page.analysis.voice_number))
}

/// A stack whose first pages lost their header (e.g. scanned too short) shows n unlabelled
/// pages followed by n back pages that do carry their part label (U1..Un Cn..C1). Each first
/// page then takes the label of its mirrored back page.
fn name_fronts_from_backs(pages: &mut [PageImage]) {
    let labelled_front = |page: &PageImage| {
        page.analysis.is_part_start && page.analysis.has_title_evidence && !page.analysis.is_blank
    };
    let unlabelled =
        |page: &PageImage| !page.analysis.is_blank && page.analysis.instrument_normalized.is_none();
    let mut start = 0;
    while start < pages.len() {
        if !unlabelled(&pages[start])
            || (start > 0 && pages[start - 1].analysis.is_part_start)
            || (start > 0 && unlabelled(&pages[start - 1]))
        {
            start += 1;
            continue;
        }
        let middle = (start..pages.len())
            .find(|index| !unlabelled(&pages[*index]))
            .unwrap_or(pages.len());
        let count = middle - start;
        let end = middle + count;
        if count < 3 || end > pages.len() {
            start = middle;
            continue;
        }
        let backs = &pages[middle..end];
        let labels: Vec<Option<(String, Option<u32>)>> = backs.iter().map(label_of).collect();
        let distinct: HashSet<&(String, Option<u32>)> = labels.iter().flatten().collect();
        let labelled = labels.iter().flatten().count();
        // Back pages of an ordinary stack repeat the labels of its first pages, which come
        // before them. Full re-scans of the same parts later in the input do not count.
        let earlier_fronts: HashSet<(String, Option<u32>)> = pages[..start]
            .iter()
            .filter(|page| labelled_front(page))
            .filter_map(label_of)
            .collect();
        // A back page with its label above the music may look like a first page by its staff
        // position, but it lacks title evidence.
        let fits = backs.iter().all(|page| {
            !page.analysis.is_blank
                && (!page.analysis.is_part_start || !page.analysis.has_title_evidence)
        }) && labelled * 10 >= count * 6
            && distinct.len() == labelled
            && distinct
                .iter()
                .filter(|label| earlier_fronts.contains(**label))
                .count()
                * 2
                < distinct.len();
        if !fits {
            start = middle;
            continue;
        }
        info!(
            "Pages {}..={}: unlabelled first pages named from their mirrored back pages {}..={}.",
            pages[start].global_order,
            pages[middle - 1].global_order,
            pages[middle].global_order,
            pages[end - 1].global_order
        );
        for page in &mut pages[middle..end] {
            page.analysis.is_part_start = false;
        }
        for (offset, label) in labels.into_iter().enumerate() {
            let front = &mut pages[middle - 1 - offset].analysis;
            front.is_part_start = true;
            if let Some((name, voice)) = label {
                front.instrument_normalized = Some(name);
                front.voice_number = voice;
            }
        }
        start = end;
    }
}

/// Ranks duplicate scans of a part, best first: a legible part label on its first page, then a
/// clearly straighter scan (its most crooked page, in 0.5° steps; smaller differences are within
/// the noise of the estimate), then less over-darkened ink on its first page, then the later scan.
fn scan_rank(group: &InstrumentGroup) -> (bool, u32, u32, Reverse<usize>) {
    let first = group
        .pages
        .iter()
        .find(|page| page.analysis.is_part_start)
        .unwrap_or(&group.pages[0]);
    let worst_skew = group
        .pages
        .iter()
        .filter(|page| !page.analysis.is_blank)
        .map(|page| page.analysis.skew_degrees.abs())
        .fold(0.0, f32::max);
    (
        first.analysis.instrument_raw.is_none(),
        (worst_skew / 0.5).floor() as u32,
        (first.analysis.ink_darkness * 1000.0).round() as u32,
        Reverse(first.global_order),
    )
}

/// The best scan of a part (see `scan_rank`) becomes the main file, the others "Copy 1", ...
fn rank_duplicate_scans(groups: &mut [InstrumentGroup]) {
    let mut parts: HashMap<GroupKey, Vec<usize>> = HashMap::new();
    for (index, group) in groups.iter().enumerate() {
        if !is_unknown(&group.key) && !is_duplicated(&group.key) && !group.pages.is_empty() {
            let part = GroupKey {
                copy: 0,
                ..group.key.clone()
            };
            parts.entry(part).or_default().push(index);
        }
    }
    for mut indices in parts.into_values().filter(|indices| indices.len() > 1) {
        indices.sort_by_key(|index| scan_rank(&groups[*index]));
        for (copy, index) in indices.into_iter().enumerate() {
            let rank = scan_rank(&groups[index]);
            let group = &mut groups[index];
            group.key.copy = copy as u32;
            if copy > 0 {
                info!(
                    "Pages {:?}: duplicate scan of {:?} (worst skew {}×0.5°, ink darkness {:.3}) → Copy {copy}.",
                    group
                        .pages
                        .iter()
                        .map(|page| page.global_order)
                        .collect::<Vec<_>>(),
                    group.key.instrument_name,
                    rank.1,
                    rank.2 as f32 / 1000.0,
                );
            }
        }
    }
}

/// The English "Baritone" with its clef marker, as the label line reads: `Some(true)` for "TC",
/// `Some(false)` for a keyed "Bb Baritone" without marker.
fn baritone_label(page: &PageImage) -> Option<bool> {
    if page.analysis.instrument_normalized.as_deref() != Some("Bariton B") {
        return None;
    }
    let text = instrument_map::normalize_text(&page.analysis.marks.label.as_ref()?.text);
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if !tokens.contains(&"baritone") {
        return None;
    }
    if tokens.contains(&"tc") {
        Some(true)
    } else {
        (tokens.contains(&"b") && !tokens.contains(&"bc")).then_some(false)
    }
}

/// Some editions print the German Tenorhorn as "Baritone TC" next to a "Bb Baritone" part (the
/// Bariton, also as "Baritone BC"). Only when a piece has both does "Baritone TC" mean Tenorhorn.
fn name_treble_baritone_tenorhorn(pages: &mut [PageImage]) {
    let keyed: HashSet<Option<String>> = pages
        .iter()
        .filter(|page| baritone_label(page) == Some(false))
        .map(|page| page.analysis.piece.clone())
        .collect();
    for page in pages.iter_mut() {
        if baritone_label(page) == Some(true) && keyed.contains(&page.analysis.piece) {
            debug!(
                "Page {}: \"Baritone TC\" next to \"Bb Baritone\" → Tenorhorn.",
                page.global_order
            );
            page.analysis.instrument_normalized = Some("Tenorhorn".to_owned());
        }
    }
}

/// Fronts of the same part that appear several times are marked as duplicate scans, so that
/// each scan forms its own group; `rank_duplicate_scans` decides which one is the main file.
fn prepare_pages(pages: &[PageImage]) -> Vec<PageImage> {
    let mut pages = pages.to_vec();
    apply_title_rule(&mut pages);
    name_fronts_from_backs(&mut pages);
    name_treble_baritone_tenorhorn(&mut pages);
    let mut pages = infer_missing_voices(&pages);
    let mut scans: HashMap<(Option<String>, String, Option<u32>), Vec<usize>> = HashMap::new();
    for (index, page) in pages.iter().enumerate() {
        let analysis = &page.analysis;
        if analysis.is_blank || !analysis.is_part_start {
            continue;
        }
        let Some(name) = analysis.instrument_normalized.clone() else {
            continue;
        };
        // The pages of a conductor's score repeat its label, unless each one is a part of its own.
        let own_part = analysis.music_title.is_some() || analysis.content_crop.is_some();
        if matches!(name.as_str(), "Direktion" | "Partitur") && !own_part {
            continue;
        }
        scans
            .entry((analysis.piece.clone(), name, analysis.voice_number))
            .or_default()
            .push(index);
    }
    for indices in scans.into_values() {
        for (copy, index) in indices.into_iter().enumerate() {
            pages[index].analysis.duplicate_copy = copy as u32;
        }
    }
    pages
}

/// When the parts of an instrument were scanned k times, each voice should appear k times; the
/// unnumbered first pages fill the voices that are short of scans, lowest voice first.
fn fill_voice_gaps(pages: &mut [PageImage]) {
    let mut families: HashMap<(Option<String>, String), (HashMap<u32, usize>, Vec<usize>)> =
        HashMap::new();
    for (index, page) in pages.iter().enumerate() {
        let analysis = &page.analysis;
        if analysis.is_blank || !analysis.is_part_start {
            continue;
        }
        let Some(name) = &analysis.instrument_normalized else {
            continue;
        };
        let entry = families
            .entry((analysis.piece.clone(), name.clone()))
            .or_default();
        match analysis.voice_number {
            Some(voice) => *entry.0.entry(voice).or_default() += 1,
            None => entry.1.push(index),
        }
    }
    for ((_, name), (scans, unnumbered)) in families {
        let (Some(&highest), Some(&copies)) = (scans.keys().max(), scans.values().max()) else {
            continue;
        };
        let missing: Vec<u32> = (1..=highest)
            .flat_map(|voice| {
                let short = copies - scans.get(&voice).copied().unwrap_or(0);
                std::iter::repeat_n(voice, short)
            })
            .collect();
        if missing.is_empty() || unnumbered.is_empty() {
            continue;
        }
        let take = missing.len().min(unnumbered.len());
        for (voice, index) in missing
            .iter()
            .take(take)
            .zip(unnumbered.iter().skip(unnumbered.len() - take))
        {
            debug!(
                "Page {}: missing voice {voice} of {name:?} assigned to the unnumbered first page.",
                pages[*index].global_order
            );
            pages[*index].analysis.voice_number = Some(*voice);
        }
    }
}

/// An unnumbered "Klarinette" first page among numbered B-clarinet parts is the Es-clarinet.
fn rename_unnumbered_clarinet(pages: &mut [PageImage]) {
    let numbered: HashSet<Option<String>> = pages
        .iter()
        .filter(|page| {
            page.analysis.is_part_start
                && page.analysis.voice_number.is_some()
                && page.analysis.instrument_normalized.as_deref() == Some("Klarinette")
        })
        .map(|page| page.analysis.piece.clone())
        .collect();
    for page in pages.iter_mut() {
        if numbered.contains(&page.analysis.piece)
            && page.analysis.is_part_start
            && !page.analysis.is_blank
            && page.analysis.voice_number.is_none()
            && page.analysis.instrument_normalized.as_deref() == Some("Klarinette")
        {
            debug!(
                "Page {}: unnumbered clarinet among numbered parts → Klarinette Es.",
                page.global_order
            );
            page.analysis.instrument_normalized = Some("Klarinette Es".to_owned());
        }
    }
}

/// A first page whose voice number was not read is first matched to a missing voice of its
/// instrument (voices 2 and 3 were read, so the unnumbered page must be voice 1), then inherits
/// the number of the directly preceding first page of the same instrument (duplicate scans).
fn infer_missing_voices(pages: &[PageImage]) -> Vec<PageImage> {
    let mut pages = pages.to_vec();
    fill_voice_gaps(&mut pages);
    let family = |page: &PageImage, name: &str| (page.analysis.piece.clone(), name.to_owned());
    let numbered: HashSet<(Option<String>, String)> = pages
        .iter()
        .filter(|page| page.analysis.is_part_start && page.analysis.voice_number.is_some())
        .filter_map(|page| {
            let name = page.analysis.instrument_normalized.as_deref()?;
            Some(family(page, name))
        })
        .collect();
    let mut previous: Option<((Option<String>, String), u32)> = None;
    for page in &mut pages {
        if page.analysis.is_blank || !page.analysis.is_part_start {
            continue;
        }
        let name = page.analysis.instrument_normalized.clone();
        match (&name, page.analysis.voice_number) {
            (Some(name), None) if numbered.contains(&family(page, name)) => {
                if let Some((previous_family, voice)) = &previous
                    && *previous_family == family(page, name)
                {
                    debug!(
                        "Page {}: voice number inferred as {voice} from the preceding first page.",
                        page.global_order
                    );
                    page.analysis.voice_number = Some(*voice);
                }
            }
            (Some(name), Some(voice)) => previous = Some((family(page, name), voice)),
            _ => previous = None,
        }
    }
    rename_unnumbered_clarinet(&mut pages);
    pages
}

/// Groups pages per instrument.
///
/// Scanned stacks are usually fed as all first pages in part order, followed by all
/// back pages in reverse order (the flipped stack). Continuation pages are therefore only
/// attached to a part when the mirrored position of a first page or a page label confirms
/// it; everything else is collected in the UNKNOWN UNDETECTED group for manual review.
pub fn build_groups(pages: &[PageImage]) -> Vec<InstrumentGroup> {
    let pages = &prepare_pages(pages);
    let owners = assign_owners(pages);
    let mut groups: Vec<InstrumentGroup> = Vec::new();
    let mut indices: HashMap<GroupKey, usize> = HashMap::new();

    for (page, owner) in pages.iter().zip(owners) {
        let Some(key) = owner else {
            continue;
        };
        let index = *indices.entry(key.clone()).or_insert_with(|| {
            info!("New instrument group: {:?}", key.label());
            groups.push(InstrumentGroup {
                key: key.clone(),
                pages: Vec::new(),
                output_filename: String::new(),
            });
            groups.len() - 1
        });
        groups[index].pages.push(page.clone());
    }
    for group in &mut groups {
        order_group_pages(group);
    }
    sort_out_repeated_scans(&mut groups);
    rank_duplicate_scans(&mut groups);
    groups
}

/// Words of at least four letters on a page, to tell two scans of the same page.
fn page_words(page: &PageImage) -> HashSet<String> {
    page.analysis
        .text_lines
        .iter()
        .flat_map(|line| {
            line.text
                .split(|character: char| !character.is_ascii_alphabetic())
                .filter(|word| word.len() >= 4)
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Two scans of the same page share most of their OCR words; two pages of the same part
/// share only a few headings and directions.
fn same_page(left: &HashSet<String>, right: &HashSet<String>) -> bool {
    let shared = left.intersection(right).count();
    let all = left.union(right).count();
    shared >= SAME_PAGE_MIN_WORDS && shared as f32 >= SAME_PAGE_SHARE * all as f32
}

/// Quality of one scan of a page, best first, as in `scan_rank`.
fn page_rank(page: &PageImage) -> (bool, u32, u32, Reverse<usize>) {
    (
        page.analysis.is_part_start && page.analysis.instrument_raw.is_none(),
        (page.analysis.skew_degrees.abs() / 0.5).floor() as u32,
        (page.analysis.ink_darkness * 1000.0).round() as u32,
        Reverse(page.global_order),
    )
}

/// Single pages of a part scanned again make the scans of that part unequal: a copy with a
/// first page only, or a page twice in one file. Each page then keeps as many scans as there
/// are complete sets, the best ones; the others go to `DUPLICATED`. Parts whose scans
/// each hold every page once are left as they are.
fn sort_out_repeated_scans(groups: &mut Vec<InstrumentGroup>) {
    let mut parts: HashMap<GroupKey, Vec<usize>> = HashMap::new();
    for (index, group) in groups.iter().enumerate() {
        if !is_unknown(&group.key) && !group.pages.is_empty() {
            let part = GroupKey {
                copy: 0,
                ..group.key.clone()
            };
            parts.entry(part).or_default().push(index);
        }
    }
    let mut left_over: Vec<PageImage> = Vec::new();
    let mut parts = parts.into_iter().collect::<Vec<_>>();
    parts.sort_by_key(|(_, indices)| indices[0]);
    for (part, mut indices) in parts {
        indices.sort_by_key(|index| groups[*index].key.copy);
        // A unit is a page with the blank pages that follow it in its file.
        let mut units: Vec<Vec<PageImage>> = Vec::new();
        let mut unit_scan: Vec<usize> = Vec::new();
        for (scan, index) in indices.iter().enumerate() {
            for page in &groups[*index].pages {
                match units.last_mut() {
                    Some(unit) if page.analysis.is_blank && unit_scan.last() == Some(&scan) => {
                        unit.push(page.clone());
                    }
                    _ => {
                        units.push(vec![page.clone()]);
                        unit_scan.push(scan);
                    }
                }
            }
        }
        // The same page: all first pages, and continuation pages with the same words.
        let words: Vec<HashSet<String>> = units.iter().map(|unit| page_words(&unit[0])).collect();
        let mut pages_of: Vec<Vec<usize>> = Vec::new();
        for unit in 0..units.len() {
            let head = &units[unit][0];
            let known = pages_of.iter().position(|scans| {
                let other = &units[scans[0]][0];
                if head.analysis.is_part_start || other.analysis.is_part_start {
                    // A further labelled page in the same scan is another page (Direktion).
                    head.analysis.is_part_start
                        && other.analysis.is_part_start
                        && scans
                            .iter()
                            .all(|known| unit_scan[*known] != unit_scan[unit])
                } else {
                    !head.analysis.is_blank
                        && !other.analysis.is_blank
                        && same_page(&words[unit], &words[scans[0]])
                }
            });
            match known {
                Some(page) => pages_of[page].push(unit),
                None => pages_of.push(vec![unit]),
            }
        }
        // Scans with the same number of pages and no page twice stay as they are, also when the
        // words of two scans of a page do not match.
        let music_pages = |index: usize| {
            groups[index]
                .pages
                .iter()
                .filter(|page| !page.analysis.is_blank)
                .count()
        };
        let equal_scans = indices
            .iter()
            .all(|index| music_pages(*index) == music_pages(indices[0]));
        let page_twice_in_a_scan = pages_of.iter().any(|scans| {
            let mut seen = HashSet::new();
            scans.iter().any(|unit| !seen.insert(unit_scan[*unit]))
        });
        if equal_scans && !page_twice_in_a_scan {
            continue;
        }
        let blank_head = |scans: &Vec<usize>| units[scans[0]][0].analysis.is_blank;
        let sets = pages_of
            .iter()
            .filter(|scans| !blank_head(scans))
            .map(Vec::len)
            .min()
            .unwrap_or(1)
            .max(1);
        let mut set_pages: Vec<Vec<PageImage>> = vec![Vec::new(); sets];
        for mut scans in pages_of {
            scans.sort_by_key(|unit| page_rank(&units[*unit][0]));
            for (rank, unit) in scans.into_iter().enumerate() {
                if rank < sets {
                    set_pages[rank].extend(units[unit].iter().cloned());
                } else {
                    info!(
                        "Page {}: another scan of a page of {:?} → {DUPLICATED_NAME}.",
                        units[unit][0].global_order,
                        part.label()
                    );
                    left_over.extend(units[unit].iter().cloned());
                }
            }
        }
        for (slot, index) in indices.iter().enumerate() {
            let pages = set_pages
                .get_mut(slot)
                .map(std::mem::take)
                .unwrap_or_default();
            groups[*index].pages = pages;
            groups[*index].key.copy = slot as u32;
            order_group_pages(&mut groups[*index]);
        }
    }
    if !left_over.is_empty() {
        let mut pages = left_over;
        pages.sort_by_key(|page| page.global_order);
        groups.push(InstrumentGroup {
            key: GroupKey {
                instrument_name: DUPLICATED_NAME.to_owned(),
                ..unknown_key()
            },
            pages,
            output_filename: String::new(),
        });
    }
    groups.retain(|group| !group.pages.is_empty());
}

fn kind(page: &PageImage) -> Kind {
    if page.analysis.is_blank {
        Kind::Blank
    } else if page.analysis.is_part_start {
        Kind::Front
    } else {
        Kind::Continuation
    }
}

fn unknown_key() -> GroupKey {
    GroupKey {
        piece: None,
        instrument_name: UNKNOWN_NAME.to_owned(),
        voice_number: None,
        copy: 0,
    }
}

fn is_unknown(key: &GroupKey) -> bool {
    key.instrument_name.contains("UNDETECTED")
}

fn is_duplicated(key: &GroupKey) -> bool {
    key.instrument_name == DUPLICATED_NAME
}

fn front_key(page: &PageImage) -> GroupKey {
    let analysis = &page.analysis;
    if let Some(name) = &analysis.instrument_normalized {
        GroupKey {
            piece: analysis.piece.clone(),
            instrument_name: name.clone(),
            voice_number: analysis.voice_number,
            copy: analysis.duplicate_copy,
        }
    } else if let Some(raw) = &analysis.instrument_raw {
        GroupKey {
            piece: analysis.piece.clone(),
            instrument_name: format!("{raw} UNDETECTED"),
            voice_number: analysis.voice_number,
            copy: 0,
        }
    } else {
        unknown_key()
    }
}

fn label_key(page: &PageImage) -> Option<GroupKey> {
    if page.analysis.is_blank {
        return None;
    }
    page.analysis
        .instrument_normalized
        .as_ref()
        .map(|name| GroupKey {
            piece: page.analysis.piece.clone(),
            instrument_name: name.clone(),
            voice_number: page.analysis.voice_number,
            copy: 0,
        })
}

fn label_score(label: Option<&GroupKey>, front: &GroupKey) -> f32 {
    let Some(label) = label else {
        return 0.0;
    };
    if is_unknown(front) {
        return 0.0;
    }
    if label.instrument_name != front.instrument_name {
        return SCORE_CONFLICT;
    }
    match (label.voice_number, front.voice_number) {
        (Some(left), Some(right)) if left == right => SCORE_VOICE_MATCH,
        (Some(_), Some(_)) => SCORE_CONFLICT,
        _ => SCORE_NAME_MATCH,
    }
}

fn assign_owners(pages: &[PageImage]) -> Vec<Option<GroupKey>> {
    let kinds: Vec<Kind> = pages.iter().map(kind).collect();
    let known: HashSet<GroupKey> = pages
        .iter()
        .zip(&kinds)
        .filter(|(page, kind)| {
            **kind == Kind::Front && page.analysis.instrument_normalized.is_some()
        })
        .map(|(page, _)| GroupKey {
            copy: 0,
            ..front_key(page)
        })
        .collect();
    let mut owners: Vec<Option<GroupKey>> = vec![None; pages.len()];

    // A conductor score or "Direktion" part is usually the last part and has unlabeled pages.
    let score_start = pages.iter().zip(&kinds).position(|(page, kind)| {
        *kind == Kind::Front
            && matches!(
                page.analysis.instrument_normalized.as_deref(),
                Some("Direktion" | "Partitur")
            )
    });
    let limit = score_start.unwrap_or(pages.len());
    if let Some(start) = score_start {
        let mut current = front_key(&pages[start]);
        for index in start..pages.len() {
            if kinds[index] == Kind::Blank {
                continue;
            }
            if kinds[index] == Kind::Front && pages[index].analysis.instrument_normalized.is_some()
            {
                current = front_key(&pages[index]);
            }
            owners[index] = Some(current.clone());
        }
    }

    // Split into blocks of first pages and the continuation pages that follow them.
    struct Block {
        fronts: Vec<usize>,
        trailing: Vec<usize>,
        leading: Vec<usize>,
    }
    let mut blocks: Vec<Block> = Vec::new();
    let mut index = 0;
    while index < limit {
        let mut fronts = Vec::new();
        while index < limit && kinds[index] == Kind::Front {
            fronts.push(index);
            index += 1;
        }
        let mut trailing = Vec::new();
        while index < limit && kinds[index] != Kind::Front {
            trailing.push(index);
            index += 1;
        }
        blocks.push(Block {
            fronts,
            trailing,
            leading: Vec::new(),
        });
    }
    // The flipped stack may come first: continuation pages before their first pages.
    for position in 0..blocks.len() {
        let next_fronts = blocks.get(position + 1).map(|block| block.fronts.len());
        let own = blocks[position].fronts.len();
        let trailing = blocks[position].trailing.len();
        let moves_forward = match next_fronts {
            Some(next) => trailing > 0 && (own == 0 || (trailing != own && trailing == next)),
            None => false,
        };
        if moves_forward {
            let moved = std::mem::take(&mut blocks[position].trailing);
            blocks[position + 1].leading = moved;
        }
    }
    for block in &blocks {
        let fronts = &block.fronts;
        for &front in fronts {
            owners[front] = Some(front_key(&pages[front]));
        }
        let (leading, trailing) = (&block.leading, &block.trailing);
        if fronts.is_empty() {
            continue;
        }
        if leading.is_empty() {
            match fronts.len() {
                1 => attach_sequential(pages, &kinds, fronts[0], trailing, &known, &mut owners),
                _ => attach_mirrored(pages, &kinds, fronts, trailing, &mut owners),
            }
        } else if trailing.is_empty() {
            let count = leading.len().min(fronts.len());
            attach_mirrored(pages, &kinds, &fronts[..count], leading, &mut owners);
        } else if leading.len() + trailing.len() == fronts.len() {
            let (first, second) = fronts.split_at(leading.len());
            attach_mirrored(pages, &kinds, first, leading, &mut owners);
            attach_mirrored(pages, &kinds, second, trailing, &mut owners);
        } else {
            attach_mirrored(pages, &kinds, fronts, trailing, &mut owners);
        }
    }

    // The labels of the first pages, for labels that lost their first letters.
    let front_labels: Vec<(GroupKey, String)> = (0..pages.len())
        .filter(|index| kinds[*index] == Kind::Front)
        .filter_map(|index| {
            let key = owners[index].clone()?;
            let raw = pages[index].analysis.instrument_raw.as_deref()?;
            (!is_unknown(&key)).then(|| (key, instrument_map::normalize_text(raw)))
        })
        .collect();
    for (index, page) in pages.iter().enumerate() {
        if kinds[index] != Kind::Continuation || owners[index].is_some() {
            continue;
        }
        let resolved = label_key(page)
            .and_then(|label| resolve_label(&label, &known))
            .or_else(|| resolve_by_label_ending(page, &front_labels));
        match resolved {
            Some(key) => {
                debug!(
                    "Page {}: attached by its own label to {:?} (voice={:?})",
                    page.global_order, key.instrument_name, key.voice_number
                );
                owners[index] = Some(key);
            }
            None => {
                warn!(
                    "Page {}: continuation page could not be confirmed for any instrument → UNKNOWN UNDETECTED.",
                    page.global_order
                );
                owners[index] = Some(unknown_key());
            }
        }
    }
    owners
}

/// A continuation label that names no part may have lost its first letters ("- Bass" for
/// "E - Bass"). It belongs to the only part whose first-page label ends with the same words and
/// has more in front of them.
fn resolve_by_label_ending(page: &PageImage, front_labels: &[(GroupKey, String)]) -> Option<GroupKey> {
    let raw = page.analysis.instrument_raw.as_deref()?;
    let label = instrument_map::normalize_text(raw);
    if label.is_empty() {
        return None;
    }
    let ending = format!(" {label}");
    let parts: HashSet<&GroupKey> = front_labels
        .iter()
        .filter(|(_, front)| front.ends_with(&ending))
        .map(|(key, _)| key)
        .collect();
    let mut parts = parts.into_iter();
    match (parts.next(), parts.next()) {
        (Some(key), None) => {
            info!(
                "Page {}: label {raw:?} is the end of the label of {:?}.",
                page.global_order,
                key.label()
            );
            Some(key.clone())
        }
        _ => None,
    }
}

/// Maps a page label to an existing first-page group; returns None when ambiguous. A piece other
/// than the page's own is only considered when one piece alone has the part.
fn resolve_label(label: &GroupKey, known: &HashSet<GroupKey>) -> Option<GroupKey> {
    let in_piece = |piece: &Option<String>| {
        let label = GroupKey {
            piece: piece.clone(),
            ..label.clone()
        };
        if known.contains(&label) {
            return Some(label);
        }
        let same_instrument: Vec<&GroupKey> = known
            .iter()
            .filter(|key| key.piece == *piece && key.instrument_name == label.instrument_name)
            .collect();
        if label.voice_number.is_none() {
            return (same_instrument.len() == 1).then(|| same_instrument[0].clone());
        }
        let unnumbered = GroupKey {
            voice_number: None,
            ..label
        };
        known.contains(&unnumbered).then_some(unnumbered)
    };
    if let Some(key) = in_piece(&label.piece) {
        return Some(key);
    }
    let others: HashSet<&Option<String>> = known
        .iter()
        .map(|key| &key.piece)
        .filter(|piece| **piece != label.piece)
        .collect();
    let mut found = others.into_iter().filter_map(in_piece);
    match (found.next(), found.next()) {
        (Some(key), None) => Some(key),
        _ => None,
    }
}

/// A first page followed by its own continuation pages (F C C ...).
fn attach_sequential(
    pages: &[PageImage],
    kinds: &[Kind],
    front: usize,
    continuations: &[usize],
    known: &HashSet<GroupKey>,
    owners: &mut [Option<GroupKey>],
) {
    let mut current = front_key(&pages[front]);
    for &index in continuations {
        if kinds[index] == Kind::Blank {
            continue;
        }
        if let Some(label) = label_key(&pages[index])
            && label_score(Some(&label), &current) < 0.0
        {
            current = resolve_label(&label, known).unwrap_or_else(unknown_key);
        }
        owners[index] = Some(current.clone());
    }
}

/// Several first pages followed by continuation pages in reversed order (F1 F2 F3 C3 C2 C1).
fn attach_mirrored(
    pages: &[PageImage],
    kinds: &[Kind],
    fronts: &[usize],
    continuations: &[usize],
    owners: &mut [Option<GroupKey>],
) {
    if continuations.is_empty() {
        return;
    }
    let front_keys: Vec<GroupKey> = fronts
        .iter()
        .map(|index| front_key(&pages[*index]))
        .collect();
    let labels: Vec<Option<GroupKey>> = continuations
        .iter()
        .map(|index| label_key(&pages[*index]))
        .collect();
    let full_mirror: Vec<Option<usize>> = (0..continuations.len())
        .map(|slot| front_keys.len().checked_sub(slot + 1))
        .collect();
    let alignment = if continuations.len() == front_keys.len() && {
        let (agreements, conflicts) = count_evidence(&front_keys, &labels, &full_mirror);
        agreements >= conflicts
    } {
        full_mirror
    } else {
        align_mirrored(&front_keys, &labels)
    };

    let mut position = 0;
    while position < alignment.len() {
        let Some(first_front) = alignment[position] else {
            position += 1;
            continue;
        };
        let start = position;
        let mut last_front = first_front;
        position += 1;
        while position < alignment.len()
            && last_front
                .checked_sub(1)
                .is_some_and(|next| alignment[position] == Some(next))
        {
            last_front -= 1;
            position += 1;
        }
        let end = position;

        let segment_front_keys = &front_keys;
        let (agreements, conflicts) = count_evidence(
            segment_front_keys,
            &labels[start..end],
            &alignment[start..end],
        );
        let covers_whole_block =
            end - start == front_keys.len() && continuations.len() == front_keys.len();
        let confirmed = (agreements > 0 && agreements > conflicts) || covers_whole_block;
        debug!(
            "Mirrored pages {}..={} → first pages {}..={}: {} agreement(s), {} conflict(s), confirmed={}",
            pages[continuations[start]].global_order,
            pages[continuations[end - 1]].global_order,
            pages[fronts[first_front]].global_order,
            pages[fronts[last_front]].global_order,
            agreements,
            conflicts,
            confirmed
        );
        if !confirmed {
            continue;
        }
        for slot in start..end {
            let page_index = continuations[slot];
            if kinds[page_index] == Kind::Blank {
                continue;
            }
            let front = alignment[slot].unwrap();
            if label_score(labels[slot].as_ref(), &front_keys[front]) < 0.0 {
                warn!(
                    "Page {}: label {:?} conflicts with mirrored first page {} ({:?}); keeping the mirrored assignment.",
                    pages[page_index].global_order,
                    labels[slot].as_ref().map(|label| &label.instrument_name),
                    pages[fronts[front]].global_order,
                    front_keys[front].instrument_name
                );
            }
            owners[page_index] = Some(front_keys[front].clone());
        }
    }
}

fn count_evidence(
    front_keys: &[GroupKey],
    labels: &[Option<GroupKey>],
    alignment: &[Option<usize>],
) -> (usize, usize) {
    let mut agreements = 0;
    let mut conflicts = 0;
    for (label, front) in labels.iter().zip(alignment) {
        let Some(front) = front else {
            continue;
        };
        let score = label_score(label.as_ref(), &front_keys[*front]);
        if score > 0.0 {
            agreements += 1;
        } else if score < 0.0 {
            conflicts += 1;
        }
    }
    (agreements, conflicts)
}

/// Assigns continuation slots to first pages so that first-page indices strictly decrease.
/// Consecutive assignments on the mirror diagonal earn a bonus, label agreement adds evidence.
fn align_mirrored(front_keys: &[GroupKey], labels: &[Option<GroupKey>]) -> Vec<Option<usize>> {
    let fronts = front_keys.len();
    let slots = labels.len();
    // value[slot][bound][chained]: best future score; bound = last used first page index
    // (fronts = none yet), chained = previous slot was assigned to `bound`.
    let stride = fronts + 1;
    let mut value = vec![0.0f32; (slots + 1) * stride * 2];
    let at = |slot: usize, bound: usize, chained: usize| (slot * stride + bound) * 2 + chained;
    let scores: Vec<Vec<f32>> = labels
        .iter()
        .map(|label| {
            front_keys
                .iter()
                .map(|front| label_score(label.as_ref(), front))
                .collect()
        })
        .collect();

    for slot in (0..slots).rev() {
        for bound in 0..=fronts {
            for chained in 0..2 {
                let mut best = value[at(slot + 1, bound, 0)];
                for front in 0..bound {
                    let bonus = if chained == 1 && front + 1 == bound {
                        SCORE_MIRROR_CHAIN
                    } else {
                        0.0
                    };
                    let candidate = scores[slot][front] + bonus + value[at(slot + 1, front, 1)];
                    if candidate > best {
                        best = candidate;
                    }
                }
                value[at(slot, bound, chained)] = best;
            }
        }
    }

    let mut alignment = vec![None; slots];
    let (mut bound, mut chained) = (fronts, 0usize);
    for slot in 0..slots {
        let target = value[at(slot, bound, chained)];
        let mut chosen = None;
        for front in 0..bound {
            let bonus = if chained == 1 && front + 1 == bound {
                SCORE_MIRROR_CHAIN
            } else {
                0.0
            };
            let candidate = scores[slot][front] + bonus + value[at(slot + 1, front, 1)];
            if candidate > value[at(slot + 1, bound, 0)] && (candidate - target).abs() < 1e-4 {
                chosen = Some(front);
                break;
            }
        }
        match chosen {
            Some(front) => {
                alignment[slot] = Some(front);
                bound = front;
                chained = 1;
            }
            None => chained = 0,
        }
    }
    alignment
}

/// Keeps scan order unless every page carries a distinct page number.
fn order_group_pages(group: &mut InstrumentGroup) {
    let numbers: Vec<Option<u32>> = group
        .pages
        .iter()
        .map(|page| {
            page.analysis
                .page_number
                .or(page.analysis.is_part_start.then_some(1))
        })
        .collect();
    let distinct: HashSet<u32> = numbers.iter().flatten().copied().collect();
    if numbers.iter().all(Option::is_some) && distinct.len() == numbers.len() {
        group
            .pages
            .sort_by_key(|page| page.analysis.page_number.unwrap_or(1));
        return;
    }
    order_by_bars(group);
}

/// Without page numbers, continuation pages are ordered by their highest bar number, when each
/// one has a distinct one. A stack of backs is often scanned the wrong way round. First
/// pages stay in front, and a blank page stays behind the page it followed.
fn order_by_bars(group: &mut InstrumentGroup) {
    let continuations: Vec<Option<u32>> = group
        .pages
        .iter()
        .filter(|page| !page.analysis.is_blank && !page.analysis.is_part_start)
        .map(|page| page.analysis.last_bar)
        .collect();
    let distinct: HashSet<u32> = continuations.iter().flatten().copied().collect();
    if continuations.len() < 2
        || !continuations.iter().all(Option::is_some)
        || distinct.len() != continuations.len()
    {
        return;
    }
    let mut key = 0;
    let keys: Vec<u32> = group
        .pages
        .iter()
        .map(|page| {
            if page.analysis.is_part_start {
                key = 0;
            } else if !page.analysis.is_blank {
                key = page.analysis.last_bar.unwrap_or(0);
            }
            key
        })
        .collect();
    let before: Vec<usize> = group.pages.iter().map(|page| page.global_order).collect();
    let mut keyed: Vec<(u32, PageImage)> = keys.into_iter().zip(group.pages.drain(..)).collect();
    keyed.sort_by_key(|(key, _)| *key);
    group.pages = keyed.into_iter().map(|(_, page)| page).collect();
    let after: Vec<usize> = group.pages.iter().map(|page| page.global_order).collect();
    if before != after {
        info!(
            "Pages {before:?} of {:?} ordered by their bar numbers: {after:?}.",
            group.key.label()
        );
    }
}

pub fn validate_groups(groups: &[InstrumentGroup]) {
    for group in groups {
        let mut previous = None;
        for page in &group.pages {
            if let Some(number) = page.analysis.page_number {
                if let Some((previous_order, previous_number)) = previous {
                    if number <= previous_number {
                        warn!(
                            "Group {:?}: page numbers not monotonic at global_order={} (page_number={} ≤ previous {} at global_order={}) — scan order preserved.",
                            group.key.instrument_name,
                            page.global_order,
                            number,
                            previous_number,
                            previous_order
                        );
                    }
                }
                previous = Some((page.global_order, number));
            }
            if page.analysis.is_final_page
                && group
                    .pages
                    .last()
                    .is_some_and(|last| last.global_order != page.global_order)
            {
                warn!(
                    "Possible grouping error: page {} has a final barline but is not the last page in group {:?}.",
                    page.global_order, group.key.instrument_name
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PageAnalysis;
    use image::{Rgb, RgbImage};
    use std::sync::Arc;

    fn page(
        order: usize,
        instrument: Option<&str>,
        voice: Option<u32>,
        part_start: bool,
    ) -> PageImage {
        let image = RgbImage::from_pixel(4, 4, Rgb([255, 255, 255]));
        PageImage {
            source_file: "test.pdf".into(),
            source_page_index: order,
            global_order: order,
            id: order,
            image: Arc::new(image.clone()),
            corrected_image: Arc::new(image),
            orientation: 0,
            analysis: PageAnalysis {
                instrument_normalized: instrument.map(str::to_owned),
                voice_number: voice,
                is_part_start: part_start,
                has_title_evidence: part_start,
                ..Default::default()
            },
        }
    }

    fn front(order: usize, instrument: &str, voice: Option<u32>) -> PageImage {
        page(order, Some(instrument), voice, true)
    }

    fn back(order: usize) -> PageImage {
        page(order, None, None, false)
    }

    fn orders(group: &InstrumentGroup) -> Vec<usize> {
        group.pages.iter().map(|page| page.global_order).collect()
    }

    fn find<'a>(
        groups: &'a [InstrumentGroup],
        name: &str,
        voice: Option<u32>,
    ) -> &'a InstrumentGroup {
        groups
            .iter()
            .find(|group| {
                group.key.instrument_name == name
                    && group.key.voice_number == voice
                    && group.key.copy == 0
            })
            .unwrap_or_else(|| panic!("missing group {name} {voice:?}"))
    }

    #[test]
    fn sequential_pages_follow_their_first_page() {
        let groups = build_groups(&[
            front(0, "Flöte", None),
            back(1),
            front(2, "Oboe", None),
            back(3),
            back(4),
        ]);
        assert_eq!(orders(find(&groups, "Flöte", None)), vec![0, 1]);
        assert_eq!(orders(find(&groups, "Oboe", None)), vec![2, 3, 4]);
    }

    #[test]
    fn unlabeled_mirrored_stack_is_paired_by_position() {
        let groups = build_groups(&[
            front(0, "Flöte", Some(1)),
            front(1, "Flöte", Some(2)),
            front(2, "Oboe", None),
            back(3),
            back(4),
            back(5),
        ]);
        assert_eq!(orders(find(&groups, "Flöte", Some(1))), vec![0, 5]);
        assert_eq!(orders(find(&groups, "Flöte", Some(2))), vec![1, 4]);
        assert_eq!(orders(find(&groups, "Oboe", None)), vec![2, 3]);
    }

    #[test]
    fn mirrored_stack_tolerates_one_wrong_label() {
        let mut wrong = back(4);
        wrong.analysis.instrument_normalized = Some("Horn F".to_owned());
        wrong.analysis.voice_number = Some(3);
        let mut right = back(5);
        right.analysis.instrument_normalized = Some("Flöte".to_owned());
        right.analysis.voice_number = Some(1);
        let groups = build_groups(&[
            front(0, "Flöte", Some(1)),
            front(1, "Flöte", Some(2)),
            front(2, "Oboe", None),
            back(3),
            wrong,
            right,
        ]);
        assert_eq!(orders(find(&groups, "Flöte", Some(1))), vec![0, 5]);
        assert_eq!(orders(find(&groups, "Flöte", Some(2))), vec![1, 4]);
    }

    #[test]
    fn unconfirmed_continuations_are_not_attached() {
        let groups = build_groups(&[
            front(0, "Flöte", None),
            front(1, "Oboe", None),
            back(2),
            back(3),
            back(4),
        ]);
        assert_eq!(orders(find(&groups, "Flöte", None)), vec![0]);
        assert_eq!(orders(find(&groups, "Oboe", None)), vec![1]);
        assert_eq!(orders(find(&groups, UNKNOWN_NAME, None)), vec![2, 3, 4]);
    }

    #[test]
    fn labels_pin_the_mirror_when_counts_differ() {
        let groups = build_groups(&[
            front(0, "Flöte", Some(1)),
            front(1, "Flöte", Some(2)),
            front(2, "Oboe", None),
            back(3),
            page(4, Some("Flöte"), Some(1), false),
        ]);
        assert_eq!(orders(find(&groups, "Flöte", Some(1))), vec![0, 4]);
        assert_eq!(orders(find(&groups, "Flöte", Some(2))), vec![1, 3]);
        assert_eq!(orders(find(&groups, "Oboe", None)), vec![2]);
    }

    #[test]
    fn mirror_only_covers_leading_continuations_when_more_pages_follow() {
        let groups = build_groups(&[
            front(0, "Flöte", None),
            front(1, "Oboe", None),
            back(2),
            page(3, Some("Oboe"), None, false),
            back(4),
            back(5),
        ]);
        assert_eq!(orders(find(&groups, "Oboe", None)), vec![1, 3]);
        assert_eq!(orders(find(&groups, "Flöte", None)), vec![0, 4]);
        assert_eq!(orders(find(&groups, UNKNOWN_NAME, None)), vec![2, 5]);
    }

    #[test]
    fn own_label_attaches_a_stray_continuation() {
        let stray = page(5, Some("Oboe"), None, false);
        let groups = build_groups(&[
            front(0, "Flöte", None),
            front(1, "Oboe", None),
            back(2),
            back(3),
            back(4),
            front(10, "Tuba C", None),
            stray,
        ]);
        assert!(find(&groups, "Tuba C", None).pages.len() >= 1);
        assert!(
            find(&groups, "Oboe", None)
                .pages
                .iter()
                .any(|page| page.global_order == 5)
        );
    }

    #[test]
    fn unread_voice_number_is_inherited_from_a_duplicate_first_page() {
        let groups = build_groups(&[
            front(0, "Klarinette", Some(1)),
            front(1, "Klarinette", None),
            front(2, "Klarinette", Some(2)),
        ]);
        assert_eq!(orders(find(&groups, "Klarinette", Some(1))), vec![1]);
        assert_eq!(orders(find(&groups, "Klarinette", Some(2))), vec![2]);
        assert!(
            groups
                .iter()
                .any(|group| group.key.label() == "Klarinette 1 - Copy 1")
        );
    }

    fn with_words(mut page: PageImage, words: &str) -> PageImage {
        page.analysis.text_lines = vec![crate::OcrLine {
            text: words.to_owned(),
            confidence: 0.9,
            left: 0.0,
            top: 0.0,
            height: 10.0,
            width: 100.0,
        }];
        page
    }

    fn group(instrument: &str, copy: u32, pages: Vec<PageImage>) -> InstrumentGroup {
        InstrumentGroup {
            key: GroupKey {
                piece: None,
                instrument_name: instrument.to_owned(),
                voice_number: Some(1),
                copy,
            },
            pages,
            output_filename: String::new(),
        }
    }

    #[test]
    fn a_copy_without_its_continuation_pages_goes_to_unknown() {
        let mut groups = vec![
            group(
                "Trompete",
                0,
                vec![
                    front(0, "Trompete", Some(1)),
                    with_words(back(2), "Finale Allegro Coda Ritardando"),
                ],
            ),
            group("Trompete", 1, vec![front(1, "Trompete", Some(1))]),
        ];
        sort_out_repeated_scans(&mut groups);
        // The later first page is the better scan; the other one is left over.
        assert_eq!(orders(find(&groups, "Trompete", Some(1))), vec![1, 2]);
        assert_eq!(orders(find(&groups, DUPLICATED_NAME, None)), vec![0]);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn pages_without_page_numbers_follow_their_bar_numbers() {
        let with_bar = |order, bar| {
            let mut page = back(order);
            page.analysis.last_bar = Some(bar);
            page
        };
        let mut part = group(
            "Saxophon Alt",
            0,
            vec![
                front(0, "Saxophon Alt", Some(1)),
                with_bar(1, 114),
                back(2),
                with_bar(3, 53),
            ],
        );
        part.pages[2].analysis.is_blank = true;
        order_group_pages(&mut part);
        assert_eq!(orders(&part), vec![0, 3, 1, 2]);
        // One page without a bar number: the scan order stays.
        let mut part = group(
            "Saxophon Alt",
            0,
            vec![
                front(0, "Saxophon Alt", Some(1)),
                with_bar(1, 114),
                back(2),
                with_bar(3, 53),
            ],
        );
        order_group_pages(&mut part);
        assert_eq!(orders(&part), vec![0, 1, 2, 3]);
    }

    fn labelled(mut page: PageImage, label: &str) -> PageImage {
        page.analysis.marks.label = Some(crate::ocr::OcrLine {
            text: label.to_owned(),
            confidence: 1.0,
            left: 0.0,
            top: 0.0,
            height: 10.0,
            width: 100.0,
        });
        page
    }

    #[test]
    fn baritone_tc_is_tenorhorn_only_next_to_a_bb_baritone() {
        let names = |pages: Vec<PageImage>| -> Vec<String> {
            build_groups(&pages)
                .iter()
                .map(|group| group.key.label())
                .collect()
        };
        let both = vec![
            labelled(front(0, "Bariton B", None), "Baritone TC"),
            labelled(front(1, "Bariton B", None), "Bb Baritone"),
            labelled(front(2, "Bariton C", None), "Baritone BC"),
        ];
        assert_eq!(names(both), ["Tenorhorn", "Bariton B", "Bariton C"]);
        let alone = vec![
            labelled(front(0, "Bariton B", None), "Baritone TC"),
            labelled(front(1, "Bariton C", None), "Baritone BC"),
        ];
        assert_eq!(names(alone), ["Bariton B", "Bariton C"]);
    }

    #[test]
    fn a_label_that_lost_its_first_letters_finds_its_part() {
        let labelled = |order, raw: &str, instrument: &str, part_start| {
            let mut page = page(order, Some(instrument), None, part_start);
            page.analysis.instrument_raw = Some(raw.to_owned());
            page
        };
        let groups = build_groups(&[
            labelled(0, "E Bass", "Gitarre Bass", true),
            labelled(1, "Bb Bass clarinet", "Klarinette Bass", true),
            labelled(2, "Bassoon", "Bassoon", true),
            // OCR read "- Bass" for "E - Bass": as a plain "Bass" it names no part.
            labelled(3, "Bass", "Bass C", false),
            labelled(4, "Bassoon", "Bassoon", false),
        ]);
        assert_eq!(orders(find(&groups, "Gitarre Bass", None)), vec![0, 3]);
    }

    #[test]
    fn a_page_scanned_twice_keeps_its_better_scan() {
        let mut crooked = with_words(back(2), "Finale Allegro Coda Ritardando Fermata");
        crooked.analysis.skew_degrees = 1.5;
        let mut groups = vec![group(
            "Horn Es",
            0,
            vec![
                front(0, "Horn Es", Some(1)),
                with_words(back(1), "Intro Swing Andante tempo"),
                crooked,
                with_words(back(3), "Finale Allegro Coda Ritardando Fermata"),
            ],
        )];
        sort_out_repeated_scans(&mut groups);
        assert_eq!(orders(find(&groups, "Horn Es", Some(1))), vec![0, 1, 3]);
        assert_eq!(orders(find(&groups, DUPLICATED_NAME, None)), vec![2]);
    }

    #[test]
    fn complete_duplicate_scans_stay_whole_copies() {
        let mut groups = vec![
            group(
                "Horn F",
                0,
                vec![
                    front(0, "Horn F", Some(1)),
                    with_words(back(1), "Finale Allegro Coda"),
                ],
            ),
            group(
                "Horn F",
                1,
                vec![
                    front(2, "Horn F", Some(1)),
                    with_words(back(3), "Intro Swing Andante"),
                ],
            ),
        ];
        sort_out_repeated_scans(&mut groups);
        assert_eq!(groups.len(), 2);
        assert_eq!(orders(&groups[0]), vec![0, 1]);
        assert_eq!(orders(&groups[1]), vec![2, 3]);
    }

    #[test]
    fn duplicate_scans_become_numbered_copies_with_the_last_scan_as_main_file() {
        let groups = build_groups(&[
            front(0, "Gesang", None),
            front(1, "Gesang", None),
            front(2, "Oboe", None),
            back(3),
            back(4),
            back(5),
        ]);
        assert_eq!(orders(find(&groups, "Gesang", None)), vec![1, 4]);
        let copy = groups
            .iter()
            .find(|group| group.key.instrument_name == "Gesang" && group.key.copy == 1)
            .expect("copy group");
        assert_eq!(orders(copy), vec![0, 5]);
        assert_eq!(copy.key.label(), "Gesang - Copy 1");
        assert_eq!(orders(find(&groups, "Oboe", None)), vec![2, 3]);
    }

    #[test]
    fn a_clearly_crooked_duplicate_scan_is_not_the_main_file() {
        let mut straight = front(0, "Gesang", None);
        straight.analysis.skew_degrees = 0.25;
        straight.analysis.ink_darkness = 0.50;
        let mut crooked = front(1, "Gesang", None);
        crooked.analysis.skew_degrees = -1.6;
        crooked.analysis.ink_darkness = 0.40;
        let groups = build_groups(&[straight, crooked, back(2), back(3)]);
        assert_eq!(orders(find(&groups, "Gesang", None)), vec![0, 3]);
    }

    #[test]
    fn the_less_over_darkened_duplicate_scan_becomes_the_main_file() {
        let mut light = front(0, "Gesang", None);
        light.analysis.skew_degrees = 0.3;
        light.analysis.ink_darkness = 0.32;
        let mut dark = front(1, "Gesang", None);
        dark.analysis.skew_degrees = 0.0;
        dark.analysis.ink_darkness = 0.38;
        let groups = build_groups(&[light, dark, back(2), back(3)]);
        assert_eq!(orders(find(&groups, "Gesang", None)), vec![0, 3]);
    }

    #[test]
    fn a_duplicate_scan_with_a_crooked_back_page_is_not_the_main_file() {
        let mut first = front(0, "Posaune", Some(1));
        first.analysis.skew_degrees = -0.45;
        let mut rescan = front(1, "Posaune", Some(1));
        rescan.analysis.skew_degrees = 0.4;
        let mut crooked_back = back(2);
        crooked_back.analysis.skew_degrees = 0.6;
        let groups = build_groups(&[first, rescan, crooked_back, back(3)]);
        assert_eq!(orders(find(&groups, "Posaune", Some(1))), vec![0, 3]);
    }

    #[test]
    fn unnumbered_rescans_fill_the_voice_that_is_short_of_scans() {
        let groups = build_groups(&[
            front(0, "Klarinette", None),
            front(1, "Klarinette", Some(3)),
            front(2, "Horn F", None),
            front(3, "Horn F", Some(2)),
            front(4, "Klarinette", None),
            front(5, "Klarinette", Some(3)),
            front(6, "Horn F", Some(1)),
            front(7, "Horn F", Some(2)),
        ]);
        let scans = |name: &str, voice| {
            let mut found: Vec<usize> = groups
                .iter()
                .filter(|group| {
                    group.key.instrument_name == name && group.key.voice_number == voice
                })
                .map(|group| group.pages[0].global_order)
                .collect();
            found.sort();
            found
        };
        assert_eq!(scans("Klarinette", Some(1)), vec![0, 4]);
        assert_eq!(scans("Horn F", Some(1)), vec![2, 6]);
        assert!(scans("Klarinette", Some(2)).is_empty());
    }

    #[test]
    fn missing_voice_one_is_filled_when_two_and_three_were_read() {
        let groups = build_groups(&[
            front(0, "Klarinette", None),
            front(1, "Klarinette", Some(2)),
            front(2, "Klarinette", Some(3)),
        ]);
        assert_eq!(orders(find(&groups, "Klarinette", Some(1))), vec![0]);
    }

    #[test]
    fn pages_after_a_direktion_cover_belong_to_it() {
        let groups = build_groups(&[
            front(0, "Flöte", None),
            back(1),
            front(2, "Direktion", None),
            page(3, None, None, true),
            back(4),
            page(5, None, None, true),
        ]);
        assert_eq!(orders(find(&groups, "Direktion", None)), vec![2, 3, 4, 5]);
        assert_eq!(orders(find(&groups, "Flöte", None)), vec![0, 1]);
    }

    #[test]
    fn headerless_first_pages_are_named_from_their_mirrored_back_pages() {
        // A back page with its label above the staff looks like a first page without a title.
        let mut labelled_back = page(3, Some("Oboe"), None, true);
        labelled_back.analysis.has_title_evidence = false;
        let groups = build_groups(&[
            back(0),
            back(1),
            back(2),
            labelled_back,
            page(4, Some("Trompete"), Some(2), false),
            page(5, Some("Flöte"), None, false),
            front(6, "Tuba C", None),
        ]);
        assert_eq!(orders(find(&groups, "Flöte", None)), vec![0, 5]);
        assert_eq!(orders(find(&groups, "Trompete", Some(2))), vec![1, 4]);
        assert_eq!(orders(find(&groups, "Oboe", None)), vec![2, 3]);
    }

    #[test]
    fn later_full_rescans_do_not_stop_naming_headerless_first_pages() {
        let legible = |order, instrument, voice| {
            let mut page = front(order, instrument, voice);
            page.analysis.instrument_raw = Some(instrument.to_owned());
            page
        };
        let groups = build_groups(&[
            back(0),
            back(1),
            back(2),
            page(3, Some("Oboe"), None, false),
            page(4, Some("Trompete"), Some(2), false),
            page(5, Some("Flöte"), None, false),
            legible(6, "Flöte", None),
            legible(7, "Trompete", Some(2)),
            legible(8, "Oboe", None),
        ]);
        let named = |name: &str, voice, expected: Vec<usize>| {
            groups.iter().any(|group| {
                group.key.instrument_name == name
                    && group.key.voice_number == voice
                    && orders(group) == expected
            })
        };
        // The headerless first pages are named and paired with their backs; as incomplete
        // scans they then give way to the legible re-scans, which take the back pages.
        assert!(named("Flöte", None, vec![6, 5]));
        assert!(named("Trompete", Some(2), vec![7, 4]));
        assert!(named("Oboe", None, vec![8, 3]));
        assert!(named(DUPLICATED_NAME, None, vec![0, 1, 2]));
    }

    #[test]
    fn labelled_backs_of_known_first_pages_do_not_create_new_first_pages() {
        let groups = build_groups(&[
            front(0, "Flöte", None),
            front(1, "Oboe", None),
            front(2, "Tuba C", None),
            front(3, "Gesang", None),
            back(4),
            back(5),
            back(6),
            page(7, Some("Oboe"), None, false),
            page(8, Some("Flöte"), None, false),
        ]);
        assert_eq!(orders(find(&groups, "Flöte", None)), vec![0, 8]);
        assert!(
            groups
                .iter()
                .filter(|group| !is_unknown(&group.key))
                .all(|group| ![4, 5, 6].contains(&group.pages[0].global_order))
        );
    }

    fn of_piece(mut page: PageImage, piece: &str) -> PageImage {
        page.analysis.piece = Some(piece.to_owned());
        page
    }

    #[test]
    fn the_same_part_of_two_pieces_is_no_duplicate_scan() {
        let groups = build_groups(&[
            front(0, "Flöte", None),
            of_piece(front(1, "Flöte", None), "Andere Weise"),
        ]);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|group| group.key.copy == 0));
        let other = groups
            .iter()
            .find(|group| group.key.piece.is_some())
            .unwrap();
        assert_eq!(orders(other), vec![1]);
    }

    #[test]
    fn duplicate_scans_are_counted_per_piece() {
        let groups = build_groups(&[
            of_piece(front(0, "Flöte", None), "Andere Weise"),
            front(1, "Flöte", None),
            of_piece(front(2, "Flöte", None), "Andere Weise"),
        ]);
        let copies = |piece: Option<&str>| {
            groups
                .iter()
                .filter(|group| group.key.piece.as_deref() == piece)
                .count()
        };
        assert_eq!(copies(None), 1);
        assert_eq!(copies(Some("Andere Weise")), 2);
    }

    #[test]
    fn back_pages_follow_the_piece_of_their_mirrored_first_page() {
        let groups = build_groups(&[
            front(0, "Flöte", None),
            of_piece(front(1, "Oboe", None), "Andere Weise"),
            back(2),
            back(3),
        ]);
        let other = groups
            .iter()
            .find(|group| group.key.piece.is_some())
            .unwrap();
        assert_eq!(orders(other), vec![1, 2]);
    }

    #[test]
    fn a_label_finds_its_part_in_the_only_piece_that_has_it() {
        let known = HashSet::from([GroupKey {
            piece: Some("Andere Weise".to_owned()),
            instrument_name: "Oboe".to_owned(),
            voice_number: None,
            copy: 0,
        }]);
        let label = GroupKey {
            piece: None,
            instrument_name: "Oboe".to_owned(),
            voice_number: None,
            copy: 0,
        };
        assert_eq!(
            resolve_label(&label, &known).unwrap().piece.as_deref(),
            Some("Andere Weise")
        );
    }

    #[test]
    fn blank_pages_keep_mirror_positions_but_are_not_written() {
        let mut blank = back(3);
        blank.analysis.is_blank = true;
        let groups = build_groups(&[
            front(0, "Flöte", Some(1)),
            front(1, "Flöte", Some(2)),
            blank,
            back(4),
        ]);
        assert_eq!(orders(find(&groups, "Flöte", Some(1))), vec![0, 4]);
        assert_eq!(orders(find(&groups, "Flöte", Some(2))), vec![1]);
    }
}
