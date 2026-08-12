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
	Action  string `json:"action"`
	ID      int64  `json:"id,omitempty"`
	Content string `json:"content,omitempty"`
}

type decision struct {
	Actions []action `json:"actions"`
}

func New(store *storage.Store, client *llm.Client) *Manager {
	return &Manager{store: store, llm: client}
}

func (m *Manager) Review(ctx context.Context, user, assistant string, existing []storage.Memory) error {
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
				if _, err := m.store.AddMemory(ctx, item.Content, "auto"); err != nil {
					return err
				}
			}
		case "update":
			if allowed[item.ID] && strings.TrimSpace(item.Content) != "" {
				if err := m.store.UpdateMemory(ctx, item.ID, item.Content); err != nil {
					return err
				}
			}
		case "delete":
			if allowed[item.ID] {
				if err := m.store.DeactivateMemory(ctx, item.ID); err != nil {
					return err
				}
			}
		}
	}
	return nil
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
