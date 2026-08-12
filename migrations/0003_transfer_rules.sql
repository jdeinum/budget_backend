-- SQLite can't ALTER a CHECK constraint in place, so this rebuilds the
-- (small) transaction_rules table with 'transfer' added to the allowed
-- kinds, plus two new transfer-only columns.
CREATE TABLE transaction_rules_new (
    id                 TEXT PRIMARY KEY,
    kind               TEXT NOT NULL CHECK (kind IN ('merchant_contains', 'tag', 'account', 'transfer')),
    pattern            TEXT,
    tag_name           TEXT,
    tag_value          TEXT,
    account_id         TEXT REFERENCES accounts (id) ON DELETE CASCADE,
    source_account_id  TEXT REFERENCES accounts (id) ON DELETE CASCADE,
    target_account_id  TEXT REFERENCES accounts (id) ON DELETE CASCADE,
    created_at         TEXT NOT NULL
);

INSERT INTO transaction_rules_new (id, kind, pattern, tag_name, tag_value, account_id, created_at)
SELECT id, kind, pattern, tag_name, tag_value, account_id, created_at FROM transaction_rules;

DROP TABLE transaction_rules;
ALTER TABLE transaction_rules_new RENAME TO transaction_rules;
