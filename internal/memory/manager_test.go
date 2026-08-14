package memory

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"sync/atomic"
	"testing"

	"companion/internal/llm"
	"companion/internal/storage"
)

func TestParseDecisionWithCodeFence(t *testing.T) {
	parsed, err := parseDecision("```json\n{\"actions\":[{\"action\":\"add\",\"content\":\"用户喜欢茶\"}]}\n```")
	if err != nil {
		t.Fatal(err)
	}
	if len(parsed.Actions) != 1 || parsed.Actions[0].Content != "用户喜欢茶" {
		t.Fatalf("unexpected decision: %#v", parsed)
	}
}

func TestRetrieveUsesFlashSearchAndRejectsUnknownIDs(t *testing.T) {
	store, err := storage.Open(filepath.Join(t.TempDir(), "memory.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	memory, err := store.AddStructuredMemory(context.Background(), storage.Memory{
		Content: "用户喝咖啡不加糖，偏好深烘焙", Source: "manual", Kind: "preference", Importance: 5, Confidence: 1,
	})
	if err != nil {
		t.Fatal(err)
	}

	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var request struct {
			Messages []llm.Message `json:"messages"`
			Tools    []llm.Tool    `json:"tools"`
		}
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Errorf("decode request: %v", err)
		}
		w.Header().Set("Content-Type", "application/json")
		if calls.Add(1) == 1 {
			if len(request.Tools) != 1 || request.Tools[0].Function.Name != "search_memories" {
				t.Errorf("retrieval tool missing: %#v", request.Tools)
			}
			_, _ = w.Write([]byte(`{"choices":[{"message":{"role":"assistant","content":"","reasoning_content":"需要查咖啡偏好","tool_calls":[{"id":"search-1","type":"function","function":{"name":"search_memories","arguments":"{\"query\":\"不加糖\"}"}}]}}]}`))
			return
		}
		if len(request.Messages) == 0 || request.Messages[len(request.Messages)-1].Role != "tool" {
			t.Errorf("tool result was not returned to model: %#v", request.Messages)
		}
		_, _ = fmt.Fprintf(w, `{"choices":[{"message":{"role":"assistant","content":"{\"ids\":[%d,999999]}"}}]}`, memory.ID)
	}))
	defer server.Close()

	client := llm.NewWithBaseURL(server.URL, "secret", "flash", "high", server.Client())
	manager := New(store, client)
	items, err := manager.Retrieve(context.Background(), "帮我选一杯咖啡", []storage.Message{{Role: "user", Content: "我去咖啡店了"}}, 5)
	if err != nil {
		t.Fatal(err)
	}
	if calls.Load() != 2 || len(items) != 1 || items[0].ID != memory.ID {
		t.Fatalf("unexpected retrieval: calls=%d items=%#v", calls.Load(), items)
	}
}

func TestParseRetrievalDecisionWithCodeFence(t *testing.T) {
	parsed, err := parseRetrievalDecision("```json\n{\"ids\":[3,5]}\n```")
	if err != nil {
		t.Fatal(err)
	}
	if len(parsed.IDs) != 2 || parsed.IDs[0] != 3 || parsed.IDs[1] != 5 {
		t.Fatalf("unexpected retrieval decision: %#v", parsed)
	}
}
