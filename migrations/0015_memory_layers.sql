ALTER TABLE memories ADD COLUMN tier TEXT NOT NULL DEFAULT 'long_term' CHECK(tier IN ('episodic','long_term'));
ALTER TABLE memories ADD COLUMN evidence_type TEXT NOT NULL DEFAULT 'explicit_fact' CHECK(evidence_type IN ('user_quote','explicit_fact','inference'));
ALTER TABLE memory_sources ADD COLUMN evidence_type TEXT NOT NULL DEFAULT 'explicit_fact';
ALTER TABLE memory_sources ADD COLUMN confidence REAL NOT NULL DEFAULT 1;
ALTER TABLE memory_sources ADD COLUMN valid_until INTEGER;
CREATE INDEX memories_tier ON memories(tier,status,updated_at);
-- A bounded local decision trail, available only through authenticated APIs.
CREATE TABLE extraction_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL,
    message_id TEXT,
    candidate TEXT,
    decision TEXT NOT NULL,
    reason TEXT NOT NULL,
    memory_id TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX extraction_events_source ON extraction_events(message_id,seq);
DROP TRIGGER memory_revision_insert;
DROP TRIGGER memory_revision_update;
CREATE TRIGGER memory_revision_insert AFTER INSERT ON memories BEGIN
    INSERT INTO memory_revisions(memory_id,revision,recorded_at,snapshot)
    VALUES(NEW.id,NEW.revision,unixepoch(),json_object('id',NEW.id,'slot',NEW.slot,'content',NEW.content,
        'kind',NEW.kind,'importance',NEW.importance,'confidence',NEW.confidence,'entities',json(NEW.entities),
        'status',NEW.status,'superseded_by',NEW.superseded_by,'created_at',NEW.created_at,
        'updated_at',NEW.updated_at,'valid_until',NEW.valid_until,'tier',NEW.tier,'evidence_type',NEW.evidence_type,'origin',NEW.origin,'deleted_at',NEW.deleted_at));
END;
CREATE TRIGGER memory_revision_update AFTER UPDATE ON memories WHEN NEW.revision != OLD.revision BEGIN
    INSERT INTO memory_revisions(memory_id,revision,recorded_at,snapshot)
    VALUES(NEW.id,NEW.revision,unixepoch(),json_object('id',NEW.id,'slot',NEW.slot,'content',NEW.content,
        'kind',NEW.kind,'importance',NEW.importance,'confidence',NEW.confidence,'entities',json(NEW.entities),
        'status',NEW.status,'superseded_by',NEW.superseded_by,'created_at',NEW.created_at,
        'updated_at',NEW.updated_at,'valid_until',NEW.valid_until,'tier',NEW.tier,'evidence_type',NEW.evidence_type,'origin',NEW.origin,'deleted_at',NEW.deleted_at));
END;
CREATE TRIGGER extraction_events_bound AFTER INSERT ON extraction_events BEGIN
    DELETE FROM extraction_events WHERE seq <= NEW.seq - 1000;
END;
CREATE TABLE extraction_runs (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    source_count INTEGER NOT NULL,
    input_tokens INTEGER NOT NULL,
    candidate_count INTEGER NOT NULL DEFAULT 0,
    stored_count INTEGER NOT NULL DEFAULT 0,
    rejected_count INTEGER NOT NULL DEFAULT 0,
    started_at INTEGER NOT NULL DEFAULT (unixepoch()),
    completed_at INTEGER,
    UNIQUE(job_id,attempt)
);
CREATE TRIGGER extraction_runs_bound AFTER INSERT ON extraction_runs BEGIN
    DELETE FROM extraction_runs WHERE seq <= NEW.seq - 200;
END;
-- Counters outlive the bounded event trail, including very large/retried batches.
CREATE TRIGGER extraction_run_counts AFTER INSERT ON extraction_events BEGIN
    UPDATE extraction_runs SET
        candidate_count=candidate_count+(NEW.decision='candidate'),
        stored_count=stored_count+(NEW.decision='stored' AND NEW.reason!='already_observed'),
        rejected_count=rejected_count+(NEW.decision='rejected')
    WHERE job_id=NEW.job_id AND status='running';
END;
