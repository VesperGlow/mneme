package llm

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
)

type Client struct {
	baseURL string
	apiKey  string
	model   string
	http    *http.Client
}

type Message struct {
	Role       string     `json:"role"`
	Content    string     `json:"content,omitempty"`
	ToolCalls  []ToolCall `json:"tool_calls,omitempty"`
	ToolCallID string     `json:"tool_call_id,omitempty"`
}

type ToolCall struct {
	ID       string   `json:"id"`
	Type     string   `json:"type"`
	Function Function `json:"function"`
}

type Function struct {
	Name      string `json:"name"`
	Arguments string `json:"arguments"`
}

type Tool struct {
	Type     string         `json:"type"`
	Function ToolDefinition `json:"function"`
}

type ToolDefinition struct {
	Name        string         `json:"name"`
	Description string         `json:"description"`
	Parameters  map[string]any `json:"parameters"`
}

func New(baseURL, apiKey, model string, httpClient *http.Client) *Client {
	if httpClient == nil {
		httpClient = http.DefaultClient
	}
	return &Client{
		baseURL: completionURL(baseURL),
		apiKey:  apiKey,
		model:   model,
		http:    httpClient,
	}
}

func completionURL(baseURL string) string {
	url := strings.TrimRight(strings.TrimSpace(baseURL), "/")
	if strings.HasSuffix(url, "/chat/completions") {
		return url
	}
	return url + "/chat/completions"
}

func (c *Client) Complete(ctx context.Context, messages []Message, tools []Tool) (Message, error) {
	body := struct {
		Model      string    `json:"model"`
		Messages   []Message `json:"messages"`
		Tools      []Tool    `json:"tools,omitempty"`
		ToolChoice string    `json:"tool_choice,omitempty"`
	}{
		Model:    c.model,
		Messages: messages,
		Tools:    tools,
	}
	if len(tools) > 0 {
		body.ToolChoice = "auto"
	}
	payload, err := json.Marshal(body)
	if err != nil {
		return Message{}, fmt.Errorf("encode LLM request: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.baseURL, bytes.NewReader(payload))
	if err != nil {
		return Message{}, fmt.Errorf("create LLM request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+c.apiKey)
	req.Header.Set("Content-Type", "application/json")
	resp, err := c.http.Do(req)
	if err != nil {
		return Message{}, fmt.Errorf("call LLM: %w", err)
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 4<<20))
	if err != nil {
		return Message{}, fmt.Errorf("read LLM response: %w", err)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return Message{}, fmt.Errorf("LLM returned %s: %s", resp.Status, strings.TrimSpace(string(raw)))
	}
	var result struct {
		Choices []struct {
			Message Message `json:"message"`
		} `json:"choices"`
		Error *struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.Unmarshal(raw, &result); err != nil {
		return Message{}, fmt.Errorf("decode LLM response: %w", err)
	}
	if result.Error != nil {
		return Message{}, fmt.Errorf("LLM error: %s", result.Error.Message)
	}
	if len(result.Choices) == 0 {
		return Message{}, fmt.Errorf("LLM returned no choices")
	}
	return result.Choices[0].Message, nil
}
