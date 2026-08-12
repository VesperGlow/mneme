package config

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"
)

type Config struct {
	LLMBaseURL     string
	LLMAPIKey      string
	LLMModel       string
	TavilyAPIKey   string
	DatabasePath   string
	ListenAddr     string
	RecentMessages int
	MaxMemories    int
	MaxToolCalls   int
	RequestTimeout time.Duration
}

func Load() (Config, error) {
	cfg := Config{
		LLMBaseURL:     strings.TrimSpace(os.Getenv("LLM_BASE_URL")),
		LLMAPIKey:      strings.TrimSpace(os.Getenv("LLM_API_KEY")),
		LLMModel:       strings.TrimSpace(os.Getenv("LLM_MODEL")),
		TavilyAPIKey:   strings.TrimSpace(os.Getenv("TAVILY_API_KEY")),
		DatabasePath:   env("COMPANION_DB", "./data/companion.db"),
		ListenAddr:     env("COMPANION_ADDR", "127.0.0.1:8787"),
		RecentMessages: envInt("COMPANION_RECENT_MESSAGES", 20),
		MaxMemories:    envInt("COMPANION_MAX_MEMORIES", 5),
		MaxToolCalls:   envInt("COMPANION_MAX_TOOL_CALLS", 3),
		RequestTimeout: time.Duration(envInt("COMPANION_REQUEST_TIMEOUT_SECONDS", 120)) * time.Second,
	}
	if cfg.LLMBaseURL == "" || cfg.LLMAPIKey == "" || cfg.LLMModel == "" {
		return Config{}, fmt.Errorf("LLM_BASE_URL, LLM_API_KEY and LLM_MODEL must be set")
	}
	if cfg.RecentMessages < 0 || cfg.MaxMemories < 0 || cfg.MaxToolCalls < 0 {
		return Config{}, fmt.Errorf("COMPANION limits must not be negative")
	}
	return cfg, nil
}

func env(name, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(name)); value != "" {
		return value
	}
	return fallback
}

func envInt(name string, fallback int) int {
	value := strings.TrimSpace(os.Getenv(name))
	if value == "" {
		return fallback
	}
	n, err := strconv.Atoi(value)
	if err != nil {
		return fallback
	}
	return n
}
