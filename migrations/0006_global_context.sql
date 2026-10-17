-- Preserve the transport of historic turns before merging their conversation IDs.
ALTER TABLE turns ADD COLUMN channel TEXT NOT NULL DEFAULT 'api' CHECK(channel IN ('api','qq'));
UPDATE turns SET channel='qq' WHERE session_id IN (
    SELECT session_id FROM qq_sessions UNION SELECT session_id FROM qq_inbox
);
CREATE INDEX messages_turn ON messages(turn_id,seq);
