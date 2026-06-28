//! Italian address normalization — the join key for correlating places at the
//! same civico, behind the generic [`AddressNormalizer`] trait. The key must
//! survive the ways Italian addresses vary: street-type abbreviations
//! (`V.le` = `Viale`), accents, esponenti (`12/A` = `12a`, `12 bis`), the `snc`
//! ("no number") sentinel, and the Firenze/Genova red-vs-black civici — where
//! `Via X 1` and `Via X 1 rosso` are *different* buildings, so the colour must
//! be part of the key.
//!
//! libpostal would be the heavyweight alternative (~1.8 GB model); for Italian
//! streets a focused rule layer is enough and ships in-binary.

use crate::traits::AddressNormalizer;

/// The Italy address-correlation driver. Freeform splitting uses the generic
/// number-after-street default; bucketing applies the Italian rules below.
pub struct ItalyNormalizer;

impl AddressNormalizer for ItalyNormalizer {
    fn bucket_key(&self, street: &str, housenumber: &str, city: &str) -> String {
        bucket_key(street, housenumber, city)
    }
}

/// Canonical Italian street types (DUG), keyed by the de-dotted abbreviation or
/// full word. Bare `v` is ambiguous (via/viale) — we resolve it to the far more
/// common `via`.
fn canonical_dug(token: &str) -> Option<&'static str> {
    Some(match token {
        "via" | "v" => "via",
        "viale" | "vle" | "vl" => "viale",
        "piazza" | "pza" | "pzza" | "pa" | "p" => "piazza",
        "piazzale" | "pzle" | "ple" => "piazzale",
        "corso" | "cso" | "c" => "corso",
        "largo" | "lgo" => "largo",
        "vicolo" | "vlo" => "vicolo",
        "strada" | "str" => "strada",
        "borgo" | "bgo" => "borgo",
        "lungotevere" | "lgt" | "lungotev" => "lungotevere",
        "salita" => "salita",
        "galleria" | "gall" => "galleria",
        "circonvallazione" | "circ" => "circonvallazione",
        _ => return None,
    })
}

/// Normalize an Italian street name to a stable join token: fold accents, drop
/// punctuation, expand the leading street-type, lowercase, collapse whitespace.
fn normalize_street(s: &str) -> String {
    let cleaned = clean(s);
    let mut tokens = cleaned.split_whitespace();
    let Some(first) = tokens.next() else {
        return String::new();
    };
    let head = canonical_dug(first).unwrap_or(first);
    let rest: Vec<&str> = tokens.collect();
    if rest.is_empty() {
        head.to_string()
    } else {
        format!("{head} {}", rest.join(" "))
    }
}

/// Normalize a house number to `(number, is_red)`. The colour is lifted OUT of
/// the string into the flag so `1` (black) and `1 rosso` (red) never collide.
/// `snc` (senza numero civico) becomes a sentinel that should weaken, not key, a
/// match.
fn normalize_housenumber(s: &str) -> (String, bool) {
    let lower = clean(s);
    if lower.is_empty() || lower == "snc" {
        return ("__snc__".to_string(), false);
    }

    // Colour: the whole word `rosso`, or a separated trailing `r` (`12/r`,
    // `12 r`). A bare suffix `r` with no separator (`12r`) stays an esponente —
    // the Rome convention — so we don't misread it as red.
    let mut body = lower.clone();
    let mut red = false;
    if let Some(stripped) = body
        .strip_suffix(" rosso")
        .or_else(|| body.strip_suffix("rosso"))
    {
        red = true;
        body = stripped.trim().to_string();
    } else if body.ends_with(" r") || body.ends_with("/r") || body.ends_with("-r") {
        red = true;
        body = body[..body.len() - 2].to_string();
    }

    // Drop separators so `12/A` == `12 a` == `12a`, and spaces in `12 bis`.
    let number: String = body
        .chars()
        .filter(|c| !matches!(c, ' ' | '/' | '-' | '.'))
        .collect();
    (number, red)
}

/// The correlation bucket: `comune | street | number(+colour)`. Two records
/// share an address iff their keys are equal.
fn bucket_key(street: &str, housenumber: &str, city: &str) -> String {
    let (number, red) = normalize_housenumber(housenumber);
    format!(
        "{}|{}|{}{}",
        clean(city),
        normalize_street(street),
        number,
        if red { "|rosso" } else { "" }
    )
}

