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

func New(store *storage.Store, client *llm.Client) *Manager {
	return &Manager{store: store, llm: client}
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
