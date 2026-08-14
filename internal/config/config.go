package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

type Config struct {
	DeepSeekAPIKey    string
	QQAppID           string
	QQAppSecret       string
	TavilyAPIKey      string
	SystemPrompt      string
	PersonaPrompt     string
	DatabasePath      string
	ListenAddr        string
	RecentMessages    int
	MaxMemories       int
	MaxToolCalls       int
	RequestTimeout    time.Duration
	MemoryQueueSize   int
	SummaryEvery      int
	BackupDir         string
	BackupInterval    time.Duration
	BackupRetention   time.Duration
	S3Bucket          string
	S3Prefix          string
	S3Region          string
	S3Endpoint        string
	S3AccessKeyID     string
	S3SecretAccessKey string
	S3SessionToken    string
}

func Load() (Config, error) {
	recentMessages, err := envInt("COMPANION_RECENT_MESSAGES", 20)
	if err != nil {
		return Config{}, err
	}
	maxMemories, err := envInt("COMPANION_MAX_MEMORIES", 5)
	if err != nil {
		return Config{}, err
	}
	maxToolCalls, err := envInt("COMPANION_MAX_TOOL_CALLS", 3)
	if err != nil {
		return Config{}, err
	}
	requestTimeout, err := envInt("COMPANION_REQUEST_TIMEOUT_SECONDS", 120)
	if err != nil {
		return Config{}, err
	}
	memoryQueueSize, err := envInt("COMPANION_MEMORY_QUEUE_SIZE", 32)
	if err != nil {
		return Config{}, err
	}
	summaryEvery, err := envInt("COMPANION_SUMMARY_EVERY", 20)
	if err != nil {
		return Config{}, err
	}
	backupIntervalHours, err := envInt("COMPANION_BACKUP_INTERVAL_HOURS", 24)
	if err != nil {
		return Config{}, err
	}
	backupRetentionDays, err := envInt("COMPANION_BACKUP_RETENTION_DAYS", 14)
	if err != nil {
		return Config{}, err
	}
	cfg := Config{
		DeepSeekAPIKey:    strings.TrimSpace(os.Getenv("DEEPSEEK_API_KEY")),
		QQAppID:           strings.TrimSpace(os.Getenv("QQ_APP_ID")),
		QQAppSecret:       strings.TrimSpace(os.Getenv("QQ_APP_SECRET")),
		TavilyAPIKey:      strings.TrimSpace(os.Getenv("TAVILY_API_KEY")),
		SystemPrompt:      promptEnv("system"),
		PersonaPrompt:     promptEnv("persona"),
		DatabasePath:      env("COMPANION_DB", "./data/companion.db"),
		ListenAddr:        env("COMPANION_ADDR", "127.0.0.1:8787"),
		RecentMessages:    recentMessages,
		MaxMemories:       maxMemories,
		MaxToolCalls:       maxToolCalls,
		RequestTimeout:    time.Duration(requestTimeout) * time.Second,
		MemoryQueueSize:   memoryQueueSize,
		SummaryEvery:      summaryEvery,
		BackupDir:         strings.TrimSpace(os.Getenv("COMPANION_BACKUP_DIR")),
		BackupInterval:    time.Duration(backupIntervalHours) * time.Hour,
		BackupRetention:   time.Duration(backupRetentionDays) * 24 * time.Hour,
		S3Bucket:          strings.TrimSpace(os.Getenv("S3_BUCKET")),
		S3Prefix:          env("S3_PREFIX", "mneme/backups"),
		S3Region:          s3Region(),
		S3Endpoint:        strings.TrimSpace(os.Getenv("S3_ENDPOINT")),
		S3AccessKeyID:     strings.TrimSpace(os.Getenv("S3_ACCESS_KEY_ID")),
		S3SecretAccessKey: strings.TrimSpace(os.Getenv("S3_SECRET_ACCESS_KEY")),
		S3SessionToken:    strings.TrimSpace(os.Getenv("S3_SESSION_TOKEN")),
	}
	if cfg.BackupDir == "" {
		cfg.BackupDir = filepath.Join(filepath.Dir(cfg.DatabasePath), "backups")
	}
	if cfg.DeepSeekAPIKey == "" {
		return Config{}, fmt.Errorf("DEEPSEEK_API_KEY must be set")
	}
	if cfg.SystemPrompt == "" || cfg.PersonaPrompt == "" {
		return Config{}, fmt.Errorf("system and persona must be set")
	}
	if cfg.RecentMessages < 0 || cfg.MaxMemories < 0 || cfg.MaxToolCalls < 0 || cfg.SummaryEvery < 0 {
		return Config{}, fmt.Errorf("COMPANION limits must not be negative")
	}
	if cfg.RequestTimeout <= 0 || cfg.MemoryQueueSize < 1 || cfg.BackupInterval <= 0 || cfg.BackupRetention <= 0 {
		return Config{}, fmt.Errorf("timeouts, queue size, and backup settings must be positive")
	}
	if (cfg.S3AccessKeyID == "") != (cfg.S3SecretAccessKey == "") {
		return Config{}, fmt.Errorf("S3_ACCESS_KEY_ID and S3_SECRET_ACCESS_KEY must be set together")
	}
	return cfg, nil
}

func s3Region() string {
	if value := strings.TrimSpace(os.Getenv("S3_REGION")); value != "" {
		return value
	}
	if value := strings.TrimSpace(os.Getenv("AWS_REGION")); value != "" {
		return value
	}
	return env("AWS_DEFAULT_REGION", "us-east-1")
}

func promptEnv(name string) string {
	value := strings.TrimSpace(os.Getenv(name))
	return strings.ReplaceAll(value, `\n`, "\n")
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

func envInt(name string, fallback int) (int, error) {
	value := strings.TrimSpace(os.Getenv(name))
	if value == "" {
		return fallback, nil
	}
	n, err := strconv.Atoi(value)
	if err != nil {
		return 0, fmt.Errorf("%s must be an integer: %w", name, err)
	}
	return n, nil
}
