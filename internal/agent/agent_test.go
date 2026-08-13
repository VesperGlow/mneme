package agent

import (
	"context"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	"companion/internal/llm"
	"companion/internal/storage"
)

func TestChatInputDeduplicatesChannelMessage(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"choices":[{"message":{"role":"assistant","content":"你好呀"}}]}`))
	}))
	defer server.Close()

	store, err := storage.Open(filepath.Join(t.TempDir(), "agent.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	client := llm.NewWithBaseURL(server.URL, "secret", "test-model", "max", server.Client())
	a := NewWithOptions(store, client, nil, nil, "system", "persona", Options{
		RecentMessages: 20, MaxMemories: 5, MaxToolCalls: 0, MemoryQueueSize: 2, SummaryEvery: 0,
	}, nil)
	t.Cleanup(func() {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		_ = a.Close(ctx)
	})

	input := Input{Channel: "qq", MessageID: "qq-message-1", Content: "你好", ReceivedAt: time.Now()}
	first, err := a.ChatInput(context.Background(), input)
	if err != nil {
		t.Fatal(err)
	}
	second, err := a.ChatInput(context.Background(), input)
	if err != nil {
		t.Fatal(err)
	}
	if first != "你好呀" || second != first || calls.Load() != 1 {
		t.Fatalf("deduplication failed: first=%q second=%q calls=%d", first, second, calls.Load())
	}
}

func TestParseMemoryID(t *testing.T) {
	if id, err := parseMemoryID("#42"); err != nil || id != 42 {
		t.Fatalf("unexpected id: %d %v", id, err)
	}
	if _, err := parseMemoryID("nope"); err == nil {
		t.Fatal("expected invalid id error")
	}
}
