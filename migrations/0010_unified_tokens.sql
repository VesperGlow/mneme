-- Retain every existing request and extend the same ledger to CPU model RPCs.
CREATE TABLE model_requests_v2 (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    turn_id TEXT REFERENCES turns(id),
    purpose TEXT NOT NULL CHECK(purpose IN ('chat','compression','memory','embedding','reranker','sticker')),
    model TEXT NOT NULL,
    context TEXT NOT NULL,
    usage TEXT NOT NULL DEFAULT '{}',
    reasoning TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'running',
    created_at INTEGER NOT NULL
);
INSERT INTO model_requests_v2 SELECT * FROM model_requests;
DROP TABLE model_requests;
ALTER TABLE model_requests_v2 RENAME TO model_requests;
CREATE INDEX model_requests_session ON model_requests(session_id,seq);
CREATE INDEX model_requests_turn ON model_requests(turn_id,seq);

-- Remember attribute identities established by exact content deduplication.
CREATE TABLE memory_slot_aliases (
    alias TEXT PRIMARY KEY,
    slot TEXT NOT NULL CHECK(alias <> slot)
);
CREATE INDEX memory_slot_aliases_target ON memory_slot_aliases(slot);
