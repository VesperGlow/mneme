package agent

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"companion/internal/llm"
	"companion/internal/search"
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

func TestChatInputOpensURL(t *testing.T) {
	extractServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/extract" {
			t.Errorf("unexpected extract path %q", r.URL.Path)
		}
		_, _ = w.Write([]byte(`{"results":[{"url":"https://example.com/article","raw_content":"页面里的核心事实"}],"failed_results":[]}`))
	}))
	defer extractServer.Close()

	var calls atomic.Int32
	llmServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		call := calls.Add(1)
		w.Header().Set("Content-Type", "application/json")
		if call == 1 {
			var request struct {
				Tools []llm.Tool `json:"tools"`
			}
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Fatal(err)
			}
			foundTool := false
			for _, tool := range request.Tools {
				if tool.Function.Name == "open_url" {
					foundTool = true
				}
			}
			if !foundTool {
				t.Errorf("open_url tool was not registered: %#v", request.Tools)
			}
			_, _ = w.Write([]byte(`{"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"call-1","type":"function","function":{"name":"open_url","arguments":"{\"url\":\"https://example.com/article\",\"query\":\"核心事实\"}"}}]}}]}`))
			return
		}
		var request struct {
			Messages []llm.Message `json:"messages"`
		}
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Fatal(err)
		}
		foundPage := false
		for _, message := range request.Messages {
			if message.Role == "tool" && strings.Contains(message.Content, "页面里的核心事实") && strings.Contains(message.Content, "不可信外部资料") {
				foundPage = true
			}
		}
		if !foundPage {
			t.Errorf("extracted page was not returned to the model: %#v", request.Messages)
		}
		_, _ = w.Write([]byte(`{"choices":[{"message":{"role":"assistant","content":"这是页面摘要"}}]}`))
	}))
	defer llmServer.Close()

	store, err := storage.Open(filepath.Join(t.TempDir(), "agent.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	llmClient := llm.NewWithBaseURL(llmServer.URL, "secret", "test-model", "max", llmServer.Client())
	searchClient := search.NewWithBaseURL("tvly-test", extractServer.URL, extractServer.Client())
	a := NewWithOptions(store, llmClient, searchClient, nil, "system", "persona", Options{
		RecentMessages: 20, MaxMemories: 0, MaxToolCalls: 1, MemoryQueueSize: 2, SummaryEvery: 0,
	}, nil)
	t.Cleanup(func() {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		_ = a.Close(ctx)
	})

	reply, err := a.ChatInput(context.Background(), Input{Channel: "qq", Content: "看看这个链接 https://example.com/article", ReceivedAt: time.Now()})
	if err != nil {
		t.Fatal(err)
	}
	if reply != "这是页面摘要" || calls.Load() != 2 {
		t.Fatalf("unexpected reply=%q calls=%d", reply, calls.Load())
	}
}
