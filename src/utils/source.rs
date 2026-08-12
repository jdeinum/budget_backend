use serde::{Deserialize, Serialize};

/// Where an account or transaction's data originated:
/// - `Plaid` — a live sync.
/// - `Neo`/`Amex` — imported from that issuer's statement export (see
///   `statement`). An account's own `source` is one of these, chosen when
///   it's created, and picks which importer its uploads go through — see
///   `routes::accounts::importer_for`.
/// - `Manual` — a single transaction entered by hand rather than synced or
///   imported. Only ever appears on a transaction, never on an account: an
///   account always has some real institution behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Plaid,
    Neo,
    Amex,
    Manual,
}

impl Source {
    /// Human-readable name, for error messages and UI labels.
    pub fn label(self) -> &'static str {
        match self {
            Source::Plaid => "Plaid",
            Source::Neo => "Neo",
            Source::Amex => "Amex",
            Source::Manual => "Manual",
        }
    }
}
