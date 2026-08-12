package agent

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"strings"
	"sync"
	"time"

	"companion/internal/llm"
	"companion/internal/memory"
	"companion/internal/search"
	"companion/internal/storage"
	"companion/prompts"
)

type Agent struct {
	store          *storage.Store
	llm            *llm.Client
	search         *search.Client
	memories       *memory.Manager
	recentMessages int
	maxMemories    int
	maxToolCalls   int
	logger         *log.Logger
	mu             sync.Mutex
}

func New(store *storage.Store, llmClient *llm.Client, searchClient *search.Client, memoryManager *memory.Manager, recentMessages, maxMemories, maxToolCalls int, logger *log.Logger) *Agent {
	return &Agent{
		store:          store,
		llm:            llmClient,
		search:         searchClient,
		memories:       memoryManager,
		recentMessages: recentMessages,
		maxMemories:    maxMemories,
		maxToolCalls:   maxToolCalls,
		logger:         logger,
	}
}

func (a *Agent) Chat(ctx context.Context, input string) (string, error) {
	a.mu.Lock()
	defer a.mu.Unlock()

	input = strings.TrimSpace(input)
	if input == "" {
		return "", fmt.Errorf("message is empty")
	}
	if strings.HasPrefix(input, "/") {
		return a.command(ctx, input)
	}
	relevant, err := a.store.SearchMemories(ctx, input, a.maxMemories)
	if err != nil {
		return "", err
	}
	recent, err := a.store.RecentMessages(ctx, a.recentMessages)
	if err != nil {
		return "", err
	}
	messages := make([]llm.Message, 0, len(recent)+3)
	system := prompts.System + "\n\n当前日期：" + time.Now().Format("2006-01-02")
	if len(relevant) > 0 {
		system += "\n\n可能相关的长期记忆（只在确实相关时使用）：\n" + formatMemoryContext(relevant)
	}
	messages = append(messages, llm.Message{Role: "system", Content: system})
	for _, item := range recent {
		messages = append(messages, llm.Message{Role: item.Role, Content: item.Content})
	}
	messages = append(messages, llm.Message{Role: "user", Content: input})

	reply, err := a.runLoop(ctx, messages)
	if err != nil {
		return "", err
	}
	if err := a.store.AddExchange(ctx, input, reply); err != nil {
		return "", err
	}
	if a.memories != nil {
		if err := a.memories.Review(ctx, input, reply, relevant); err != nil && a.logger != nil {
			a.logger.Printf("memory review skipped: %v", err)
		}
	}
	return reply, nil
}

func (a *Agent) runLoop(ctx context.Context, messages []llm.Message) (string, error) {
	usedTools := 0
	tools := []llm.Tool{webSearchTool()}
	maxRounds := a.maxToolCalls + 2
	if maxRounds < 2 {
		maxRounds = 2
	}
	for round := 0; round < maxRounds; round++ {
		response, err := a.llm.Complete(ctx, messages, tools)
		if err != nil {
			return "", err
		}
		if len(response.ToolCalls) == 0 {
			if strings.TrimSpace(response.Content) == "" {
				return "", fmt.Errorf("LLM returned an empty response")
			}
			return response.Content, nil
		}
		messages = append(messages, response)
		for _, call := range response.ToolCalls {
			var output string
			if usedTools >= a.maxToolCalls {
				output = "工具调用次数已达到上限，请基于已有信息回答，并说明无法继续搜索。"
			} else {
				usedTools++
				output = a.executeTool(ctx, call)
			}
			messages = append(messages, llm.Message{Role: "tool", ToolCallID: call.ID, Content: output})
		}
		if usedTools >= a.maxToolCalls {
			tools = nil
		}
	}
	return "", fmt.Errorf("agent stopped after %d model rounds", maxRounds)
}

func (a *Agent) executeTool(ctx context.Context, call llm.ToolCall) string {
	if call.Function.Name != "web_search" {
		return "未知工具：" + call.Function.Name
	}
	var args struct {
		Query string `json:"query"`
	}
	if err := json.Unmarshal([]byte(call.Function.Arguments), &args); err != nil {
		return "web_search 参数无效：" + err.Error()
	}
	if a.search == nil {
		return "web_search 不可用：未配置搜索客户端"
	}
	result, err := a.search.Search(ctx, args.Query)
	if err != nil {
		return "web_search 失败：" + err.Error()
	}
	raw, err := json.Marshal(result)
	if err != nil {
		return "web_search 结果编码失败：" + err.Error()
	}
	return string(raw)
}

func webSearchTool() llm.Tool {
	return llm.Tool{
		Type: "function",
		Function: llm.ToolDefinition{
			Name:        "web_search",
			Description: "搜索互联网。仅用于用户明确要求搜索、实时新闻/价格/版本等可能变化的信息，或不确定的外部事实；普通聊天不要调用。",
			Parameters: map[string]any{
				"type": "object",
				"properties": map[string]any{
					"query": map[string]any{"type": "string", "description": "简洁、具体的搜索词"},
				},
				"required":             []string{"query"},
				"additionalProperties": false,
			},
		},
	}
}

func formatMemoryContext(items []storage.Memory) string {
	var builder strings.Builder
	for _, item := range items {
		fmt.Fprintf(&builder, "- [id=%d] %s\n", item.ID, item.Content)
	}
	return strings.TrimSpace(builder.String())
}

func (a *Agent) command(ctx context.Context, input string) (string, error) {
	name, value, _ := strings.Cut(input, " ")
	value = strings.TrimSpace(value)
	switch name {
	case "/remember":
		if value == "" {
			return "用法：/remember 要记住的内容", nil
		}
		item, err := a.store.AddMemory(ctx, value, "manual")
		if err != nil {
			return "", err
		}
		return fmt.Sprintf("已记住（#%d）：%s", item.ID, item.Content), nil
	case "/memories":
		items, err := a.store.ListMemories(ctx, 100, false)
		if err != nil {
			return "", err
		}
		if len(items) == 0 {
			return "目前没有长期记忆。", nil
		}
		var builder strings.Builder
		builder.WriteString("长期记忆：\n")
		for _, item := range items {
			fmt.Fprintf(&builder, "#%d %s\n", item.ID, item.Content)
		}
		return strings.TrimSpace(builder.String()), nil
	case "/forget":
		if value == "" {
			return "用法：/forget 要忘记的内容", nil
		}
		n, err := a.store.Forget(ctx, value, 5)
		if err != nil {
			return "", err
		}
		if n == 0 {
			return "没有找到相关的长期记忆。", nil
		}
		return fmt.Sprintf("已将 %d 条相关记忆标记为失效。", n), nil
	default:
		return "未知命令。可用命令：/remember、/memories、/forget", nil
	}
}
