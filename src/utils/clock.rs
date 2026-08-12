use chrono::{DateTime, Utc};

/// Where "now" comes from. Kept as a single trait object on `AppState`
/// rather than threaded as a dependency through every function — call sites
/// just take the `DateTime<Utc>` it produces, so nothing outside the route
/// layer and `sync.rs` needs to know a `Clock` exists.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
