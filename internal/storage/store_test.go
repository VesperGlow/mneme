package storage

import (
	"context"
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

	if err := store.AddExchange(ctx, "你好", "你好呀"); err != nil {
		t.Fatal(err)
	}
	messages, err := store.RecentMessages(ctx, 10)
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
