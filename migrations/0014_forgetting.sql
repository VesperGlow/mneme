-- Short-lived, immutable previews. Confirmation consumes a plan in the same
-- transaction as deletion; retries replay the result without deleting again.
CREATE TABLE forget_requests (
    id TEXT PRIMARY KEY,
    candidates TEXT NOT NULL,
    through_seq INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    result TEXT
);
CREATE INDEX forget_requests_expiry ON forget_requests(expires_at);
