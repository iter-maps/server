//! Region/country-specific drivers (ADR 0017). When a surface needs genuinely
//! custom code — not just config — the generic gateway dispatches through a
//! trait to the implementation for the deployment's country, selected from the
//! resolved region (the first segment of the region path, e.g. `italy`). Adding
//! a country = a new `regions::<country>` module implementing the trait; the
//! generic core is untouched.

pub mod italy;

use std::sync::Arc;

/// Country-specific address normalization for the place-correlation bucket key
/// (street-type expansion, house-number/esponente rules, …). The bucket key is
/// the join: two records share an address iff their keys match (ADR 0012).
pub trait AddressNormalizer: Send + Sync {
    /// The correlation bucket for `(street, housenumber, city)`.
    fn bucket_key(&self, street: &str, housenumber: &str, city: &str) -> String;

    /// Split a freeform address ("Via X 12") into `(street, number)`. The
    /// default takes the trailing numeric token (number-after-street, as in most
    /// of Europe); override for number-first locales.
    fn split_freeform(&self, freeform: &str) -> (String, Option<String>) {
        default_split_freeform(freeform)
    }
}

/// Select the address normalizer for a region's country. Unknown countries get a
/// minimal generic normalizer (no country rules).
pub fn address_normalizer(country: &str) -> Arc<dyn AddressNormalizer> {
    match country {
        "italy" => Arc::new(italy::address::ItalyNormalizer),
        _ => Arc::new(GenericNormalizer),
    }
}

/// Fallback normalizer: lowercase + keep alphanumerics, no country-specific
/// street-type or house-number rules.
pub struct GenericNormalizer;

impl AddressNormalizer for GenericNormalizer {
    fn bucket_key(&self, street: &str, housenumber: &str, city: &str) -> String {
        format!(
            "{}|{}|{}",
            squash(city),
            squash(street),
            squash(housenumber)
        )
    }
}

fn squash(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Trailing-number freeform split (the shared default).
pub(crate) fn default_split_freeform(s: &str) -> (String, Option<String>) {
    let trimmed = s.trim().trim_end_matches([',', ' ']);
    if let Some(pos) = trimmed.rfind([' ', ',']) {
        let (head, tail) = trimmed.split_at(pos);
        let tail = tail.trim_start_matches([',', ' ']);
        if tail.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return (
                head.trim_end_matches([',', ' ']).to_string(),
                Some(tail.to_string()),
            );
        }
    }
    (trimmed.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_split_takes_trailing_number() {
        assert_eq!(
            default_split_freeform("Main Road 12"),
            ("Main Road".to_string(), Some("12".to_string()))
        );
        assert_eq!(
            default_split_freeform("Some Park"),
            ("Some Park".to_string(), None)
        );
    }

    #[test]
    fn generic_normalizer_buckets_case_insensitively() {
        let n = GenericNormalizer;
        assert_eq!(
            n.bucket_key("Main Rd", "12", "Town"),
            n.bucket_key("MAIN  RD", "12", "town")
        );
    }

    #[test]
    fn selector_picks_italy() {
        // italy gets the DUG-aware normalizer (V.le == Viale); generic does not.
        let it = address_normalizer("italy");
        let generic = address_normalizer("narnia");
        assert_eq!(
            it.bucket_key("V.le Roma", "1", "X"),
            it.bucket_key("Viale Roma", "1", "X")
        );
        assert_ne!(
            generic.bucket_key("V.le Roma", "1", "X"),
            generic.bucket_key("Viale Roma", "1", "X")
        );
    }
}
