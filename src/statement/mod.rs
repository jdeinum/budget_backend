mod amex;
mod neo;

use chrono::NaiveDate;

pub use amex::AmexImporter;
pub use neo::NeoImporter;

/// A transaction read out of a statement export, normalized to the same
/// sign convention Plaid uses: positive `amount` means money left the
/// account (a purchase), negative means money came in (a refund or
/// payment). Each importer is responsible for flipping its issuer's own
/// convention to match this before returning.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTransaction {
    pub date: NaiveDate,
    pub amount: f64,
    pub description: String,
    /// Who the transaction was with. Some exports (richer Amex ones) have
    /// their own distinct `Merchant` column; where they don't (Neo, and
    /// Amex's simpler export shape), the importer falls back to
    /// `description` itself rather than leaving this empty — there's
    /// nothing better to match a merchant on, but there's also nothing
    /// stopping a merchant match entirely.
    pub merchant: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("csv error: {0}")]
    Csv(#[from] csv::Error),
    #[error("row {row}: invalid date {value:?}: {source}")]
    InvalidDate {
        row: usize,
        value: String,
        #[source]
        source: chrono::ParseError,
    },
    #[error("row {row}: invalid amount {value:?}: {source}")]
    InvalidAmount {
        row: usize,
        value: String,
        #[source]
        source: std::num::ParseFloatError,
    },
}

/// One issuer's statement export format. Every bank/card export has its own
/// headers, date format, and amount sign convention, so there's one impl
/// per issuer rather than a single generic CSV parser — see `neo` and
/// `amex` for the two currently supported.
pub trait StatementImporter: Send + Sync {
    /// Whether this importer's format matches the given CSV header row.
    fn detect(&self, header: &csv::StringRecord) -> bool;

    fn parse(&self, bytes: &[u8]) -> Result<Vec<ParsedTransaction>, ImportError>;
}

/// Collapses runs of whitespace (statement descriptions are often
/// fixed-width padded, e.g. `"REAL CDN SUPERSTORE #1 CALGARY       CAN"`)
/// down to single spaces, so two extractions of the same row hash
/// identically and merchant matching isn't thrown off by stray padding.
fn normalize_description(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `header` contains every column in `required`, regardless of
/// order or extra columns alongside them. Issuers add columns to their
/// exports over time (loyalty fields, merchant address, foreign-currency
/// detail, ...) without warning, and serde's CSV deserialization already
/// matches struct fields by column name and ignores anything it doesn't
/// recognize — so `detect` only needs to confirm the columns each `Row`
/// actually reads are present, not that the header matches some exact
/// snapshot of it.
fn header_contains_all(header: &csv::StringRecord, required: &[&str]) -> bool {
    required
        .iter()
        .all(|&col| header.iter().any(|actual| actual == col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_description_collapses_padding() {
        assert_eq!(
            normalize_description("REAL CDN SUPERSTORE #1 CALGARY       CAN"),
            "REAL CDN SUPERSTORE #1 CALGARY CAN"
        );
    }

    /// Normalizing an already-normalized string is a no-op — running it
    /// twice can never move further away from a fixed point than running it
    /// once.
    // By-value is required: cast to `fn(String) -> bool` below, matching
    // `quickcheck::Testable`'s signature for a property fn.
    #[allow(clippy::needless_pass_by_value)]
    fn prop_normalize_description_is_idempotent(raw: String) -> bool {
        let once = normalize_description(&raw);
        let twice = normalize_description(&once);
        once == twice
    }

    #[test]
    fn normalize_description_is_idempotent() {
        crate::utils::proptest_support::run(
            "normalize_description_is_idempotent",
            prop_normalize_description_is_idempotent as fn(String) -> bool,
        );
    }

    /// Whatever whitespace `raw` used internally, the result never has a run
    /// of more than one space and never starts/ends with whitespace (unless
    /// it's empty) — the property behind "collapses padding" that the
    /// example test above only spot-checks one input for.
    // By-value is required — see the sibling property above.
    #[allow(clippy::needless_pass_by_value)]
    fn prop_normalize_description_has_no_stray_whitespace(raw: String) -> bool {
        let out = normalize_description(&raw);
        !out.contains("  ") && out == out.trim()
    }

    #[test]
    fn normalize_description_has_no_stray_whitespace() {
        crate::utils::proptest_support::run(
            "normalize_description_has_no_stray_whitespace",
            prop_normalize_description_has_no_stray_whitespace as fn(String) -> bool,
        );
    }
}
