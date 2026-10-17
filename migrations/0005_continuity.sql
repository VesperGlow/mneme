CREATE TABLE conversation_context (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id),
    summary TEXT NOT NULL DEFAULT '',
    through_seq INTEGER NOT NULL DEFAULT 0,
    compactions INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER
);
CREATE TABLE console_sessions (
    token_hash TEXT PRIMARY KEY,
    expires_at INTEGER NOT NULL
);
CREATE INDEX console_sessions_expiry ON console_sessions(expires_at);
CREATE TABLE extracted_messages (
    message_id TEXT PRIMARY KEY REFERENCES messages(id)
);
CREATE TABLE conversation_aliases (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id),
    conversation_id TEXT NOT NULL REFERENCES sessions(id)
);
