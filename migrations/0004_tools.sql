CREATE TABLE assets (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    mime TEXT NOT NULL,
    data BLOB NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('upload','screenshot')),
    created_at INTEGER NOT NULL
);
CREATE INDEX assets_session ON assets(session_id,created_at);
CREATE TABLE message_assets (
    message_id TEXT NOT NULL REFERENCES messages(id),
    asset_id TEXT NOT NULL REFERENCES assets(id),
    position INTEGER NOT NULL,
    PRIMARY KEY(message_id,asset_id)
);
CREATE TABLE tool_calls (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    turn_id TEXT NOT NULL REFERENCES turns(id),
    call_id TEXT NOT NULL,
    name TEXT NOT NULL,
    action TEXT NOT NULL,
    arguments TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','completed','failed')),
    result TEXT,
    error TEXT,
    elapsed_ms INTEGER,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    UNIQUE(turn_id,call_id)
);
CREATE INDEX tool_calls_recent ON tool_calls(created_at DESC);
CREATE INDEX tool_calls_session ON tool_calls(session_id,created_at);
