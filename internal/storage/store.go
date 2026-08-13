package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
	"unicode"

	_ "modernc.org/sqlite"
)

type Store struct {
	db *sql.DB
}

type Message struct {
	ID         int64     `json:"id"`
	Role       string    `json:"role"`
	Content    string    `json:"content"`
	Channel    string    `json:"channel,omitempty"`
	ExternalID string    `json:"external_id,omitempty"`
	CreatedAt  time.Time `json:"created_at"`
}

type Memory struct {
	ID              int64     `json:"id"`
	Content         string    `json:"content"`
	Source          string    `json:"source"`
	Status          string    `json:"status"`
	Kind            string    `json:"kind"`
	Importance      int       `json:"importance"`
	Confidence      float64   `json:"confidence"`
	LastConfirmedAt time.Time `json:"last_confirmed_at"`
	SourceMessageID string    `json:"source_message_id,omitempty"`
	Pinned          bool      `json:"pinned"`
	SupersedesID    int64     `json:"supersedes_id,omitempty"`
	CreatedAt       time.Time `json:"created_at"`
	UpdatedAt       time.Time `json:"updated_at"`
}

type Summary struct {
	ID               int64     `json:"id"`
	Content          string    `json:"content"`
	ThroughMessageID int64     `json:"through_message_id"`
	CreatedAt        time.Time `json:"created_at"`
}

type Export struct {
	ExportedAt time.Time `json:"exported_at"`
	Messages   []Message `json:"messages"`
	Memories   []Memory  `json:"memories"`
	Summaries  []Summary `json:"summaries"`
}

func Open(path string) (*Store, error) {
	if dir := filepath.Dir(path); dir != "." {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return nil, fmt.Errorf("create database directory: %w", err)
		}
	}
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("open database: %w", err)
	}
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	store := &Store{db: db}
	if err := store.migrate(context.Background()); err != nil {
		db.Close()
		return nil, err
	}
	return store, nil
}

func (s *Store) Close() error { return s.db.Close() }

func (s *Store) Ping(ctx context.Context) error { return s.db.PingContext(ctx) }

func (s *Store) migrate(ctx context.Context) error {
	statements := []string{
		`PRAGMA journal_mode=WAL`,
		`PRAGMA busy_timeout=5000`,
		`PRAGMA foreign_keys=ON`,
		`CREATE TABLE IF NOT EXISTS messages (
			id INTEGER PRIMARY KEY AUTOINCREMENT,
			role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
			content TEXT NOT NULL,
			created_at TEXT NOT NULL
		)`,
		`CREATE INDEX IF NOT EXISTS messages_created_at ON messages(created_at)`,
		`CREATE TABLE IF NOT EXISTS memories (
			id INTEGER PRIMARY KEY AUTOINCREMENT,
			content TEXT NOT NULL,
			source TEXT NOT NULL DEFAULT 'auto',
			status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive')),
			created_at TEXT NOT NULL,
			updated_at TEXT NOT NULL
		)`,
		`CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(content, content='memories', content_rowid='id', tokenize='trigram')`,
		`CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
			INSERT INTO memory_fts(rowid, content) VALUES (new.id, new.content);
		END`,
		`CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
			INSERT INTO memory_fts(memory_fts, rowid, content) VALUES ('delete', old.id, old.content);
		END`,
		`CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE OF content ON memories BEGIN
			INSERT INTO memory_fts(memory_fts, rowid, content) VALUES ('delete', old.id, old.content);
			INSERT INTO memory_fts(rowid, content) VALUES (new.id, new.content);
		END`,
		`CREATE TABLE IF NOT EXISTS conversation_summaries (
			id INTEGER PRIMARY KEY AUTOINCREMENT,
			content TEXT NOT NULL,
			through_message_id INTEGER NOT NULL,
			created_at TEXT NOT NULL
		)`,
	}
	for _, statement := range statements {
		if _, err := s.db.ExecContext(ctx, statement); err != nil {
			return fmt.Errorf("migrate database: %w", err)
		}
	}
	columns := []struct {
		table, name, definition string
	}{
		{"messages", "channel", `TEXT NOT NULL DEFAULT 'legacy'`},
		{"messages", "external_id", `TEXT NOT NULL DEFAULT ''`},
		{"memories", "kind", `TEXT NOT NULL DEFAULT 'fact'`},
		{"memories", "importance", `INTEGER NOT NULL DEFAULT 3`},
		{"memories", "confidence", `REAL NOT NULL DEFAULT 1.0`},
		{"memories", "last_confirmed_at", `TEXT NOT NULL DEFAULT ''`},
		{"memories", "source_message_id", `TEXT NOT NULL DEFAULT ''`},
		{"memories", "pinned", `INTEGER NOT NULL DEFAULT 0`},
		{"memories", "supersedes_id", `INTEGER NOT NULL DEFAULT 0`},
	}
	for _, column := range columns {
		if err := s.ensureColumn(ctx, column.table, column.name, column.definition); err != nil {
			return err
		}
	}
	if _, err := s.db.ExecContext(ctx, `CREATE UNIQUE INDEX IF NOT EXISTS messages_external_id ON messages(channel, external_id) WHERE external_id <> ''`); err != nil {
		return fmt.Errorf("migrate message identifiers: %w", err)
	}
	return nil
}

