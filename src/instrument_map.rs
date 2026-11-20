use std::cmp::Reverse;

struct Mapping {
    patterns: &'static [&'static str],
    clef: Option<&'static str>,
    output: &'static str,
}

const MAP: &[Mapping] = &[
    // Early in the list: among equally long matches the later one wins, so "Bass Tuba" stays Tuba.
    Mapping {
        patterns: &["bass", "c bass", "bass c", "bass in c"],
        clef: Some("bass"),
        output: "Bass C",
    },
    Mapping {
        patterns: &["bass"],
        clef: Some("treble"),
        output: "Bass B",
    },
    Mapping {
        patterns: &["bariton"],
        clef: Some("treble"),
        output: "Bariton B",
    },
    Mapping {
        patterns: &["bariton"],
        clef: Some("bass"),
        output: "Bariton C",
    },
    Mapping {
        patterns: &["bariton b", "b bariton", "bariton in b", "bariton in bb"],
        clef: Some("treble"),
        output: "Bariton B",
    },
    Mapping {
        patterns: &["bariton b", "b bariton", "bariton in b", "bariton in bb"],
        clef: Some("bass"),
        output: "Bariton B C",
    },
    Mapping {
        patterns: &["bariton c", "c bariton", "bariton in c"],
        clef: Some("bass"),
        output: "Bariton C",
    },
    Mapping {
        patterns: &[
            "tenorhorn",
            "b tenorhorn",
            "bb tenorhorn",
            "tenorhorn in b",
            "tenorhorn in bb",
            "tenor horn",
        ],
        clef: Some("treble"),
        output: "Tenorhorn",
    },
    Mapping {
        patterns: &["flöte", "flute", "floete", "flöte in c"],
        clef: Some("treble"),
        output: "Flöte",
    },
    Mapping {
        patterns: &["flöte in des", "flöte des", "des flöte"],
        clef: Some("treble"),
        output: "Flöte Des",
    },
    Mapping {
        patterns: &["piccolo"],
        clef: Some("treble"),
        output: "Flöte Piccolo",
    },
    Mapping {
        patterns: &[
            "flügelhorn",
            "fluegelhorn",
            // The English and Dutch name of the Flügelhorn.
            "bugle",
            "flügelhorn in b",
            "flügelhorn in bb",
        ],
        clef: Some("treble"),
        output: "Flügelhorn",
    },
    Mapping {
        patterns: &[
            "horn",
            "horn f",
            "f horn",
            "horn in f",
            "horn in f eb",
            "horn in f e",
            "horninf",
            "althorn f",
            "althorn in f",
            "althorn melodie in f",
        ],
        clef: Some("treble"),
        output: "Horn F",
    },
    Mapping {
        patterns: &["horn"],
        clef: Some("bass"),
        output: "Horn F",
    },
    // A melody part for the horns, beside the plain "Horn in F" part of the same piece.
    Mapping {
        patterns: &["horn melodie", "horn melodie in f", "f horn melodie"],
        clef: None,
        output: "Horn F Melodie",
    },
    Mapping {
        patterns: &[
            "horn es",
            "es horn",
            "eb horn",
            "horn eb",
            "horn in eb",
            "althorn",
            "es althorn",
            "althorn in es",
            "althorn melodie",
        ],
        clef: Some("treble"),
        output: "Horn Es",
    },
    Mapping {
        patterns: &[
            "klarinette",
            "clarinette",
            "b klarinette",
            "bb klarinette",
            "klarinette in b",
            "klarinette in bb",
        ],
        clef: Some("treble"),
        output: "Klarinette",
    },
    Mapping {
        patterns: &[
            "klarinette in es",
            "klarinette in eb",
            "klarinette in e",
            "es klarinette",
            "eb klarinette",
            "klarinette es",
        ],
        clef: Some("treble"),
        output: "Klarinette Es",
    },
    Mapping {
        patterns: &[
            "bassklarinette",
            "b bassklarinette",
            "bb bassklarinette",
            "bassklarinette in b",
            "bassklarinette in bb",
            "bass klarinette",
        ],
        clef: None,
        output: "Klarinette Bass",
    },
    Mapping {
        patterns: &["bassposaune", "bass posaune", "bass trombone"],
        clef: None,
        output: "Posaune Bass",
    },
    Mapping {
        patterns: &[
            "posaune",
            "c posaune",
            "trombone",
            "c trombone",
            "posaune in c",
        ],
        clef: Some("bass"),
        output: "Posaune",
    },
    Mapping {
        patterns: &[
            "posaune b",
            "b posaune",
            "b trombone",
            "posaune in b",
            "posaune in bb",
        ],
        clef: Some("treble"),
        output: "Posaune B",
    },
    Mapping {
        patterns: &[
            "posaune b",
            "b posaune",
            "b trombone",
            "posaune in b",
            "posaune in bb",
        ],
        clef: Some("bass"),
        output: "Posaune B C",
    },
    Mapping {
        patterns: &[
            "altsaxophon",
            "es altsaxophon",
            "eb altsaxophon",
            "alt saxophon",
            "altsaxophon in eb",
            "saxophon eb",
            "saxophon es",
            "saxophon e",
        ],
        clef: Some("treble"),
        output: "Saxophon Alt",
    },
    Mapping {
        patterns: &[
            "tenorsaxophon",
            "b tenorsaxophon",
            "bb tenorsaxophon",
            "tenor saxophon",
            "tenorsaxophon in b",
            "tenorsaxophon in bb",
        ],
        clef: Some("treble"),
        output: "Saxophon Tenor",
    },
    Mapping {
        patterns: &[
            "baritonsaxophon",
            "e baritonsaxophon",
            "eb baritonsaxophon",
            "baritonsaxophon in eb",
        ],
        clef: Some("treble"),
        output: "Saxophon Bariton",
    },
    Mapping {
        patterns: &[
            "trompete",
            "b trompete",
            "bb trompete",
            "trumpet",
            "trompete in b",
            "trompete in bb",
        ],
        clef: Some("treble"),
        output: "Trompete",
    },
    Mapping {
        patterns: &[
            "solo trompete",
            "solo trompete in b",
            "trompete solo",
            "solo trumpet",
        ],
        clef: Some("treble"),
        output: "Trompete Solo",
    },
    Mapping {
        patterns: &["tuba", "c tuba", "tuba in c"],
        clef: Some("bass"),
        output: "Tuba C",
    },
    Mapping {
        patterns: &["tuba b", "b tuba", "bb tuba", "tuba in b", "tuba in bb"],
        clef: Some("treble"),
        output: "Tuba B",
    },
    Mapping {
        patterns: &["tuba b", "b tuba", "bb tuba", "tuba in b", "tuba in bb"],
        clef: Some("bass"),
        output: "Tuba B C",
    },
    Mapping {
        patterns: &["tuba es", "es tuba", "eb tuba", "tuba in eb"],
        clef: Some("treble"),
        output: "Tuba Es",
    },
    Mapping {
        patterns: &["schlagzeug", "drums"],
        clef: Some("treble"),
        output: "Drums Schlagzeug",
    },
    Mapping {
        patterns: &["percussion"],
        clef: Some("treble"),
        output: "Drums Percussion",
    },
    Mapping {
        patterns: &["mallet percussion"],
        clef: Some("treble"),
        output: "Drums Mallet Percussion",
    },
    Mapping {
        patterns: &["glockenspiel", "lyra"],
        clef: Some("treble"),
        output: "Drums Glockenspiel",
    },
    Mapping {
        patterns: &["kleine trommel"],
        clef: Some("treble"),
        output: "Drums Kleine Trommel",
    },
    Mapping {
        patterns: &["timpani"],
        clef: Some("treble"),
        output: "Drums Timpani",
    },
    Mapping {
        patterns: &["pauken"],
        clef: Some("treble"),
        output: "Drums Pauken",
    },
    Mapping {
        patterns: &["fagott"],
        clef: Some("treble"),
        output: "Fagott",
    },
    Mapping {
        patterns: &["bassoon"],
        clef: Some("treble"),
        output: "Bassoon",
    },
    Mapping {
        patterns: &[
            "euphonium b",
            "b euphonium",
            "bb euphonium",
            "euphonium in b",
        ],
        clef: Some("treble"),
        output: "Euphonium B",
    },
    Mapping {
        patterns: &[
            "euphonium b",
            "b euphonium",
            "bb euphonium",
            "euphonium in b",
        ],
        clef: Some("bass"),
        output: "Euphonium B C",
    },
    Mapping {
        patterns: &["euphonium", "euphonium c", "c euphonium", "euphonium in c"],
        clef: Some("bass"),
        output: "Euphonium C",
    },
    Mapping {
        patterns: &["euphonium"],
        clef: Some("treble"),
        output: "Euphonium B",
    },
    Mapping {
        patterns: &["baritone horn"],
        clef: Some("bass"),
        output: "Euphonium C",
    },
    Mapping {
        patterns: &["baritone horn"],
        clef: Some("treble"),
        output: "Euphonium B",
    },
    Mapping {
        patterns: &["tuba es", "es tuba", "eb tuba", "tuba in eb"],
        clef: Some("bass"),
        output: "Tuba Es C",
    },
    Mapping {
        patterns: &["kontrabass"],
        clef: None,
        output: "Kontrabass",
    },
    Mapping {
        patterns: &["double bass", "bass double"],
        clef: None,
        output: "Bass Double",
    },
    Mapping {
        patterns: &["bass guitar", "guitar bass", "e bass"],
        clef: None,
        output: "Gitarre Bass",
    },
    Mapping {
        patterns: &["b bass", "bass b", "bass in b", "bass in bb"],
        clef: None,
        output: "Bass B",
    },
    Mapping {
        patterns: &["bass es", "bass eb", "bass in es", "bass in eb"],
        clef: None,
        output: "Bass Es",
    },
    // After "e bass": the text folding makes "E-Bass" and "Es-Bass" the same. An electric bass
    // is written in bass clef, so only the treble-clef reading is the wind instrument.
    Mapping {
        patterns: &["es bass"],
        clef: Some("treble"),
        output: "Bass Es",
    },
    Mapping {
        patterns: &["guitar", "e guitar", "electric guitar", "gitarre"],
        clef: None,
        output: "Gitarre E",
    },
    Mapping {
        patterns: &["drum set", "drumset"],
        clef: None,
        output: "Drums Set",
    },
    Mapping {
        patterns: &["becken grosse trommel", "becken"],
        clef: None,
        output: "Drums Becken Große Trommel",
    },
    Mapping {
        patterns: &["grosse trommel", "bass drum"],
        clef: None,
        output: "Drums Große Trommel",
    },
    Mapping {
        patterns: &["mallets"],
        clef: None,
        output: "Drums Mallets",
    },
    Mapping {
        patterns: &["clarinet", "b clarinet", "bb clarinet", "clarinet in b"],
        clef: Some("treble"),
        output: "Klarinette",
    },
    Mapping {
        patterns: &["eb clarinet", "es clarinet", "clarinet in eb"],
        clef: Some("treble"),
        output: "Klarinette Es",
    },
    Mapping {
        patterns: &["bass clarinet", "b bass clarinet", "bb bass clarinet"],
        clef: None,
        output: "Klarinette Bass",
    },
    Mapping {
        patterns: &["alto saxophone", "alto sax", "eb alto saxophone"],
        clef: Some("treble"),
        output: "Saxophon Alt",
    },
    Mapping {
        patterns: &["tenor saxophone", "tenor sax", "bb tenor saxophone"],
        clef: Some("treble"),
        output: "Saxophon Tenor",
    },
    Mapping {
        patterns: &[
            "baritone saxophone",
            "baritone sax",
            "eb baritone saxophone",
        ],
        clef: Some("treble"),
        output: "Saxophon Bariton",
    },
    Mapping {
        patterns: &[
            "b solo trombone",
            "bb solo trombone",
            "posaune b solo",
            "b solo posaune",
        ],
        clef: Some("treble"),
        output: "Posaune B Solo",
    },
    Mapping {
        patterns: &[
            "b solo trombone",
            "bb solo trombone",
            "posaune b solo",
            "b solo posaune",
        ],
        clef: Some("bass"),
        output: "Posaune B C Solo",
    },
    Mapping {
        patterns: &["solo trombone", "c solo trombone", "posaune solo"],
        clef: Some("bass"),
        output: "Posaune Solo",
    },
    Mapping {
        patterns: &["posaune", "trombone"],
        clef: Some("treble"),
        output: "Posaune B",
    },
    Mapping {
        patterns: &["partitur", "score"],
        clef: None,
        output: "Partitur",
    },
    Mapping {
        patterns: &["oboe"],
        clef: Some("treble"),
        output: "Oboe",
    },
    Mapping {
        patterns: &["gesang"],
        clef: Some("treble"),
        output: "Gesang",
    },
    Mapping {
        patterns: &["direktion", "direktion in c", "direktion in b"],
        clef: None,
        output: "Direktion",
    },
];

