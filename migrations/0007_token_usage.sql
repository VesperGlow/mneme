-- One row per upstream request. Streaming usage updates replace the same row.
CREATE TABLE model_requests (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    turn_id TEXT REFERENCES turns(id),
    purpose TEXT NOT NULL CHECK(purpose IN ('chat','compression','memory')),
    model TEXT NOT NULL,
    context TEXT NOT NULL,
    usage TEXT NOT NULL DEFAULT '{}',
    reasoning TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'running',
    created_at INTEGER NOT NULL
);
CREATE INDEX model_requests_session ON model_requests(session_id,seq);
CREATE INDEX model_requests_turn ON model_requests(turn_id,seq);
