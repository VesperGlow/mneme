package search

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
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

type ExtractResult struct {
	URL        string `json:"url"`
	RawContent string `json:"raw_content"`
}

type FailedExtractResult struct {
	URL   string `json:"url"`
	Error string `json:"error"`
}

type ExtractResponse struct {
	Results       []ExtractResult       `json:"results"`
	FailedResults []FailedExtractResult `json:"failed_results"`
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

func (c *Client) Extract(ctx context.Context, rawURL, query string) (ExtractResponse, error) {
	if c.apiKey == "" {
		return ExtractResponse{}, fmt.Errorf("web extraction is unavailable: TAVILY_API_KEY is not configured")
	}
	rawURL = strings.TrimSpace(rawURL)
	if err := validatePublicURL(rawURL); err != nil {
		return ExtractResponse{}, err
	}
	query = strings.TrimSpace(query)
	if query == "" {
		query = "总结页面的主要内容"
	}
	body := map[string]any{
		"urls":              rawURL,
		"query":             query,
		"chunks_per_source": 3,
		"extract_depth":     "basic",
		"include_images":    false,
		"format":            "markdown",
	}
	payload, err := json.Marshal(body)
	if err != nil {
		return ExtractResponse{}, fmt.Errorf("encode Tavily extract request: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.baseURL+"/extract", bytes.NewReader(payload))
	if err != nil {
		return ExtractResponse{}, fmt.Errorf("create Tavily extract request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+c.apiKey)
	req.Header.Set("Content-Type", "application/json")
	resp, err := c.http.Do(req)
	if err != nil {
		return ExtractResponse{}, fmt.Errorf("call Tavily extract: %w", err)
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 2<<20))
	if err != nil {
		return ExtractResponse{}, fmt.Errorf("read Tavily extract response: %w", err)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return ExtractResponse{}, fmt.Errorf("Tavily extract returned %s: %s", resp.Status, strings.TrimSpace(string(raw)))
	}
	var result ExtractResponse
	if err := json.Unmarshal(raw, &result); err != nil {
		return ExtractResponse{}, fmt.Errorf("decode Tavily extract response: %w", err)
	}
	return result, nil
}

func validatePublicURL(rawURL string) error {
	if rawURL == "" {
		return fmt.Errorf("URL is empty")
	}
	if len(rawURL) > 2048 {
		return fmt.Errorf("URL is too long")
	}
	parsed, err := url.ParseRequestURI(rawURL)
	if err != nil {
		return fmt.Errorf("invalid URL: %w", err)
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return fmt.Errorf("URL scheme must be http or https")
	}
	if parsed.Hostname() == "" {
		return fmt.Errorf("URL host is empty")
	}
	if parsed.User != nil {
		return fmt.Errorf("URL must not include credentials")
	}
	host := strings.ToLower(strings.TrimSuffix(parsed.Hostname(), "."))
	if host == "localhost" || strings.HasSuffix(host, ".localhost") || strings.HasSuffix(host, ".local") {
		return fmt.Errorf("local URLs are not allowed")
	}
	if ip := net.ParseIP(host); ip != nil && (ip.IsPrivate() || ip.IsLoopback() || ip.IsLinkLocalUnicast() || ip.IsLinkLocalMulticast() || ip.IsUnspecified()) {
		return fmt.Errorf("private or local IP addresses are not allowed")
	}
	return nil
}