func (s *Store) ensureColumn(ctx context.Context, table, name, definition string) error {
	rows, err := s.db.QueryContext(ctx, `PRAGMA table_info(`+table+`)`)
	if err != nil {
		return fmt.Errorf("inspect %s schema: %w", table, err)
	}
	found := false
	for rows.Next() {
		var cid int
		var columnName, columnType string
		var notNull, pk int
		var defaultValue any
		if err := rows.Scan(&cid, &columnName, &columnType, &notNull, &defaultValue, &pk); err != nil {
			rows.Close()
			return err
		}
		if columnName == name {
			found = true
		}
	}
	if err := rows.Close(); err != nil {
		return err
	}
	if found {
		return nil
	}
	if _, err := s.db.ExecContext(ctx, `ALTER TABLE `+table+` ADD COLUMN `+name+` `+definition); err != nil {
		return fmt.Errorf("add %s.%s: %w", table, name, err)
	}
	return nil
}

func (s *Store) AddExchange(ctx context.Context, user, assistant string) error {
	return s.AddExchangeWithMeta(ctx, user, assistant, "legacy", "", time.Now())
}

func (s *Store) AddExchangeWithMeta(ctx context.Context, user, assistant, channel, externalID string, receivedAt time.Time) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if receivedAt.IsZero() {
		receivedAt = time.Now()
	}
	now := receivedAt.UTC().Format(time.RFC3339Nano)
	if channel == "" {
		channel = "unknown"
	}
	if _, err := tx.ExecContext(ctx, `INSERT INTO messages(role, content, channel, external_id, created_at) VALUES ('user', ?, ?, ?, ?)`, user, channel, externalID, now); err != nil {
		return fmt.Errorf("save user message: %w", err)
	}
	if _, err := tx.ExecContext(ctx, `INSERT INTO messages(role, content, channel, external_id, created_at) VALUES ('assistant', ?, ?, '', ?)`, assistant, channel, now); err != nil {
		return fmt.Errorf("save assistant message: %w", err)
	}
	return tx.Commit()
}

func (s *Store) ReplyForExternalID(ctx context.Context, channel, externalID string) (string, bool, error) {
	if strings.TrimSpace(externalID) == "" {
		return "", false, nil
	}
	var reply string
	err := s.db.QueryRowContext(ctx, `
		SELECT a.content
		FROM messages u
		JOIN messages a ON a.id = (
			SELECT id FROM messages
			WHERE id > u.id AND role = 'assistant'
			ORDER BY id LIMIT 1
		)
		WHERE u.channel = ? AND u.external_id = ? AND u.role = 'user'`, channel, externalID).Scan(&reply)
	if err == sql.ErrNoRows {
		return "", false, nil
	}
	if err != nil {
		return "", false, fmt.Errorf("find message reply: %w", err)
	}
	return reply, true, nil
}

