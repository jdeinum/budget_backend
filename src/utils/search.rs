/// Sanitizes free-text user input into a safe FTS5 `MATCH` query: each
/// whitespace-separated token becomes a quoted phrase (with embedded quotes
/// doubled, FTS5's own escaping rule), joined with FTS5's implicit AND —
/// this keeps special characters (`"`, `*`, `:`, `-`, `(`, `)`, column
/// filters) from being interpreted as FTS5 query syntax.
pub fn fts5_query(input: &str) -> String {
    input
        .split_whitespace()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}
