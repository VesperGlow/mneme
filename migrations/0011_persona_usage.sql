-- Reuse the request ledger for explicit persona generation and previews.
CREATE TABLE model_requests_v3 (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    turn_id TEXT REFERENCES turns(id),
    purpose TEXT NOT NULL CHECK(purpose IN ('chat','compression','memory','embedding','reranker','sticker','persona_compile','persona_preview')),
    model TEXT NOT NULL,
    context TEXT NOT NULL,
    usage TEXT NOT NULL DEFAULT '{}',
    reasoning TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'running',
    created_at INTEGER NOT NULL
);
INSERT INTO model_requests_v3 SELECT * FROM model_requests;
DROP TABLE model_requests;
ALTER TABLE model_requests_v3 RENAME TO model_requests;
CREATE INDEX model_requests_session ON model_requests(session_id,seq);
CREATE INDEX model_requests_turn ON model_requests(turn_id,seq);
