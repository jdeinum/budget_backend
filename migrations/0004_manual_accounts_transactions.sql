-- no-transaction
--
-- Adds support for accounts/transactions that don't come from a Plaid item
-- (manual accounts + statement CSV imports — see `src/statement/`). Both
-- `accounts.item_id` and `transactions.item_id` go from NOT NULL to
-- nullable, which SQLite can't do with a plain ALTER TABLE, so both tables
-- are rebuilt. That requires PRAGMA foreign_keys=OFF for the duration (a
-- DROP TABLE with foreign keys enabled performs an implicit, cascading
-- DELETE FROM first — see SQLite's foreign key docs, section 7 — which
-- would wipe `transaction_tags`/`account_tags`/`transaction_rules` rows
-- instead of leaving them to repoint at the rebuilt table). Toggling that
-- pragma is a no-op inside a transaction, hence `-- no-transaction` above
-- and an explicit BEGIN/COMMIT bracketing just the DDL below.
PRAGMA foreign_keys = OFF;

BEGIN;

CREATE TABLE accounts_new (
    id          TEXT PRIMARY KEY,
    item_id     TEXT REFERENCES items (id) ON DELETE CASCADE,
    source      TEXT NOT NULL DEFAULT 'plaid',
    name        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

INSERT INTO accounts_new (id, item_id, source, name, created_at, updated_at)
SELECT id, item_id, 'plaid', name, created_at, updated_at FROM accounts;

DROP TABLE accounts;
ALTER TABLE accounts_new RENAME TO accounts;

-- `transactions.id` is a TEXT primary key, not a rowid alias, so
-- `transactions_fts` (content='transactions', content_rowid='rowid')
-- indexes rows by SQLite's own implicit rowid rather than `id`. Rebuilding
-- the table would otherwise reassign fresh sequential rowids on INSERT,
-- desyncing the FTS index from its content table — so rowid is carried
-- over explicitly instead.
CREATE TABLE transactions_new (
    id                        TEXT PRIMARY KEY,
    item_id                   TEXT REFERENCES items (id) ON DELETE CASCADE,
    -- No CHECK on the allowed values here (unlike transaction_rules.kind) —
    -- new statement importers are expected to add new sources over time,
    -- and Source's own sqlx::Type decode already rejects anything it
    -- doesn't recognize.
    source                    TEXT NOT NULL DEFAULT 'plaid',
    plaid_transaction_id      TEXT,
    -- Manually-imported transactions have no stable external id to key on,
    -- so duplicates (re-uploading the same statement, or an overlapping
    -- date range from a later one) are instead detected by natural key —
    -- see idx_transactions_import_dedup below and
    -- repo::transaction::import_transactions. `occurrence` distinguishes
    -- genuinely repeated same-day/same-amount/same-description charges
    -- (e.g. two identical coffees) from a re-import of the same row: it's
    -- the row's position among duplicates *within its source file*, so
    -- re-parsing that file reproduces the same occurrence numbers and
    -- collides on conflict, while a new, additional same-day duplicate
    -- gets a new one.
    occurrence                INTEGER NOT NULL DEFAULT 0,
    account_id                TEXT NOT NULL REFERENCES accounts (id),
    amount                    REAL NOT NULL,
    iso_currency_code         TEXT,
    unofficial_currency_code  TEXT,
    date                      TEXT NOT NULL,
    datetime                  TEXT,
    name                      TEXT,
    merchant_name             TEXT,
    pending                   BOOLEAN NOT NULL DEFAULT FALSE,
    payment_channel           TEXT,
    merchant_id               TEXT REFERENCES merchants (id),
    ignored                   INTEGER NOT NULL DEFAULT 0,
    created_at                TEXT NOT NULL,
    updated_at                TEXT NOT NULL
);

INSERT INTO transactions_new (
    rowid, id, item_id, source, plaid_transaction_id, account_id, amount,
    iso_currency_code, unofficial_currency_code, date, datetime, name,
    merchant_name, pending, payment_channel, merchant_id, ignored,
    created_at, updated_at
)
SELECT
    rowid, id, item_id, 'plaid', plaid_transaction_id, account_id, amount,
    iso_currency_code, unofficial_currency_code, date, datetime, name,
    merchant_name, pending, payment_channel, merchant_id, ignored,
    created_at, updated_at
FROM transactions;

DROP TABLE transactions;
ALTER TABLE transactions_new RENAME TO transactions;

-- DROP TABLE auto-drops indexes/triggers owned by the table; recreate them.
CREATE INDEX idx_transactions_item_id ON transactions (item_id);
CREATE INDEX idx_transactions_date ON transactions (date);
CREATE INDEX idx_transactions_merchant_id ON transactions (merchant_id);
CREATE INDEX idx_transactions_ignored ON transactions (ignored);

CREATE UNIQUE INDEX idx_transactions_plaid_transaction_id
    ON transactions (plaid_transaction_id)
    WHERE plaid_transaction_id IS NOT NULL;

CREATE UNIQUE INDEX idx_transactions_import_dedup
    ON transactions (account_id, date, amount, name, occurrence)
    WHERE source != 'plaid';

CREATE TRIGGER transactions_fts_ai AFTER INSERT ON transactions BEGIN
    INSERT INTO transactions_fts(rowid, name, merchant_name)
    VALUES (new.rowid, new.name, new.merchant_name);
END;

CREATE TRIGGER transactions_fts_ad AFTER DELETE ON transactions BEGIN
    INSERT INTO transactions_fts(transactions_fts, rowid, name, merchant_name)
    VALUES ('delete', old.rowid, old.name, old.merchant_name);
END;

CREATE TRIGGER transactions_fts_au AFTER UPDATE ON transactions BEGIN
    INSERT INTO transactions_fts(transactions_fts, rowid, name, merchant_name)
    VALUES ('delete', old.rowid, old.name, old.merchant_name);
    INSERT INTO transactions_fts(rowid, name, merchant_name)
    VALUES (new.rowid, new.name, new.merchant_name);
END;

COMMIT;

PRAGMA foreign_keys = ON;
