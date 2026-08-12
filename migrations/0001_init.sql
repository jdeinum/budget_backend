CREATE TABLE items (
    id              TEXT PRIMARY KEY,
    plaid_item_id   TEXT NOT NULL UNIQUE,
    access_token    TEXT NOT NULL,
    institution_id  TEXT,
    cursor          TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

-- One row per Plaid account_id, auto-created the first time a transaction
-- references it (mirrors how merchants are auto-created). `id` is Plaid's
-- own account_id, not a generated one — there's no separate identity to
-- dedupe here. `name` defaults to the raw account_id (Plaid's transaction
-- payload carries no friendly account name) and is never overwritten by a
-- re-sync, so a user's rename sticks.
CREATE TABLE accounts (
    id          TEXT PRIMARY KEY,
    item_id     TEXT NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE merchants (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    entity_id   TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

-- Plaid's merchant_entity_id is a stable identifier for a merchant, unlike
-- the display name (which varies in formatting across transactions, e.g.
-- "UBER *TRIP" vs "Uber"). Key merchants on it when Plaid supplies one...
CREATE UNIQUE INDEX idx_merchants_entity_id
    ON merchants (entity_id)
    WHERE entity_id IS NOT NULL;

-- ...otherwise fall back to deduping by name.
CREATE UNIQUE INDEX idx_merchants_name_no_entity
    ON merchants (name)
    WHERE entity_id IS NULL;

-- History of a merchant's category over time (a merchant's Plaid category
-- can change as Plaid re-classifies it). Exactly one row per merchant has
-- effective_to IS NULL: the current category.
CREATE TABLE merchant_categories (
    id                 TEXT PRIMARY KEY,
    merchant_id        TEXT NOT NULL REFERENCES merchants(id) ON DELETE CASCADE,
    category_primary   TEXT,
    category_detailed  TEXT,
    effective_from     TEXT NOT NULL,
    effective_to       TEXT,
    created_at         TEXT NOT NULL
);

CREATE INDEX idx_merchant_categories_merchant_id ON merchant_categories (merchant_id);

-- Enforces at most one open (current) category period per merchant.
CREATE UNIQUE INDEX idx_merchant_categories_current
    ON merchant_categories (merchant_id)
    WHERE effective_to IS NULL;

CREATE TABLE transactions (
    id                        TEXT PRIMARY KEY,
    item_id                   TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    plaid_transaction_id      TEXT NOT NULL UNIQUE,
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
    created_at                TEXT NOT NULL,
    updated_at                TEXT NOT NULL
);

CREATE INDEX idx_transactions_item_id ON transactions (item_id);
CREATE INDEX idx_transactions_date ON transactions (date);
CREATE INDEX idx_transactions_merchant_id ON transactions (merchant_id);

-- Generic key/value tagging. Each distinct (name, value) pair is one row —
-- e.g. one "category"/"FOOD_AND_DRINK" tag is shared by every transaction in
-- that category, rather than each attachment carrying its own copy of the
-- value. The join tables are pure many-to-many.
CREATE TABLE tags (
    id     TEXT PRIMARY KEY,
    name   TEXT NOT NULL,
    value  TEXT NOT NULL,
    UNIQUE (name, value)
);

CREATE TABLE transaction_tags (
    transaction_id  TEXT NOT NULL REFERENCES transactions (id) ON DELETE CASCADE,
    tag_id          TEXT NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    PRIMARY KEY (transaction_id, tag_id)
);

CREATE INDEX idx_transaction_tags_tag_id ON transaction_tags (tag_id);

CREATE TABLE merchant_tags (
    merchant_id  TEXT NOT NULL REFERENCES merchants (id) ON DELETE CASCADE,
    tag_id       TEXT NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    PRIMARY KEY (merchant_id, tag_id)
);

CREATE INDEX idx_merchant_tags_tag_id ON merchant_tags (tag_id);

CREATE TABLE account_tags (
    account_id  TEXT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    tag_id      TEXT NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    PRIMARY KEY (account_id, tag_id)
);

CREATE INDEX idx_account_tags_tag_id ON account_tags (tag_id);

-- Full-text search over transactions' free-text fields. External-content
-- table keyed on the base table's implicit rowid (transactions.id is a TEXT
-- primary key, so it does not alias rowid the way an INTEGER PRIMARY KEY
-- would) — kept in sync via triggers rather than sqlx query-time writes.
CREATE VIRTUAL TABLE transactions_fts USING fts5(
    name, merchant_name, content='transactions', content_rowid='rowid'
);

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

-- Full-text search over merchant display names.
CREATE VIRTUAL TABLE merchants_fts USING fts5(
    name, content='merchants', content_rowid='rowid'
);

CREATE TRIGGER merchants_fts_ai AFTER INSERT ON merchants BEGIN
    INSERT INTO merchants_fts(rowid, name) VALUES (new.rowid, new.name);
END;

CREATE TRIGGER merchants_fts_ad AFTER DELETE ON merchants BEGIN
    INSERT INTO merchants_fts(merchants_fts, rowid, name) VALUES ('delete', old.rowid, old.name);
END;

CREATE TRIGGER merchants_fts_au AFTER UPDATE ON merchants BEGIN
    INSERT INTO merchants_fts(merchants_fts, rowid, name) VALUES ('delete', old.rowid, old.name);
    INSERT INTO merchants_fts(rowid, name) VALUES (new.rowid, new.name);
END;