/// Lowercase, fold Italian accents, collapse punctuation. Dots are *removed*
/// (so `V.le` → `vle` → `viale`); other punctuation becomes a space.
fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let folded = match ch.to_ascii_lowercase() {
            'à' | 'á' | 'â' | 'ä' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        };
        match folded {
            '.' => {} // drop, so `v.le` collapses to `vle`
            c if c.is_alphanumeric() => out.push(c),
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_street_type_abbreviations() {
        assert_eq!(normalize_street("V.le Trastevere"), "viale trastevere");
        assert_eq!(normalize_street("Via Tripoli"), "via tripoli");
        assert_eq!(normalize_street("P.za di Spagna"), "piazza di spagna");
        assert_eq!(normalize_street("C.so Vittorio"), "corso vittorio");
        assert_eq!(normalize_street("L.go Argentina"), "largo argentina");
        // bare `V` resolves to via (the common case).
        assert_eq!(normalize_street("V XX Settembre"), "via xx settembre");
    }

    #[test]
    fn folds_accents_and_punctuation() {
        assert_eq!(
            normalize_street("Via Niccolò Machiavelli"),
            "via niccolo machiavelli"
        );
        // apostrophes and dots collapse to spaces consistently.
        assert_eq!(
            normalize_street("Via Sant'Angelo"),
            normalize_street("via sant angelo")
        );
    }

    #[test]
    fn equivalent_streets_share_a_key() {
        assert_eq!(
            normalize_street("V.le del Colosseo"),
            normalize_street("Viale del Colosseo")
        );
    }

    #[test]
    fn normalizes_housenumber_esponenti() {
        assert_eq!(normalize_housenumber("12"), ("12".to_string(), false));
        assert_eq!(normalize_housenumber("12/A"), ("12a".to_string(), false));
        assert_eq!(
            normalize_housenumber("12 bis"),
            ("12bis".to_string(), false)
        );
        assert_eq!(normalize_housenumber("12 A"), ("12a".to_string(), false));
    }

    #[test]
    fn red_civici_never_collide_with_black() {
        let (n_black, red_black) = normalize_housenumber("1");
        let (n_red, red_red) = normalize_housenumber("1 rosso");
        assert!(!red_black);
        assert!(red_red);
        assert_eq!(n_black, n_red); // same number...
        // ...but the bucket differs because the colour flag is in the key.
        assert_ne!(
            bucket_key("Via X", "1", "Firenze"),
            bucket_key("Via X", "1 rosso", "Firenze")
        );
        // `12/r` is red; bare `12r` is an esponente (Rome), not red.
        assert!(normalize_housenumber("12/r").1);
        assert!(!normalize_housenumber("12r").1);
    }

    #[test]
    fn snc_is_a_sentinel_not_a_number() {
        assert_eq!(normalize_housenumber("snc"), ("__snc__".to_string(), false));
        assert_eq!(normalize_housenumber(""), ("__snc__".to_string(), false));
    }

    #[test]
    fn bucket_key_joins_comune_street_number() {
        // "V.le Trastevere 12/A, Roma" and "Viale Trastevere 12 A, roma" agree.
        assert_eq!(
            bucket_key("V.le Trastevere", "12/A", "Roma"),
            bucket_key("Viale Trastevere", "12 A", "roma")
        );
    }

    #[test]
    fn splits_freeform_addresses() {
        // The driver inherits the generic number-after-street split.
        assert_eq!(
            ItalyNormalizer.split_freeform("Via Tripoli 20"),
            ("Via Tripoli".to_string(), Some("20".to_string()))
        );
        assert_eq!(
            ItalyNormalizer.split_freeform("Piazza di Spagna, 31"),
            ("Piazza di Spagna".to_string(), Some("31".to_string()))
        );
        assert_eq!(
            ItalyNormalizer.split_freeform("Largo di Torre Argentina"),
            ("Largo di Torre Argentina".to_string(), None)
        );
        // a freeform with a number splits to the same bucket as its parts.
        let (st, hn) = ItalyNormalizer.split_freeform("Via Cavour 1");
        assert_eq!(
            bucket_key(&st, hn.as_deref().unwrap(), "Roma"),
            bucket_key("Via Cavour", "1", "Roma")
        );
    }
}
