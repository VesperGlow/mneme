-- QQ delivery state is part of the SQLite source of truth, including crash recovery.
ALTER TABLE forget_barriers ADD COLUMN through_qq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE content_barriers ADD COLUMN through_qq INTEGER NOT NULL DEFAULT 0;
CREATE TABLE qq_sessions (
    app_id TEXT NOT NULL,
    user_openid TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    PRIMARY KEY(app_id, user_openid)
);
CREATE TABLE qq_inbox (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    app_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    user_openid TEXT NOT NULL,
    content TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    reply TEXT,
    next_chunk INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','running','sent','dead')),
    attempts INTEGER NOT NULL DEFAULT 0,
    available_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    last_error TEXT,
    UNIQUE(app_id,message_id)
);
CREATE INDEX qq_inbox_due ON qq_inbox(app_id,status,created_at);
