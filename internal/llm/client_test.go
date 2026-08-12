package llm

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestComplete(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/chat/completions" {
			t.Errorf("unexpected path %q", r.URL.Path)
		}
		if r.Header.Get("Authorization") != "Bearer secret" {
			t.Errorf("unexpected authorization header")
		}
		var body map[string]any
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			t.Fatal(err)
		}
		if body["model"] != "test-model" {
			t.Errorf("unexpected model: %v", body["model"])
		}
		if body["reasoning_effort"] != "max" {
			t.Errorf("unexpected reasoning effort: %v", body["reasoning_effort"])
		}
		thinking, ok := body["thinking"].(map[string]any)
		if !ok || thinking["type"] != "enabled" {
			t.Errorf("thinking mode was not enabled: %v", body["thinking"])
		}
		if _, exists := body["tool_choice"]; exists {
			t.Errorf("DeepSeek thinking mode must not send tool_choice")
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"choices":[{"message":{"role":"assistant","reasoning_content":"internal","content":"hello"}}]}`))
	}))
	defer server.Close()

	client := NewWithBaseURL(server.URL+"/v1", "secret", "test-model", server.Client())
	message, err := client.Complete(context.Background(), []Message{{Role: "user", Content: "hi"}}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if message.Content != "hello" {
		t.Fatalf("unexpected response: %#v", message)
	}
	if message.ReasoningContent != "internal" {
		t.Fatalf("reasoning content was not preserved: %#v", message)
	}
}
