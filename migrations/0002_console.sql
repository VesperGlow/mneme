ALTER TABLE memories ADD COLUMN origin TEXT NOT NULL DEFAULT 'extracted';
ALTER TABLE memories ADD COLUMN deleted_at INTEGER;
ALTER TABLE memories ADD COLUMN indexed_revision INTEGER NOT NULL DEFAULT 0;
CREATE TABLE content_barriers (content_hash TEXT PRIMARY KEY, through_seq INTEGER NOT NULL);
CREATE TABLE suppressed_sources (message_id TEXT PRIMARY KEY REFERENCES messages(id));
CREATE TABLE memory_revisions (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_id TEXT NOT NULL REFERENCES memories(id),
    revision INTEGER NOT NULL,
    recorded_at INTEGER NOT NULL,
    snapshot TEXT NOT NULL,
    UNIQUE(memory_id,revision)
);
INSERT INTO memory_revisions(memory_id,revision,recorded_at,snapshot)
SELECT id,revision,unixepoch(),json_object('id',id,'slot',slot,'content',content,'kind',kind,
    'importance',importance,'confidence',confidence,'entities',json(entities),'status',status,
    'superseded_by',superseded_by,'created_at',created_at,'updated_at',updated_at,
    'valid_until',valid_until,'origin',origin,'deleted_at',deleted_at)
FROM memories;
CREATE TRIGGER memory_revision_insert AFTER INSERT ON memories BEGIN
    INSERT INTO memory_revisions(memory_id,revision,recorded_at,snapshot)
    VALUES(NEW.id,NEW.revision,unixepoch(),json_object('id',NEW.id,'slot',NEW.slot,'content',NEW.content,
        'kind',NEW.kind,'importance',NEW.importance,'confidence',NEW.confidence,'entities',json(NEW.entities),
        'status',NEW.status,'superseded_by',NEW.superseded_by,'created_at',NEW.created_at,
        'updated_at',NEW.updated_at,'valid_until',NEW.valid_until,'origin',NEW.origin,'deleted_at',NEW.deleted_at));
END;
CREATE TRIGGER memory_revision_update AFTER UPDATE ON memories WHEN NEW.revision != OLD.revision BEGIN
    INSERT INTO memory_revisions(memory_id,revision,recorded_at,snapshot)
    VALUES(NEW.id,NEW.revision,unixepoch(),json_object('id',NEW.id,'slot',NEW.slot,'content',NEW.content,
        'kind',NEW.kind,'importance',NEW.importance,'confidence',NEW.confidence,'entities',json(NEW.entities),
        'status',NEW.status,'superseded_by',NEW.superseded_by,'created_at',NEW.created_at,
        'updated_at',NEW.updated_at,'valid_until',NEW.valid_until,'origin',NEW.origin,'deleted_at',NEW.deleted_at));
END;
CREATE INDEX memory_revisions_memory ON memory_revisions(memory_id,seq);
CREATE INDEX memories_console ON memories(deleted_at,status,kind,updated_at);
-- Existing rows also need an indexed_revision marker; replay their current projection once.
INSERT OR IGNORE INTO jobs(id,kind,payload,available_at,created_at)
SELECT 'project:' || id || ':console-migration:' || revision,'project',id,unixepoch(),unixepoch()
FROM memories;
