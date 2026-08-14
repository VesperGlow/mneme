package memory

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"companion/internal/llm"
	"companion/internal/storage"
	"companion/prompts"
)

type Manager struct {
	store *storage.Store
	llm   *llm.Client
}

type action struct {
	Action     string  `json:"action"`
	ID         int64   `json:"id,omitempty"`
	Content    string  `json:"content,omitempty"`
	Kind       string  `json:"kind,omitempty"`
	Importance int     `json:"importance,omitempty"`
	Confidence float64 `json:"confidence,omitempty"`
}

type decision struct {
	Actions []action `json:"actions"`
}

type retrievalDecision struct {
	IDs []int64 `json:"ids"`
}

const maxRetrievalSearches = 3

func New(store *storage.Store, client *llm.Client) *Manager {
	return &Manager{store: store, llm: client}
}

func (m *Manager) Retrieve(ctx context.Context, input string, recent []storage.Message, limit int) ([]storage.Memory, error) {
	if m == nil || m.store == nil || m.llm == nil {
		return nil, fmt.Errorf("memory retriever is unavailable")
	}
	if limit <= 0 {
		return nil, nil
	}
	if len(recent) > 6 {
		recent = recent[len(recent)-6:]
	}
	payload, err := json.Marshal(struct {
		Recent  []storage.Message `json:"recent,omitempty"`
		Current string            `json:"current"`
	}{Recent: recent, Current: strings.TrimSpace(input)})
	if err != nil {
		return nil, err
	}
	messages := []llm.Message{
		{Role: "system", Content: prompts.Retrieval},
		{Role: "user", Content: string(payload)},
	}
	tools := []llm.Tool{memorySearchTool()}
	candidates := make(map[int64]storage.Memory)
	searches := 0
	for round := 0; round < maxRetrievalSearches+2; round++ {
		response, err := m.llm.Complete(ctx, messages, tools)
		if err != nil {
			return nil, fmt.Errorf("plan memory retrieval: %w", err)
		}
		if len(response.ToolCalls) == 0 {
			selected, err := parseRetrievalDecision(response.Content)
			if err != nil {
				return nil, err
			}
			result := make([]storage.Memory, 0, min(limit, len(selected.IDs)))
			seen := make(map[int64]bool)
			for _, id := range selected.IDs {
				item, ok := candidates[id]
				if !ok || seen[id] {
					continue
				}
				seen[id] = true
				result = append(result, item)
				if len(result) >= limit {
					break
				}
			}
			return result, nil
		}
		messages = append(messages, response)
		for _, call := range response.ToolCalls {
			output := "search_memories 调用次数已达到上限，请从已有结果选择并输出 JSON。"
			if searches < maxRetrievalSearches {
				searches++
				output = m.executeMemorySearch(ctx, call, limit, candidates)
			}
			messages = append(messages, llm.Message{Role: "tool", ToolCallID: call.ID, Content: output})
		}
		if searches >= maxRetrievalSearches {
			tools = nil
		}
	}
	return nil, fmt.Errorf("memory retrieval stopped after too many model rounds")
}

func (m *Manager) executeMemorySearch(ctx context.Context, call llm.ToolCall, limit int, candidates map[int64]storage.Memory) string {
	if call.Function.Name != "search_memories" {
		return "未知工具：" + call.Function.Name
	}
	var args struct {
		Query string `json:"query"`
	}
	if err := json.Unmarshal([]byte(call.Function.Arguments), &args); err != nil {
		return "search_memories 参数无效：" + err.Error()
	}
	query := strings.TrimSpace(args.Query)
	if query == "" {
		return "search_memories 参数无效：query 不能为空"
	}
	searchLimit := limit * 4
	if searchLimit < limit {
		searchLimit = limit
	}
	items, err := m.store.SearchMemories(ctx, query, searchLimit)
	if err != nil {
		return "search_memories 失败：" + err.Error()
	}
	for _, item := range items {
		candidates[item.ID] = item
	}
	raw, err := json.Marshal(items)
	if err != nil {
		return "search_memories 结果编码失败：" + err.Error()
	}
	return string(raw)
}

func memorySearchTool() llm.Tool {
	return llm.Tool{
		Type: "function",
		Function: llm.ToolDefinition{
			Name:        "search_memories",
			Description: "按关键词检索这个用户的长期记忆，返回可供本轮选择的候选记忆。",
			Parameters: map[string]any{
				"type": "object",
				"properties": map[string]any{
					"query": map[string]any{"type": "string", "description": "从当前消息提炼的简短检索词"},
				},
				"required":             []string{"query"},
				"additionalProperties": false,
			},
		},
	}
}

