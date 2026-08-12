package search

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
)

const defaultBaseURL = "https://api.tavily.com"

type Client struct {
	apiKey  string
	baseURL string
	http    *http.Client
}

type Result struct {
	Title   string  `json:"title"`
	URL     string  `json:"url"`
	Content string  `json:"content"`
	Score   float64 `json:"score,omitempty"`
}

type Response struct {
	Answer  string   `json:"answer,omitempty"`
	Results []Result `json:"results"`
}

func New(apiKey string, httpClient *http.Client) *Client {
	return NewWithBaseURL(apiKey, defaultBaseURL, httpClient)
}

func NewWithBaseURL(apiKey, baseURL string, httpClient *http.Client) *Client {
	if httpClient == nil {
		httpClient = http.DefaultClient
	}
	return &Client{apiKey: strings.TrimSpace(apiKey), baseURL: strings.TrimRight(baseURL, "/"), http: httpClient}
}

func (c *Client) Search(ctx context.Context, query string) (Response, error) {
	if c.apiKey == "" {
		return Response{}, fmt.Errorf("web search is unavailable: TAVILY_API_KEY is not configured")
	}
	query = strings.TrimSpace(query)
	if query == "" {
		return Response{}, fmt.Errorf("search query is empty")
	}
	body := map[string]any{
		"query":          query,
		"search_depth":   "basic",
		"max_results":    5,
		"include_answer": true,
	}
	payload, err := json.Marshal(body)
	if err != nil {
		return Response{}, fmt.Errorf("encode Tavily request: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.baseURL+"/search", bytes.NewReader(payload))
	if err != nil {
		return Response{}, fmt.Errorf("create Tavily request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+c.apiKey)
	req.Header.Set("Content-Type", "application/json")
	resp, err := c.http.Do(req)
	if err != nil {
		return Response{}, fmt.Errorf("call Tavily: %w", err)
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 2<<20))
	if err != nil {
		return Response{}, fmt.Errorf("read Tavily response: %w", err)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return Response{}, fmt.Errorf("Tavily returned %s: %s", resp.Status, strings.TrimSpace(string(raw)))
	}
	var result Response
	if err := json.Unmarshal(raw, &result); err != nil {
		return Response{}, fmt.Errorf("decode Tavily response: %w", err)
	}
	return result, nil
}
