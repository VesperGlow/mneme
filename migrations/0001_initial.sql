CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    active_turn TEXT
);
CREATE TABLE turns (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    status TEXT NOT NULL CHECK(status IN ('generating','complete','failed')),
    error_code TEXT,
    created_at INTEGER NOT NULL,
    completed_at INTEGER
);
CREATE TABLE messages (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    turn_id TEXT NOT NULL REFERENCES turns(id),
    role TEXT NOT NULL CHECK(role IN ('user','assistant')),
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX messages_session ON messages(session_id, seq);
CREATE TABLE memories (
    id TEXT PRIMARY KEY,
    slot TEXT NOT NULL,
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    kind TEXT NOT NULL,
    importance REAL NOT NULL CHECK(importance BETWEEN 0 AND 1),
    confidence REAL NOT NULL CHECK(confidence BETWEEN 0 AND 1),
    entities TEXT NOT NULL,
    embedding TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('active','superseded','forgotten')),
    superseded_by TEXT,
    revision INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    source_seq INTEGER NOT NULL,
    last_accessed_at INTEGER,
    access_count INTEGER NOT NULL DEFAULT 0,
    valid_until INTEGER
);
CREATE UNIQUE INDEX memories_active_slot ON memories(slot) WHERE status='active';
CREATE UNIQUE INDEX memories_active_hash ON memories(content_hash) WHERE status='active';
CREATE INDEX memories_status ON memories(status, updated_at);
CREATE TABLE memory_sources (
    memory_id TEXT NOT NULL REFERENCES memories(id),
    message_id TEXT NOT NULL REFERENCES messages(id),
    quote TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    PRIMARY KEY(memory_id, message_id)
);
CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('extract','project')),
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','running','dead')),
    attempts INTEGER NOT NULL DEFAULT 0,
    available_at INTEGER NOT NULL,
    last_error TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX jobs_due ON jobs(status, available_at);
CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- Prevent an extraction already in flight from resurrecting manually forgotten slots.
CREATE TABLE forget_barriers (slot TEXT PRIMARY KEY, through_seq INTEGER NOT NULL);
