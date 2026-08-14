package search

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
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

func TestExtract(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/extract" {
			t.Errorf("unexpected path %q", r.URL.Path)
		}
		if r.Header.Get("Authorization") != "Bearer tvly-test" {
			t.Errorf("missing authorization")
		}
		var body map[string]any
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			t.Fatal(err)
		}
		if body["urls"] != "https://example.com/article" || body["query"] != "核心结论" {
			t.Errorf("unexpected extract request: %#v", body)
		}
		if body["extract_depth"] != "basic" || body["format"] != "markdown" || body["chunks_per_source"] != float64(3) {
			t.Errorf("unexpected extract options: %#v", body)
		}
		_, _ = w.Write([]byte(`{"results":[{"url":"https://example.com/article","raw_content":"页面正文"}],"failed_results":[]}`))
	}))
	defer server.Close()

	client := NewWithBaseURL("tvly-test", server.URL, server.Client())
	result, err := client.Extract(context.Background(), "https://example.com/article", "核心结论")
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Results) != 1 || result.Results[0].RawContent != "页面正文" {
		t.Fatalf("unexpected result: %#v", result)
	}
}

func TestExtractRejectsUnsafeURLs(t *testing.T) {
	client := New("tvly-test", nil)
	for _, rawURL := range []string{
		"file:///etc/passwd",
		"http://localhost/admin",
		"http://127.0.0.1/admin",
		"http://169.254.169.254/latest/meta-data",
		"https://user:password@example.com/private",
	} {
		t.Run(strings.ReplaceAll(rawURL, "/", "_"), func(t *testing.T) {
			if _, err := client.Extract(context.Background(), rawURL, "test"); err == nil {
				t.Fatalf("expected %q to be rejected", rawURL)
			}
		})
	}
}
