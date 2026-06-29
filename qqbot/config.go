package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"
)

const (
	EventModeWebhook   = "webhook"
	EventModeWebSocket = "websocket"
)

type Config struct {
	AppID           string
	AppSecret       string
	EventMode       string
	ListenAddr      string
	WebhookPath     string
	AIURL           string
	AIAPIKey        string
	AITimeout       time.Duration
	OpenAPITimeout  time.Duration
	Debug           bool
	ReplyMaxRunes   int
	ReplyMaxParts   int
	Workers         int
	QueueSize       int
	DedupTTL        time.Duration
	MaxWebhookBytes int64
}

func LoadConfig() (Config, error) {
	cfg := Config{
		AppID:           strings.TrimSpace(os.Getenv("QQ_APP_ID")),
		AppSecret:       strings.TrimSpace(os.Getenv("QQ_APP_SECRET")),
		EventMode:       strings.ToLower(envString("QQ_EVENT_MODE", EventModeWebhook)),
		ListenAddr:      envString("QQ_LISTEN_ADDR", ":9000"),
		WebhookPath:     envString("QQ_WEBHOOK_PATH", "/qqbot"),
		AIURL:           envString("QQ_AI_URL", "http://app:8000/v1/chat"),
		AIAPIKey:        strings.TrimSpace(os.Getenv("APP_API_KEY")),
		AITimeout:       time.Duration(envInt("QQ_AI_TIMEOUT_SECONDS", 180, 5, 600)) * time.Second,
		OpenAPITimeout:  time.Duration(envInt("QQ_OPENAPI_TIMEOUT_SECONDS", 15, 5, 60)) * time.Second,
		Debug:           envBool("QQ_BOT_DEBUG", false),
		ReplyMaxRunes:   envInt("QQ_REPLY_MAX_RUNES", 1800, 200, 10000),
		ReplyMaxParts:   envInt("QQ_REPLY_MAX_PARTS", 4, 1, 5),
		Workers:         envInt("QQ_WORKERS", 8, 1, 64),
		QueueSize:       envInt("QQ_QUEUE_SIZE", 128, 1, 10000),
		DedupTTL:        time.Duration(envInt("QQ_DEDUP_TTL_SECONDS", 600, 60, 86400)) * time.Second,
		MaxWebhookBytes: int64(envInt("QQ_MAX_WEBHOOK_BYTES", 1048576, 4096, 10485760)),
	}
	if cfg.AppID == "" || cfg.AppSecret == "" {
		return Config{}, fmt.Errorf("QQ_APP_ID 和 QQ_APP_SECRET 不能为空")
	}
	if cfg.EventMode != EventModeWebhook && cfg.EventMode != EventModeWebSocket {
		return Config{}, fmt.Errorf("QQ_EVENT_MODE 必须是 webhook 或 websocket")
	}
	if !strings.HasPrefix(cfg.WebhookPath, "/") {
		cfg.WebhookPath = "/" + cfg.WebhookPath
	}
	if !strings.HasPrefix(cfg.AIURL, "http://") && !strings.HasPrefix(cfg.AIURL, "https://") {
		return Config{}, fmt.Errorf("QQ_AI_URL 必须是 http:// 或 https:// 地址")
	}
	return cfg, nil
}

func envString(name, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(name)); value != "" {
		return value
	}
	return fallback
}

func envBool(name string, fallback bool) bool {
	value := strings.TrimSpace(os.Getenv(name))
	if value == "" {
		return fallback
	}
	parsed, err := strconv.ParseBool(value)
	if err != nil {
		return fallback
	}
	return parsed
}

func envInt(name string, fallback, minValue, maxValue int) int {
	value, err := strconv.Atoi(strings.TrimSpace(os.Getenv(name)))
	if err != nil {
		return fallback
	}
	if value < minValue {
		return minValue
	}
	if value > maxValue {
		return maxValue
	}
	return value
}
