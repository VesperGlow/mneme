-- Reuse the request ledger for explicit persona generation and previews.
CREATE TABLE model_requests_v4 (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    turn_id TEXT REFERENCES turns(id),
    purpose TEXT NOT NULL CHECK(purpose IN ('chat','compression','memory','embedding','reranker','sticker','persona_compile','persona_preview','persona_reflect','persona_validate')),
    model TEXT NOT NULL,
    context TEXT NOT NULL,
    usage TEXT NOT NULL DEFAULT '{}',
    reasoning TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'running',
    created_at INTEGER NOT NULL
);
INSERT INTO model_requests_v4 SELECT * FROM model_requests;
DROP TABLE model_requests;
ALTER TABLE model_requests_v4 RENAME TO model_requests;
CREATE INDEX model_requests_session ON model_requests(session_id,seq);
CREATE INDEX model_requests_turn ON model_requests(turn_id,seq);

-- Behavioral evidence and revisions never enter factual memories or Neo4j.
CREATE TABLE evolution_feedback (
    source_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','consumed')),
    created_at INTEGER NOT NULL
);
CREATE TABLE evolution_revisions (
    version INTEGER PRIMARY KEY,
    record TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
