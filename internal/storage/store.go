package storage

import (
	"context"
	"database/sql"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
	"unicode"

	_ "modernc.org/sqlite"
)

type Store struct {
	db *sql.DB
}

type Message struct {
	ID        int64     `json:"id"`
	Role      string    `json:"role"`
	Content   string    `json:"content"`
	CreatedAt time.Time `json:"created_at"`
}

type Memory struct {
	ID        int64     `json:"id"`
	Content   string    `json:"content"`
	Source    string    `json:"source"`
	Status    string    `json:"status"`
	CreatedAt time.Time `json:"created_at"`
	UpdatedAt time.Time `json:"updated_at"`
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
			conversation_id TEXT NOT NULL DEFAULT 'default',
			role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
			content TEXT NOT NULL,
			created_at TEXT NOT NULL
		)`,
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
	}
	for _, statement := range statements {
		if _, err := s.db.ExecContext(ctx, statement); err != nil {
			return fmt.Errorf("migrate database: %w", err)
		}
	}
	if err := s.ensureMessagesConversationID(ctx); err != nil {
		return err
	}
	if _, err := s.db.ExecContext(ctx, `CREATE INDEX IF NOT EXISTS messages_conversation_id ON messages(conversation_id, id)`); err != nil {
		return fmt.Errorf("create message index: %w", err)
	}
	return nil
}

func (s *Store) ensureMessagesConversationID(ctx context.Context) error {
	rows, err := s.db.QueryContext(ctx, `PRAGMA table_info(messages)`)
	if err != nil {
		return fmt.Errorf("inspect messages table: %w", err)
	}
	defer rows.Close()
	found := false
	for rows.Next() {
		var cid, notNull, primaryKey int
		var name, kind string
		var defaultValue any
		if err := rows.Scan(&cid, &name, &kind, &notNull, &defaultValue, &primaryKey); err != nil {
			return err
		}
		if name == "conversation_id" {
			found = true
		}
	}
	if err := rows.Err(); err != nil {
		return err
	}
	if err := rows.Close(); err != nil {
		return err
	}
	if found {
		return nil
	}
	if _, err := s.db.ExecContext(ctx, `ALTER TABLE messages ADD COLUMN conversation_id TEXT NOT NULL DEFAULT 'default'`); err != nil {
		return fmt.Errorf("add conversation_id to messages: %w", err)
	}
	return nil
}

func (s *Store) AddExchange(ctx context.Context, conversationID, user, assistant string) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := tx.ExecContext(ctx, `INSERT INTO messages(conversation_id, role, content, created_at) VALUES (?, 'user', ?, ?)`, conversationID, user, now); err != nil {
		return fmt.Errorf("save user message: %w", err)
	}
	if _, err := tx.ExecContext(ctx, `INSERT INTO messages(conversation_id, role, content, created_at) VALUES (?, 'assistant', ?, ?)`, conversationID, assistant, now); err != nil {
		return fmt.Errorf("save assistant message: %w", err)
	}
	return tx.Commit()
}

func (s *Store) RecentMessages(ctx context.Context, conversationID string, limit int) ([]Message, error) {
	if limit <= 0 {
		return nil, nil
	}
	rows, err := s.db.QueryContext(ctx, `SELECT id, role, content, created_at FROM messages WHERE conversation_id = ? ORDER BY id DESC LIMIT ?`, conversationID, limit)
	if err != nil {
		return nil, fmt.Errorf("load recent messages: %w", err)
	}
	defer rows.Close()
	var reversed []Message
	for rows.Next() {
		var m Message
		var created string
		if err := rows.Scan(&m.ID, &m.Role, &m.Content, &created); err != nil {
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

func (s *Store) AddMemory(ctx context.Context, content, source string) (Memory, error) {
	content = strings.TrimSpace(content)
	if content == "" {
		return Memory{}, fmt.Errorf("memory content is empty")
	}
	if source == "" {
		source = "auto"
	}
	now := time.Now().UTC()
	result, err := s.db.ExecContext(ctx, `INSERT INTO memories(content, source, status, created_at, updated_at) VALUES (?, ?, 'active', ?, ?)`, content, source, now.Format(time.RFC3339Nano), now.Format(time.RFC3339Nano))
	if err != nil {
		return Memory{}, fmt.Errorf("add memory: %w", err)
	}
	id, err := result.LastInsertId()
	if err != nil {
		return Memory{}, err
	}
	return Memory{ID: id, Content: content, Source: source, Status: "active", CreatedAt: now, UpdatedAt: now}, nil
}

func (s *Store) UpdateMemory(ctx context.Context, id int64, content string) error {
	content = strings.TrimSpace(content)
	if content == "" {
		return fmt.Errorf("memory content is empty")
	}
	result, err := s.db.ExecContext(ctx, `UPDATE memories SET content = ?, status = 'active', updated_at = ? WHERE id = ?`, content, time.Now().UTC().Format(time.RFC3339Nano), id)
	if err != nil {
		return fmt.Errorf("update memory: %w", err)
	}
	return requireChanged(result, "memory not found")
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
	query := `SELECT id, content, source, status, created_at, updated_at FROM memories`
	if !includeInactive {
		query += ` WHERE status = 'active'`
	}
	query += ` ORDER BY updated_at DESC LIMIT ?`
	rows, err := s.db.QueryContext(ctx, query, limit)
	if err != nil {
		return nil, fmt.Errorf("list memories: %w", err)
	}
	defer rows.Close()
	return scanMemories(rows)
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
			SELECT id, content, source, status, created_at, updated_at
			FROM memories
			WHERE status = 'active' AND instr(lower(content), lower(?)) > 0
			ORDER BY updated_at DESC LIMIT ?`, query, limit)
		if err != nil {
			return nil, fmt.Errorf("search short memory query: %w", err)
		}
		defer rows.Close()
		return scanMemories(rows)
	}
	rows, err := s.db.QueryContext(ctx, `
		SELECT m.id, m.content, m.source, m.status, m.created_at, m.updated_at
		FROM memory_fts
		JOIN memories m ON m.id = memory_fts.rowid
		WHERE memory_fts MATCH ? AND m.status = 'active'
		ORDER BY bm25(memory_fts), m.updated_at DESC
		LIMIT ?`, fts, limit)
	if err != nil {
		return nil, fmt.Errorf("search memories: %w", err)
	}
	defer rows.Close()
	return scanMemories(rows)
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
		var created, updated string
		if err := rows.Scan(&m.ID, &m.Content, &m.Source, &m.Status, &created, &updated); err != nil {
			return nil, err
		}
		m.CreatedAt, _ = time.Parse(time.RFC3339Nano, created)
		m.UpdatedAt, _ = time.Parse(time.RFC3339Nano, updated)
		memories = append(memories, m)
	}
	return memories, rows.Err()
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
