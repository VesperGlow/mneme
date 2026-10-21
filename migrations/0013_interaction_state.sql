-- Ephemeral interpretations of this conversation, never facts or persona rules.
CREATE TABLE interaction_state (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    persona_hash TEXT NOT NULL,
    turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
    payload TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
