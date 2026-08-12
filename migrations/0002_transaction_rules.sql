CREATE TABLE transaction_rules (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL CHECK (kind IN ('merchant_contains', 'tag', 'account')),
    pattern     TEXT,
    tag_name    TEXT,
    tag_value   TEXT,
    account_id  TEXT REFERENCES accounts (id) ON DELETE CASCADE,
    created_at  TEXT NOT NULL
);

ALTER TABLE transactions ADD COLUMN ignored INTEGER NOT NULL DEFAULT 0;
CREATE INDEX idx_transactions_ignored ON transactions (ignored);
