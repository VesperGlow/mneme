package config

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"
)

type Config struct {
	DeepSeekAPIKey string
	QQAppID        string
	QQAppSecret    string
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
		DeepSeekAPIKey: strings.TrimSpace(os.Getenv("DEEPSEEK_API_KEY")),
		QQAppID:        strings.TrimSpace(os.Getenv("QQ_APP_ID")),
		QQAppSecret:    strings.TrimSpace(os.Getenv("QQ_APP_SECRET")),
		TavilyAPIKey:   strings.TrimSpace(os.Getenv("TAVILY_API_KEY")),
		DatabasePath:   env("COMPANION_DB", "./data/companion.db"),
		ListenAddr:     env("COMPANION_ADDR", "127.0.0.1:8787"),
		RecentMessages: envInt("COMPANION_RECENT_MESSAGES", 20),
		MaxMemories:    envInt("COMPANION_MAX_MEMORIES", 5),
		MaxToolCalls:   envInt("COMPANION_MAX_TOOL_CALLS", 3),
		RequestTimeout: time.Duration(envInt("COMPANION_REQUEST_TIMEOUT_SECONDS", 120)) * time.Second,
	}
	if cfg.DeepSeekAPIKey == "" || cfg.TavilyAPIKey == "" {
		return Config{}, fmt.Errorf("DEEPSEEK_API_KEY and TAVILY_API_KEY must be set")
	}
	if cfg.RecentMessages < 0 || cfg.MaxMemories < 0 || cfg.MaxToolCalls < 0 {
		return Config{}, fmt.Errorf("COMPANION limits must not be negative")
	}
	return cfg, nil
}

func (c Config) ValidateQQ() error {
	if c.QQAppID == "" || c.QQAppSecret == "" {
		return fmt.Errorf("QQ_APP_ID and QQ_APP_SECRET must be set for serve mode")
	}
	return nil
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
