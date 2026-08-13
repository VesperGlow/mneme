package storage

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
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

func TestStoreMetadataSummaryExportAndBackup(t *testing.T) {
	directory := t.TempDir()
	store, err := Open(filepath.Join(directory, "companion.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	ctx := context.Background()

	if err := store.AddExchangeWithMeta(ctx, "记住我的项目", "好的", "qq", "message-1", time.Now()); err != nil {
		t.Fatal(err)
	}
	reply, found, err := store.ReplyForExternalID(ctx, "qq", "message-1")
	if err != nil || !found || reply != "好的" {
		t.Fatalf("unexpected deduplicated reply: %q %t %v", reply, found, err)
	}
	item, err := store.AddStructuredMemory(ctx, Memory{
		Content: "用户正在开发 Mneme", Source: "manual", Kind: "project", Importance: 5, Confidence: 1, Pinned: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	loaded, err := store.GetMemory(ctx, item.ID, false)
	if err != nil || !loaded.Pinned || loaded.Kind != "project" || loaded.Importance != 5 {
		t.Fatalf("unexpected structured memory: %#v %v", loaded, err)
	}
	duplicate, err := store.AddStructuredMemory(ctx, Memory{Content: "用户正在开发 Mneme", Source: "auto", Kind: "project", Importance: 3, Confidence: 1})
	if err != nil || duplicate.ID != item.ID {
		t.Fatalf("exact duplicate was not merged: %#v %v", duplicate, err)
	}
	messages, err := store.MessagesAfter(ctx, 0, 10)
	if err != nil || len(messages) != 2 {
		t.Fatalf("unexpected messages: %#v %v", messages, err)
	}
	if _, err := store.SaveSummary(ctx, "用户正在开发 Mneme。", messages[len(messages)-1].ID); err != nil {
		t.Fatal(err)
	}
	if summary, err := store.LatestSummary(ctx); err != nil || summary.Content == "" {
		t.Fatalf("unexpected summary: %#v %v", summary, err)
	}
	raw, err := store.ExportJSON(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var exported Export
	if err := json.Unmarshal(raw, &exported); err != nil || len(exported.Messages) != 2 || len(exported.Memories) != 1 {
		t.Fatalf("unexpected export: %#v %v", exported, err)
	}
	backup, err := store.BackupIfDue(ctx, filepath.Join(directory, "backups"), time.Hour, 24*time.Hour)
	if err != nil || backup == "" {
		t.Fatalf("backup failed: %q %v", backup, err)
	}
	if info, err := os.Stat(backup); err != nil || info.Size() == 0 {
		t.Fatalf("invalid backup: %v %v", info, err)
	}
	if second, err := store.BackupIfDue(ctx, filepath.Join(directory, "backups"), time.Hour, 24*time.Hour); err != nil || second != "" {
		t.Fatalf("backup interval not respected: %q %v", second, err)
	}
	if err := store.IntegrityCheck(ctx); err != nil {
		t.Fatal(err)
	}
}