func (m *Manager) Review(ctx context.Context, user, assistant, sourceMessageID string, existing []storage.Memory) error {
	existingJSON, err := json.Marshal(existing)
	if err != nil {
		return err
	}
	message := fmt.Sprintf("已有相关记忆：\n%s\n\n本轮用户：\n%s\n\n本轮助手：\n%s", existingJSON, user, assistant)
	result, err := m.llm.Complete(ctx, []llm.Message{
		{Role: "system", Content: prompts.Memory},
		{Role: "user", Content: message},
	}, nil)
	if err != nil {
		return fmt.Errorf("review memories: %w", err)
	}
	parsed, err := parseDecision(result.Content)
	if err != nil {
		return err
	}
	allowed := make(map[int64]bool, len(existing))
	for _, item := range existing {
		allowed[item.ID] = true
	}
	for i, item := range parsed.Actions {
		if i >= 3 {
			break
		}
		switch strings.ToLower(item.Action) {
		case "add":
			if strings.TrimSpace(item.Content) != "" {
				if _, err := m.store.AddStructuredMemory(ctx, storage.Memory{
					Content: item.Content, Source: "auto", Kind: validKind(item.Kind), Importance: validImportance(item.Importance),
					Confidence: validConfidence(item.Confidence), SourceMessageID: sourceMessageID,
				}); err != nil {
					return err
				}
			}
		case "update":
			if allowed[item.ID] && strings.TrimSpace(item.Content) != "" && !memoryPinned(existing, item.ID) {
				if err := m.store.UpdateStructuredMemory(ctx, item.ID, storage.Memory{
					Content: item.Content, Kind: validKind(item.Kind), Importance: validImportance(item.Importance),
					Confidence: validConfidence(item.Confidence), SourceMessageID: sourceMessageID,
				}); err != nil {
					return err
				}
			}
		case "delete":
			if allowed[item.ID] && !memoryPinned(existing, item.ID) {
				if err := m.store.DeactivateMemory(ctx, item.ID); err != nil {
					return err
				}
			}
		}
	}
	return nil
}

func (m *Manager) Summarize(ctx context.Context, previous string, messages []storage.Message) (string, error) {
	raw, err := json.Marshal(messages)
	if err != nil {
		return "", err
	}
	input := "已有阶段摘要：\n" + previous + "\n\n新增对话：\n" + string(raw)
	result, err := m.llm.Complete(ctx, []llm.Message{{Role: "system", Content: prompts.Summary}, {Role: "user", Content: input}}, nil)
	if err != nil {
		return "", fmt.Errorf("summarize conversation: %w", err)
	}
	content := strings.TrimSpace(result.Content)
	if content == "" {
		return "", fmt.Errorf("conversation summary was empty")
	}
	return content, nil
}

func validKind(kind string) string {
	switch strings.ToLower(strings.TrimSpace(kind)) {
	case "preference", "person", "goal", "project", "decision", "relationship", "fact":
		return strings.ToLower(strings.TrimSpace(kind))
	default:
		return "fact"
	}
}

func validImportance(value int) int {
	if value < 1 || value > 5 {
		return 3
	}
	return value
}

func validConfidence(value float64) float64 {
	if value <= 0 || value > 1 {
		return 1
	}
	return value
}

func memoryPinned(items []storage.Memory, id int64) bool {
	for _, item := range items {
		if item.ID == id {
			return item.Pinned
		}
	}
	return false
}

func parseDecision(content string) (decision, error) {
	start := strings.Index(content, "{")
	end := strings.LastIndex(content, "}")
	if start < 0 || end < start {
		return decision{}, fmt.Errorf("memory review did not return JSON")
	}
	var result decision
	if err := json.Unmarshal([]byte(content[start:end+1]), &result); err != nil {
		return decision{}, fmt.Errorf("decode memory review: %w", err)
	}
	return result, nil
}

func parseRetrievalDecision(content string) (retrievalDecision, error) {
	start := strings.Index(content, "{")
	end := strings.LastIndex(content, "}")
	if start < 0 || end < start {
		return retrievalDecision{}, fmt.Errorf("memory retrieval did not return JSON")
	}
	var result retrievalDecision
	if err := json.Unmarshal([]byte(content[start:end+1]), &result); err != nil {
		return retrievalDecision{}, fmt.Errorf("decode memory retrieval: %w", err)
	}
	return result, nil
}
