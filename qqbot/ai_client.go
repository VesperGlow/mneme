package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

type AIClient struct {
	url    string
	apiKey string
	http   *http.Client
}

type aiChatRequest struct {
	UserID         string `json:"user_id"`
	Message        string `json:"message"`
	ConversationID string `json:"conversation_id"`
}

type aiChatResponse struct {
	Message string `json:"message"`
}

func NewAIClient(url, apiKey string, timeout time.Duration) *AIClient {
	return &AIClient{
		url:    strings.TrimRight(url, "/"),
		apiKey: apiKey,
		http:   &http.Client{Timeout: timeout},
	}
}

func (c *AIClient) Chat(ctx context.Context, userID, conversationID, message string) (string, error) {
	payload, err := json.Marshal(aiChatRequest{
		UserID: userID, Message: message, ConversationID: conversationID,
	})
	if err != nil {
		return "", err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.url, bytes.NewReader(payload))
	if err != nil {
		return "", err
	}
	req.Header.Set("Content-Type", "application/json")
	if c.apiKey != "" {
		req.Header.Set("Authorization", "Bearer "+c.apiKey)
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return "", fmt.Errorf("调用记忆助手失败: %w", err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(io.LimitReader(resp.Body, 2<<20))
	if err != nil {
		return "", fmt.Errorf("读取记忆助手响应失败: %w", err)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return "", fmt.Errorf("记忆助手返回 HTTP %d: %s", resp.StatusCode, limitString(string(body), 1000))
	}
	var result aiChatResponse
	if err := json.Unmarshal(body, &result); err != nil {
		return "", fmt.Errorf("解析记忆助手响应失败: %w", err)
	}
	if strings.TrimSpace(result.Message) == "" {
		return "", fmt.Errorf("记忆助手返回了空消息")
	}
	return strings.TrimSpace(result.Message), nil
}

func limitString(value string, max int) string {
	runes := []rune(value)
	if len(runes) <= max {
		return value
	}
	return string(runes[:max]) + "…"
}
