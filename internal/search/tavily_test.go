package search

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestSearch(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/search" {
			t.Errorf("unexpected path %q", r.URL.Path)
		}
		if r.Header.Get("Authorization") != "Bearer tvly-test" {
			t.Errorf("missing authorization")
		}
		var body map[string]any
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			t.Fatal(err)
		}
		if body["query"] != "Go current version" {
			t.Errorf("unexpected query: %v", body["query"])
		}
		_, _ = w.Write([]byte(`{"answer":"Go 1.x","results":[{"title":"Go","url":"https://go.dev","content":"release"}]}`))
	}))
	defer server.Close()

	client := NewWithBaseURL("tvly-test", server.URL, server.Client())
	result, err := client.Search(context.Background(), "Go current version")
	if err != nil {
		t.Fatal(err)
	}
	if result.Answer != "Go 1.x" || len(result.Results) != 1 {
		t.Fatalf("unexpected result: %#v", result)
	}
}

func TestSearchWithoutKey(t *testing.T) {
	client := New("", nil)
	if _, err := client.Search(context.Background(), "anything"); err == nil {
		t.Fatal("expected missing key error")
	}
}
