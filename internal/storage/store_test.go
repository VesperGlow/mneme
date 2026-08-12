package storage

import (
	"context"
	"database/sql"
	"path/filepath"
	"testing"
)

func TestStoreMessagesAndMemories(t *testing.T) {
	store, err := Open(filepath.Join(t.TempDir(), "companion.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	ctx := context.Background()

	if err := store.AddExchange(ctx, "chat-a", "你好", "你好呀"); err != nil {
		t.Fatal(err)
	}
	if err := store.AddExchange(ctx, "chat-b", "另一个会话", "不会串进来"); err != nil {
		t.Fatal(err)
	}
	messages, err := store.RecentMessages(ctx, "chat-a", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(messages) != 2 || messages[0].Role != "user" || messages[1].Role != "assistant" {
		t.Fatalf("unexpected messages: %#v", messages)
	}

	first, err := store.AddMemory(ctx, "用户喝咖啡不加糖，偏好深烘焙", "manual")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.AddMemory(ctx, "用户的重要朋友叫小明", "auto"); err != nil {
		t.Fatal(err)
	}
	matches, err := store.SearchMemories(ctx, "推荐咖啡时记得不加糖", 5)
	if err != nil {
		t.Fatal(err)
	}
	if len(matches) == 0 || matches[0].ID != first.ID {
		t.Fatalf("expected coffee memory, got %#v", matches)
	}
	forgotten, err := store.Forget(ctx, "小明", 5)
	if err != nil {
		t.Fatal(err)
	}
	if forgotten != 1 {
		t.Fatalf("expected one forgotten memory, got %d", forgotten)
	}
}

func TestOpenMigratesExistingMessages(t *testing.T) {
	path := filepath.Join(t.TempDir(), "old.db")
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	_, err = db.Exec(`CREATE TABLE messages (
		id INTEGER PRIMARY KEY AUTOINCREMENT,
		role TEXT NOT NULL,
		content TEXT NOT NULL,
		created_at TEXT NOT NULL
	)`)
	if err != nil {
		t.Fatal(err)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}

	store, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	if err := store.AddExchange(context.Background(), "default", "旧库", "已迁移"); err != nil {
		t.Fatal(err)
	}
}