func (s *Store) RecentMessages(ctx context.Context, limit int) ([]Message, error) {
	if limit <= 0 {
		return nil, nil
	}
	rows, err := s.db.QueryContext(ctx, `SELECT id, role, content, channel, external_id, created_at FROM messages ORDER BY id DESC LIMIT ?`, limit)
	if err != nil {
		return nil, fmt.Errorf("load recent messages: %w", err)
	}
	defer rows.Close()
	var reversed []Message
	for rows.Next() {
		var m Message
		var created string
		if err := rows.Scan(&m.ID, &m.Role, &m.Content, &m.Channel, &m.ExternalID, &created); err != nil {
			return nil, err
		}
		m.CreatedAt, _ = time.Parse(time.RFC3339Nano, created)
		reversed = append(reversed, m)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	result := make([]Message, len(reversed))
	for i := range reversed {
		result[len(reversed)-1-i] = reversed[i]
	}
	return result, nil
}

func (s *Store) MessagesAfter(ctx context.Context, afterID int64, limit int) ([]Message, error) {
	if limit <= 0 {
		limit = 100
	}
	rows, err := s.db.QueryContext(ctx, `SELECT id, role, content, channel, external_id, created_at FROM messages WHERE id > ? ORDER BY id LIMIT ?`, afterID, limit)
	if err != nil {
		return nil, fmt.Errorf("load messages after summary: %w", err)
	}
	defer rows.Close()
	var result []Message
	for rows.Next() {
		var item Message
		var created string
		if err := rows.Scan(&item.ID, &item.Role, &item.Content, &item.Channel, &item.ExternalID, &created); err != nil {
			return nil, err
		}
		item.CreatedAt, _ = time.Parse(time.RFC3339Nano, created)
		result = append(result, item)
	}
	return result, rows.Err()
}

func (s *Store) LatestSummary(ctx context.Context) (Summary, error) {
	var item Summary
	var created string
	err := s.db.QueryRowContext(ctx, `SELECT id, content, through_message_id, created_at FROM conversation_summaries ORDER BY id DESC LIMIT 1`).Scan(&item.ID, &item.Content, &item.ThroughMessageID, &created)
	if err == sql.ErrNoRows {
		return Summary{}, nil
	}
	if err != nil {
		return Summary{}, fmt.Errorf("load conversation summary: %w", err)
	}
	item.CreatedAt, _ = time.Parse(time.RFC3339Nano, created)
	return item, nil
}

func (s *Store) SaveSummary(ctx context.Context, content string, throughMessageID int64) (Summary, error) {
	content = strings.TrimSpace(content)
	if content == "" || throughMessageID < 1 {
		return Summary{}, fmt.Errorf("summary content and message position are required")
	}
	now := time.Now().UTC()
	result, err := s.db.ExecContext(ctx, `INSERT INTO conversation_summaries(content, through_message_id, created_at) VALUES (?, ?, ?)`, content, throughMessageID, now.Format(time.RFC3339Nano))
	if err != nil {
		return Summary{}, fmt.Errorf("save conversation summary: %w", err)
	}
	id, err := result.LastInsertId()
	if err != nil {
		return Summary{}, err
	}
	return Summary{ID: id, Content: content, ThroughMessageID: throughMessageID, CreatedAt: now}, nil
}

func (s *Store) AddMemory(ctx context.Context, content, source string) (Memory, error) {
	return s.AddStructuredMemory(ctx, Memory{Content: content, Source: source, Kind: "fact", Importance: 3, Confidence: 1})
}

func (s *Store) AddStructuredMemory(ctx context.Context, item Memory) (Memory, error) {
	content := strings.TrimSpace(item.Content)
	content = strings.TrimSpace(content)
	if content == "" {
		return Memory{}, fmt.Errorf("memory content is empty")
	}
	if item.Source == "" {
		item.Source = "auto"
	}
	if item.Kind == "" {
		item.Kind = "fact"
	}
	if item.Importance < 1 || item.Importance > 5 {
		item.Importance = 3
	}
	if item.Confidence <= 0 || item.Confidence > 1 {
		item.Confidence = 1
	}
	now := time.Now().UTC()
	if item.LastConfirmedAt.IsZero() {
		item.LastConfirmedAt = now
	}
	var existingID int64
	err := s.db.QueryRowContext(ctx, `SELECT id FROM memories WHERE status = 'active' AND lower(content) = lower(?) ORDER BY id LIMIT 1`, content).Scan(&existingID)
	if err == nil {
		if _, err := s.db.ExecContext(ctx, `UPDATE memories SET pinned = CASE WHEN ? = 1 THEN 1 ELSE pinned END, importance = max(importance, ?), confidence = max(confidence, ?), last_confirmed_at = ?, updated_at = ? WHERE id = ?`, boolInt(item.Pinned), item.Importance, item.Confidence, now.Format(time.RFC3339Nano), now.Format(time.RFC3339Nano), existingID); err != nil {
			return Memory{}, fmt.Errorf("refresh memory: %w", err)
		}
		return s.GetMemory(ctx, existingID, false)
	}
	if err != sql.ErrNoRows {
		return Memory{}, fmt.Errorf("check duplicate memory: %w", err)
	}
	result, err := s.db.ExecContext(ctx, `
		INSERT INTO memories(content, source, status, kind, importance, confidence, last_confirmed_at, source_message_id, pinned, supersedes_id, created_at, updated_at)
		VALUES (?, ?, 'active', ?, ?, ?, ?, ?, ?, ?, ?, ?)`, content, item.Source, item.Kind, item.Importance, item.Confidence,
		item.LastConfirmedAt.UTC().Format(time.RFC3339Nano), item.SourceMessageID, boolInt(item.Pinned), item.SupersedesID,
		now.Format(time.RFC3339Nano), now.Format(time.RFC3339Nano))
	if err != nil {
		return Memory{}, fmt.Errorf("add memory: %w", err)
	}
	id, err := result.LastInsertId()
	if err != nil {
		return Memory{}, err
	}
	item.ID = id
	item.Content = content
	item.Status = "active"
	item.CreatedAt = now
	item.UpdatedAt = now
	return item, nil
}

func (s *Store) UpdateMemory(ctx context.Context, id int64, content string) error {
	return s.UpdateStructuredMemory(ctx, id, Memory{Content: content})
}

func (s *Store) UpdateStructuredMemory(ctx context.Context, id int64, item Memory) error {
	content := strings.TrimSpace(item.Content)
	content = strings.TrimSpace(content)
	if content == "" {
		return fmt.Errorf("memory content is empty")
	}
	now := time.Now().UTC()
	result, err := s.db.ExecContext(ctx, `UPDATE memories SET
		content = ?,
		kind = CASE WHEN ? = '' THEN kind ELSE ? END,
		importance = CASE WHEN ? BETWEEN 1 AND 5 THEN ? ELSE importance END,
		confidence = CASE WHEN ? > 0 AND ? <= 1 THEN ? ELSE confidence END,
		last_confirmed_at = ?,
		source_message_id = CASE WHEN ? = '' THEN source_message_id ELSE ? END,
		status = 'active', updated_at = ? WHERE id = ?`,
		content, item.Kind, item.Kind, item.Importance, item.Importance,
		item.Confidence, item.Confidence, item.Confidence, now.Format(time.RFC3339Nano),
		item.SourceMessageID, item.SourceMessageID, now.Format(time.RFC3339Nano), id)
	if err != nil {
		return fmt.Errorf("update memory: %w", err)
	}
	return requireChanged(result, "memory not found")
}

func (s *Store) GetMemory(ctx context.Context, id int64, includeInactive bool) (Memory, error) {
	query := `SELECT ` + memoryColumns + ` FROM memories WHERE id = ?`
	if !includeInactive {
		query += ` AND status = 'active'`
	}
	rows, err := s.db.QueryContext(ctx, query, id)
	if err != nil {
		return Memory{}, err
	}
	defer rows.Close()
	items, err := scanMemories(rows)
	if err != nil {
		return Memory{}, err
	}
	if len(items) == 0 {
		return Memory{}, fmt.Errorf("memory not found")
	}
	return items[0], nil
}

func (s *Store) DeactivateMemory(ctx context.Context, id int64) error {
	result, err := s.db.ExecContext(ctx, `UPDATE memories SET status = 'inactive', updated_at = ? WHERE id = ?`, time.Now().UTC().Format(time.RFC3339Nano), id)
	if err != nil {
		return fmt.Errorf("deactivate memory: %w", err)
	}
	return requireChanged(result, "memory not found")
}

func requireChanged(result sql.Result, message string) error {
	n, err := result.RowsAffected()
	if err != nil {
		return err
	}
	if n == 0 {
		return fmt.Errorf("%s", message)
	}
	return nil
}

func (s *Store) ListMemories(ctx context.Context, limit int, includeInactive bool) ([]Memory, error) {
	if limit <= 0 {
		limit = 100
	}
	query := `SELECT ` + memoryColumns + ` FROM memories`
	if !includeInactive {
		query += ` WHERE status = 'active'`
	}
	query += ` ORDER BY updated_at DESC LIMIT ?`
	rows, err := s.db.QueryContext(ctx, query, limit)
	if err != nil {
		return nil, fmt.Errorf("list memories: %w", err)
	}
	defer rows.Close()
	items, err := scanMemories(rows)
	if err != nil {
		return nil, err
	}
	rerankMemories(items)
	return items, nil
}

func (s *Store) SearchMemories(ctx context.Context, query string, limit int) ([]Memory, error) {
	if limit <= 0 {
		return nil, nil
	}
	query = strings.TrimSpace(query)
	fts := makeFTSQuery(query)
	if fts == "" {
		if query == "" {
			return s.ListMemories(ctx, limit, false)
		}
		rows, err := s.db.QueryContext(ctx, `
			SELECT `+memoryColumns+`
			FROM memories
			WHERE status = 'active' AND instr(lower(content), lower(?)) > 0
			ORDER BY updated_at DESC LIMIT ?`, query, limit)
		if err != nil {
			return nil, fmt.Errorf("search short memory query: %w", err)
		}
		defer rows.Close()
		items, err := scanMemories(rows)
		if err == nil {
			rerankMemories(items)
		}
		return items, err
	}
	rows, err := s.db.QueryContext(ctx, `
		SELECT `+prefixedMemoryColumns("m")+`
		FROM memory_fts
		JOIN memories m ON m.id = memory_fts.rowid
		WHERE memory_fts MATCH ? AND m.status = 'active'
		ORDER BY bm25(memory_fts), m.updated_at DESC
		LIMIT ?`, fts, limit)
	if err != nil {
		return nil, fmt.Errorf("search memories: %w", err)
	}
	defer rows.Close()
	items, err := scanMemories(rows)
	if err == nil {
		rerankMemories(items)
	}
	return items, err
}

func (s *Store) Forget(ctx context.Context, query string, limit int) (int, error) {
	matches, err := s.SearchMemories(ctx, query, limit)
	if err != nil {
		return 0, err
	}
	if len(matches) == 0 {
		return 0, nil
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return 0, err
	}
	defer tx.Rollback()
	now := time.Now().UTC().Format(time.RFC3339Nano)
	for _, memory := range matches {
		if _, err := tx.ExecContext(ctx, `UPDATE memories SET status = 'inactive', updated_at = ? WHERE id = ?`, now, memory.ID); err != nil {
			return 0, err
		}
	}
	if err := tx.Commit(); err != nil {
		return 0, err
	}
	return len(matches), nil
}

type rowScanner interface {
	Next() bool
	Scan(...any) error
	Err() error
}

func scanMemories(rows rowScanner) ([]Memory, error) {
	var memories []Memory
	for rows.Next() {
		var m Memory
		var created, updated, confirmed string
		var pinned int
		if err := rows.Scan(&m.ID, &m.Content, &m.Source, &m.Status, &m.Kind, &m.Importance, &m.Confidence, &confirmed, &m.SourceMessageID, &pinned, &m.SupersedesID, &created, &updated); err != nil {
			return nil, err
		}
		m.Pinned = pinned != 0
		m.CreatedAt, _ = time.Parse(time.RFC3339Nano, created)
		m.UpdatedAt, _ = time.Parse(time.RFC3339Nano, updated)
		m.LastConfirmedAt, _ = time.Parse(time.RFC3339Nano, confirmed)
		memories = append(memories, m)
	}
	return memories, rows.Err()
}

const memoryColumns = `id, content, source, status, kind, importance, confidence, last_confirmed_at, source_message_id, pinned, supersedes_id, created_at, updated_at`

func prefixedMemoryColumns(prefix string) string {
	parts := strings.Split(memoryColumns, ", ")
	for i := range parts {
		parts[i] = prefix + "." + parts[i]
	}
	return strings.Join(parts, ", ")
}

func boolInt(value bool) int {
	if value {
		return 1
	}
	return 0
}

func rerankMemories(items []Memory) {
	now := time.Now()
	scores := make(map[int64]float64, len(items))
	for position, item := range items {
		value := float64(len(items)-position) + float64(item.Importance*4) + item.Confidence*3
		if item.Pinned {
			value += 30
		}
		if !item.LastConfirmedAt.IsZero() {
			ageDays := now.Sub(item.LastConfirmedAt).Hours() / 24
			if ageDays < 30 {
				value += 3
			} else if ageDays > 365 {
				value -= 2
			}
		}
		scores[item.ID] = value
	}
	sort.SliceStable(items, func(i, j int) bool {
		return scores[items[i].ID] > scores[items[j].ID]
	})
}

func (s *Store) ExportJSON(ctx context.Context) ([]byte, error) {
	messages, err := s.MessagesAfter(ctx, 0, 1_000_000)
	if err != nil {
		return nil, err
	}
	memories, err := s.ListMemories(ctx, 1_000_000, true)
	if err != nil {
		return nil, err
	}
	rows, err := s.db.QueryContext(ctx, `SELECT id, content, through_message_id, created_at FROM conversation_summaries ORDER BY id`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var summaries []Summary
	for rows.Next() {
		var item Summary
		var created string
		if err := rows.Scan(&item.ID, &item.Content, &item.ThroughMessageID, &created); err != nil {
			return nil, err
		}
		item.CreatedAt, _ = time.Parse(time.RFC3339Nano, created)
		summaries = append(summaries, item)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return json.MarshalIndent(Export{ExportedAt: time.Now().UTC(), Messages: messages, Memories: memories, Summaries: summaries}, "", "  ")
}

func (s *Store) IntegrityCheck(ctx context.Context) error {
	var result string
	if err := s.db.QueryRowContext(ctx, `PRAGMA quick_check`).Scan(&result); err != nil {
		return fmt.Errorf("check database integrity: %w", err)
	}
	if result != "ok" {
		return fmt.Errorf("database integrity check failed: %s", result)
	}
	return nil
}

func (s *Store) BackupIfDue(ctx context.Context, directory string, interval, retention time.Duration) (string, error) {
	if err := os.MkdirAll(directory, 0o700); err != nil {
		return "", fmt.Errorf("create backup directory: %w", err)
	}
	entries, err := os.ReadDir(directory)
	if err != nil {
		return "", fmt.Errorf("read backup directory: %w", err)
	}
	now := time.Now()
	var newest time.Time
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasPrefix(entry.Name(), "mneme-") || !strings.HasSuffix(entry.Name(), ".db") {
			continue
		}
		info, err := entry.Info()
		if err != nil {
			continue
		}
		if info.ModTime().After(newest) {
			newest = info.ModTime()
		}
		if retention > 0 && now.Sub(info.ModTime()) > retention {
			_ = os.Remove(filepath.Join(directory, entry.Name()))
		}
	}
	if !newest.IsZero() && now.Sub(newest) < interval {
		return "", nil
	}
	path := filepath.Join(directory, "mneme-"+now.UTC().Format("20060102T150405.000000000Z")+".db")
	if _, err := s.db.ExecContext(ctx, `VACUUM INTO ?`, path); err != nil {
		return "", fmt.Errorf("back up database: %w", err)
	}
	if err := os.Chmod(path, 0o600); err != nil {
		return "", fmt.Errorf("secure backup permissions: %w", err)
	}
	return path, nil
}

func makeFTSQuery(input string) string {
	var groups []string
	var current []rune
	flush := func() {
		if len(current) >= 3 {
			for i := 0; i+3 <= len(current) && len(groups) < 32; i++ {
				groups = append(groups, `"`+strings.ReplaceAll(string(current[i:i+3]), `"`, `""`)+`"`)
			}
		}
		current = nil
	}
	for _, r := range []rune(strings.ToLower(input)) {
		if unicode.IsLetter(r) || unicode.IsNumber(r) {
			current = append(current, r)
		} else {
			flush()
		}
		if len(groups) >= 32 {
			break
		}
	}
	flush()
	return strings.Join(groups, " OR ")
}
