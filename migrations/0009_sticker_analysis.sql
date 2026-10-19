ALTER TABLE stickers ADD COLUMN analysis_status TEXT NOT NULL DEFAULT 'pending'
    CHECK(analysis_status IN ('pending','complete','failed','manual'));
ALTER TABLE stickers ADD COLUMN analysis_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE stickers ADD COLUMN analysis_error TEXT;

-- Keep request usage for visual indexing separate from chat and memory extraction.
CREATE TABLE model_requests_new (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    turn_id TEXT REFERENCES turns(id),
    purpose TEXT NOT NULL CHECK(purpose IN ('chat','compression','memory','sticker')),
    model TEXT NOT NULL,
    context TEXT NOT NULL,
    usage TEXT NOT NULL DEFAULT '{}',
    reasoning TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'running',
    created_at INTEGER NOT NULL
);
INSERT INTO model_requests_new SELECT * FROM model_requests;
DROP TABLE model_requests;
ALTER TABLE model_requests_new RENAME TO model_requests;
CREATE INDEX model_requests_session ON model_requests(session_id,seq);
CREATE INDEX model_requests_turn ON model_requests(turn_id,seq);
