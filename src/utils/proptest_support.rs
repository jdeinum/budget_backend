use quickcheck::Testable;

/// Runs `prop` under quickcheck only when `PROP_TEST` is set. Property tests
/// explore a much larger input space than example-based tests and are
/// slower as a result (100 generated cases per property by default), so
/// they're opt-in rather than part of the default `cargo test` run — set
/// `PROP_TEST=1` to include them (e.g. in CI or before a release).
pub fn run(name: &str, prop: impl Testable) {
    if std::env::var("PROP_TEST").is_err() {
        eprintln!("PROP_TEST unset — skipping property test `{name}`");
        return;
    }
    quickcheck::QuickCheck::new().quickcheck(prop);
}