pub fn normalize_text(text: &str) -> String {
    // OCR drops the stem of a small "E" in "E♭" and reads it as "-b".
    // A small "ß" in "Baß" also comes out as a letter.
    let text = text
        .split_whitespace()
        .map(|token| match token {
            "-b" => "eb",
            "BaR" | "BaB" | "Bal" => "bass",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut folded = String::new();
    for character in text.to_lowercase().chars() {
        match character {
            'ä' => folded.push('a'),
            'ö' => folded.push('o'),
            'ü' => folded.push('u'),
            'ß' => folded.push_str("ss"),
            '♭' => folded.push('b'),
            other => folded.push(other),
        }
    }
    // The OCR alphabet has no umlauts or ß and reads them as "?".
    let folded = folded
        .replace("fl?te", "flote")
        .replace("fl?gel", "flugel")
        // OCR also reads the "ü" as "ii", and glues the "in" to "Melodie".
        .replace("fliigel", "flugel")
        .replace("melodiein", "melodie in ")
        .replace("ba?", "bass")
        .replace("gro?e", "grosse")
        .replace("b flat", "bb")
        .replace("e flat", "eb");
    let chars: Vec<char> = folded.chars().collect();
    let mut output = String::with_capacity(folded.len());
    let mut previous_space = true;
    for character in chars {
        if character.is_ascii_alphanumeric() {
            output.push(character);
            previous_space = false;
        } else if !previous_space {
            output.push(' ');
            previous_space = true;
        }
    }
    // Key prefixes: "Bb"/"Eb" are written forms of "B"/"Es". OCR reads "B♭" as "Rb", "3b",
    // "BL", "Bh" or a leading "8", and "E♭" as "Fb" (there is no F-flat instrument), "EL", "Eh"
    // or "E".
    let tokens: Vec<&str> = output.split_whitespace().collect();
    let mut canonical: Vec<&str> = Vec::with_capacity(tokens.len());
    for (index, token) in tokens.iter().enumerate() {
        // "EBass": the electric bass with its letter glued on.
        if *token == "ebass" {
            canonical.extend(["es", "bass"]);
            continue;
        }
        if let Some(rest) = glued_key_remainder(token) {
            canonical.push(if token.starts_with('e') { "es" } else { "b" });
            canonical.push(rest);
            continue;
        }
        canonical.push(match *token {
            "bb" | "rb" | "3b" | "bl" | "bh" => "b",
            "eb" | "fb" | "fs" | "el" | "e" | "eh" => "es",
            "8" if index == 0 && tokens.len() > 1 => "b",
            other => other,
        });
    }
    canonical.join(" ")
}

/// Splits a flat key glued to an instrument word, e.g. "bltuba" → "tuba".
fn glued_key_remainder(token: &str) -> Option<&str> {
    let rest = ["bl", "bb", "eb"]
        .iter()
        .find_map(|key| token.strip_prefix(key))?;
    (rest.len() >= 4
        && MAP.iter().any(|mapping| {
            mapping
                .patterns
                .iter()
                .any(|pattern| pattern.split_whitespace().any(|word| word == rest))
        }))
    .then_some(rest)
}

pub fn normalize_instrument(text: &str, clef: Option<&str>) -> Option<String> {
    let normalized = normalize_text(text);
    let matches = MAP
        .iter()
        .flat_map(|mapping| {
            mapping.patterns.iter().filter_map(|pattern| {
                let pattern = normalize_text(pattern);
                contains_phrase(&normalized, &pattern).then_some((
                    pattern.len(),
                    mapping.clef,
                    mapping.output,
                ))
            })
        })
        .filter(|(_, constraint, _)| clef.is_none() || constraint.is_none() || *constraint == clef)
        .max_by_key(|(length, _, _)| *length);
    matches
        .or_else(|| {
            MAP.iter()
                .flat_map(|mapping| {
                    mapping.patterns.iter().filter_map(|pattern| {
                        let normalized_pattern = normalize_text(pattern);
                        (fuzzy_pattern_allowed(&normalized_pattern)
                            && fuzzy_contains(&normalized, &normalized_pattern))
                        .then_some((normalized_pattern.len(), mapping.clef, mapping.output))
                    })
                })
                .filter(|(_, constraint, _)| {
                    clef.is_none() || constraint.is_none() || *constraint == clef
                })
                .max_by_key(|(length, _, _)| *length)
        })
        .map(|(_, _, output)| output.to_owned())
}

pub fn normalize_instrument_with_fallback(text: &str, clef: Option<&str>) -> Option<String> {
    if let Some(clef) = clef {
        if let Some(mapped) = normalize_instrument(text, Some(clef)) {
            return Some(mapped);
        }
    }
    let alternatives = ["treble", "bass"]
        .into_iter()
        .filter_map(|candidate| normalize_instrument(text, Some(candidate)))
        .collect::<std::collections::HashSet<_>>();
    (alternatives.len() == 1)
        .then(|| alternatives.into_iter().next())
        .flatten()
}

pub fn match_candidates(text: &str) -> Vec<(usize, &'static str, Option<&'static str>)> {
    let normalized = normalize_text(text);
    let mut matches = MAP
        .iter()
        .flat_map(|mapping| {
            mapping.patterns.iter().filter_map(|pattern| {
                let normalized_pattern = normalize_text(pattern);
                (contains_phrase(&normalized, &normalized_pattern)
                    || (fuzzy_pattern_allowed(&normalized_pattern)
                        && fuzzy_contains(&normalized, &normalized_pattern)))
                .then_some((normalized_pattern.len(), mapping.output, mapping.clef))
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|(length, _, _)| Reverse(*length));
    matches
}

pub fn starts_with_instrument_pattern(text: &str) -> bool {
    let normalized = normalize_text(text);
    MAP.iter().any(|mapping| {
        mapping.patterns.iter().any(|pattern| {
            let pattern = normalize_text(pattern);
            normalized == pattern
                || normalized
                    .strip_prefix(&pattern)
                    .is_some_and(|suffix| suffix.starts_with(' '))
                || fuzzy_starts_with(&normalized, &pattern)
        })
    })
}

fn contains_phrase(text: &str, phrase: &str) -> bool {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .windows(phrase.split_whitespace().count())
        .any(|window| window.join(" ") == phrase)
}

fn fuzzy_contains(text: &str, phrase: &str) -> bool {
    let phrase = phrase
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<Vec<_>>();
    if phrase.len() < 5 {
        return false;
    }
    // A match starts at a word: "in F Gute" must not read as "flute" across the word break.
    let mut word_starts = Vec::new();
    let mut letters = Vec::new();
    for word in text.split_whitespace() {
        word_starts.push(letters.len());
        letters.extend(word.chars().filter(char::is_ascii_alphanumeric));
    }
    let minimum = phrase.len().saturating_sub(1);
    let maximum = phrase.len() + 1;
    (minimum..=maximum).any(|length| {
        word_starts.iter().any(|start| {
            letters
                .get(*start..*start + length)
                .is_some_and(|window| levenshtein(window, &phrase) <= 1)
        })
    })
}

fn fuzzy_pattern_allowed(pattern: &str) -> bool {
    !pattern
        .split_whitespace()
        .any(|token| ["b", "bb", "c", "eb", "es", "e", "f"].contains(&token))
}

fn fuzzy_starts_with(text: &str, phrase: &str) -> bool {
    let phrase = phrase
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<Vec<_>>();
    if phrase.len() < 5 {
        return false;
    }
    let text = text
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<Vec<_>>();
    let minimum = phrase.len().saturating_sub(1);
    let maximum = phrase.len() + 1;
    (minimum..=maximum)
        .filter(|length| *length <= text.len())
        .any(|length| levenshtein(&text[..length], &phrase) <= 1)
}

fn levenshtein(left: &[char], right: &[char]) -> usize {
    let mut row = (0..=right.len()).collect::<Vec<_>>();
    for (i, left_char) in left.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(previous + usize::from(left_char != right_char));
            previous = old;
        }
    }
    row[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_spec_mapping_and_clef_variant() {
        assert_eq!(
            normalize_instrument("Bariton in B", Some("treble")).as_deref(),
            Some("Bariton B")
        );
        assert_eq!(
            normalize_instrument("Bariton in B", Some("bass")).as_deref(),
            Some("Bariton B C")
        );
        assert_eq!(
            normalize_instrument("Glockenspiel", Some("treble")).as_deref(),
            Some("Drums Glockenspiel")
        );
        assert_eq!(
            normalize_instrument("Gesang", Some("treble")).as_deref(),
            Some("Gesang")
        );
        assert_eq!(
            normalize_instrument("Mallet Percussion", Some("treble")).as_deref(),
            Some("Drums Mallet Percussion")
        );
    }

    #[test]
    fn folds_german_and_flat_notation() {
        assert_eq!(
            normalize_instrument("Flöte", Some("treble")).as_deref(),
            Some("Flöte")
        );
        assert_eq!(
            normalize_instrument("Trompete in B♭", Some("treble")).as_deref(),
            Some("Trompete")
        );
        assert_eq!(
            normalize_instrument("Tuba in Eb", Some("treble")).as_deref(),
            Some("Tuba Es")
        );
    }

    #[test]
    fn reads_ocr_variants_of_flat_key_prefixes() {
        let bass = Some("bass");
        assert_eq!(
            normalize_instrument("Bb Trombone", bass).as_deref(),
            Some("Posaune B C")
        );
        assert_eq!(
            normalize_instrument("Fb Tuba", bass).as_deref(),
            Some("Tuba Es C")
        );
        assert_eq!(
            normalize_instrument("-b Tuba", bass).as_deref(),
            Some("Tuba Es C")
        );
        assert_eq!(
            normalize_instrument("Rb Euphonium", bass).as_deref(),
            Some("Euphonium B C")
        );
        assert_eq!(
            normalize_instrument("Fb Horn", Some("treble")).as_deref(),
            Some("Horn Es")
        );
        assert_eq!(
            normalize_instrument("E, HORN", Some("treble")).as_deref(),
            Some("Horn Es")
        );
        assert_eq!(
            normalize_instrument("BLTUBA", Some("treble")).as_deref(),
            Some("Tuba B")
        );
        assert_eq!(
            normalize_instrument("8 TROMBONE", bass).as_deref(),
            Some("Posaune B C")
        );
    }

    #[test]
    fn maps_melody_and_solo_parts_apart_from_the_plain_part() {
        let name = |text: &str| normalize_instrument_with_fallback(text, Some("treble"));
        assert_eq!(name("Horn in F").as_deref(), Some("Horn F"));
        assert_eq!(name("Horn-Melodie in F").as_deref(), Some("Horn F Melodie"));
        assert_eq!(name("Horn-MelodieinF").as_deref(), Some("Horn F Melodie"));
        assert_eq!(name("Horn-Melodiein").as_deref(), Some("Horn F Melodie"));
        assert_eq!(name("Trompete in Bb").as_deref(), Some("Trompete"));
        assert_eq!(name("Solo- Trompete in Rh").as_deref(), Some("Trompete Solo"));
        assert_eq!(name("Fliigelhorn in B").as_deref(), Some("Flügelhorn"));
    }

    #[test]
    fn maps_english_labels_with_ocr_errors() {
        let treble = Some("treble");
        let map = |text: &str| normalize_instrument_with_fallback(text, treble);
        assert_eq!(map("Bugle").as_deref(), Some("Flügelhorn"));
        assert_eq!(map("Buale").as_deref(), Some("Flügelhorn"));
        assert_eq!(map("Bh Trombone").as_deref(), Some("Posaune B"));
        assert_eq!(
            map("Eh Baritone Saxophone").as_deref(),
            Some("Saxophon Bariton")
        );
        assert_eq!(map("Eh Horn").as_deref(), Some("Horn Es"));
        assert_eq!(
            normalize_instrument_with_fallback("BASSOON", Some("bass")).as_deref(),
            Some("Bassoon")
        );
        assert_eq!(
            normalize_instrument_with_fallback("EBass", Some("bass")).as_deref(),
            Some("Gitarre Bass")
        );
    }

    #[test]
    fn tolerates_one_ocr_character_error_in_instrument_names() {
        assert_eq!(
            normalize_instrument_with_fallback("Fl?te", Some("treble")).as_deref(),
            Some("Flöte")
        );
        assert_eq!(
            normalize_instrument_with_fallback("Fl?gelhorn", Some("treble")).as_deref(),
            Some("Flügelhorn")
        );
        assert_eq!(
            normalize_instrument_with_fallback("Fl?gelhorn in Bb", Some("bass")).as_deref(),
            Some("Flügelhorn")
        );
        assert_eq!(
            normalize_instrument_with_fallback("Horn in F/Eb", Some("treble")).as_deref(),
            Some("Horn F")
        );
        assert_eq!(
            normalize_instrument_with_fallback("crecend", Some("treble")),
            None
        );
    }

    #[test]
    fn maps_unqualified_horn_using_either_clef() {
        assert_eq!(
            normalize_instrument_with_fallback("Horn", Some("treble")).as_deref(),
            Some("Horn F")
        );
        assert_eq!(
            normalize_instrument_with_fallback("Horn", Some("bass")).as_deref(),
            Some("Horn F")
        );
    }

    #[test]
    fn bass_takes_its_key_from_the_label_or_the_clef() {
        let name = |text: &str, clef: &str| normalize_instrument_with_fallback(text, Some(clef));
        assert_eq!(name("Bass", "bass").as_deref(), Some("Bass C"));
        assert_eq!(name("Bass", "treble").as_deref(), Some("Bass B"));
        assert_eq!(name("Baß", "bass").as_deref(), Some("Bass C"));
        assert_eq!(name("Ba?", "bass").as_deref(), Some("Bass C"));
        assert_eq!(name("Ba? in Bb", "treble").as_deref(), Some("Bass B"));
        assert_eq!(name("BaR", "bass").as_deref(), Some("Bass C"));
        assert_eq!(name("Bal", "bass").as_deref(), Some("Bass C"));
        for clef in ["treble", "bass"] {
            assert_eq!(name("Bass in B", clef).as_deref(), Some("Bass B"));
            assert_eq!(name("Bass in Es", clef).as_deref(), Some("Bass Es"));
        }
        assert_eq!(name("Es-Bass", "treble").as_deref(), Some("Bass Es"));
        assert_eq!(name("E-Bass", "bass").as_deref(), Some("Gitarre Bass"));
        assert_eq!(name("Bass Tuba", "bass").as_deref(), Some("Tuba C"));
        assert_eq!(name("Bassposaune", "bass").as_deref(), Some("Posaune Bass"));
        assert_eq!(name("Bassoon", "treble").as_deref(), Some("Bassoon"));
        // A wrongly detected bass clef must not turn a bass clarinet into a bass.
        assert_eq!(
            name("8 BASS CLARINET", "bass").as_deref(),
            Some("Klarinette Bass")
        );
    }

    #[test]
    fn maps_the_names_of_older_wind_band_editions() {
        let name = |text: &str, clef: &str| normalize_instrument_with_fallback(text, Some(clef));
        assert_eq!(name("Bariton", "treble").as_deref(), Some("Bariton B"));
        assert_eq!(name("Bariton", "bass").as_deref(), Some("Bariton C"));
        assert_eq!(
            name("Althorn-Melodie in F", "treble").as_deref(),
            Some("Horn F")
        );
        assert_eq!(
            name("Eb Althorn-Melodie", "treble").as_deref(),
            Some("Horn Es")
        );
        assert_eq!(
            name("Lyra", "treble").as_deref(),
            Some("Drums Glockenspiel")
        );
        assert_eq!(name("Oboe-Lyra", "treble").as_deref(), Some("Oboe"));
        assert_eq!(
            name("Große Trommel", "bass").as_deref(),
            Some("Drums Große Trommel")
        );
        assert_eq!(
            name("Gro?e Trommel", "bass").as_deref(),
            Some("Drums Große Trommel")
        );
        assert_eq!(name("Tenor Horn", "treble").as_deref(), Some("Tenorhorn"));
        assert_eq!(
            name("Baritone Horn", "bass").as_deref(),
            Some("Euphonium C")
        );
    }

    #[test]
    fn piccolo_flute_labels_keep_their_key() {
        let name = |text: &str| normalize_instrument_with_fallback(text, Some("treble"));
        assert_eq!(name("Piccolo/Flöte in Des").as_deref(), Some("Flöte Des"));
        assert_eq!(name("Piccolo/ Fl?te in Des").as_deref(), Some("Flöte Des"));
        assert_eq!(name("Piccolo/Flöte in C").as_deref(), Some("Flöte"));
        assert_eq!(name("Piccolo").as_deref(), Some("Flöte Piccolo"));
    }

    #[test]
    fn a_fuzzy_match_must_start_at_a_word() {
        let name = |text: &str| normalize_instrument_with_fallback(text, Some("treble"));
        assert_eq!(name("HorninF Weise").as_deref(), Some("Horn F"));
        assert_eq!(name("xinF Gute"), None);
    }

    #[test]
    fn instrument_pattern_must_start_the_candidate() {
        assert!(starts_with_instrument_pattern("Horn in F"));
        assert!(starts_with_instrument_pattern("Fl?te 2"));
        assert!(!starts_with_instrument_pattern("x:1. Horn in F"));
        assert!(!starts_with_instrument_pattern("Stück Klarinette in Eb"));
    }

    #[test]
    fn rejects_directions_and_clef_conflicts() {
        assert_eq!(normalize_instrument("molto rit.", Some("treble")), None);
        assert_eq!(normalize_instrument("Trompete in B", Some("bass")), None);
        assert_eq!(
            normalize_instrument_with_fallback("Flöte", Some("bass")).as_deref(),
            Some("Flöte")
        );
    }
}
