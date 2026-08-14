package main

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"companion/internal/agent"
	"companion/internal/config"
	"companion/internal/httpapi"
	"companion/internal/llm"
	"companion/internal/logging"
	"companion/internal/memory"
	"companion/internal/qqbot"
	"companion/internal/s3backup"
	"companion/internal/search"
	"companion/internal/storage"
)

var buildCommit = "dev"

func main() {
	if err := run(); err != nil {
		logger, loggerErr := logging.NewFromEnv(os.Stderr)
		if loggerErr != nil {
			logger = logging.New(os.Stderr, logging.Options{})
		}
		logger.Named("app").Error("启动失败", "error", err)
		os.Exit(1)
	}
}

func run() error {
	if len(os.Args) != 2 || (os.Args[1] != "chat" && os.Args[1] != "serve") {
		return fmt.Errorf("usage: companion <chat|serve>")
	}
	logger, err := logging.NewFromEnv(os.Stderr)
	if err != nil {
		return err
	}
	logger = logger.Named("app")
	mode := os.Args[1]
	logger.Info("启动开始", "mode", mode, "commit", shortCommit(buildCommit))
	cfg, err := config.Load()
	if err != nil {
		return err
	}
	logger.Info("配置已加载", "mode", mode, "address", cfg.ListenAddr, "chat_model", llm.DeepSeekChatModel, "chat_effort", "max", "memory_model", llm.DeepSeekMemoryModel, "memory_effort", "high", "recent_messages", cfg.RecentMessages, "max_memories", cfg.MaxMemories, "max_tool_calls", cfg.MaxToolCalls, "memory_queue_capacity", cfg.MemoryQueueSize, "summary_every", cfg.SummaryEvery, "web_search", cfg.TavilyAPIKey != "", "open_url", cfg.TavilyAPIKey != "", "s3_backup", cfg.S3Bucket != "")
	var remoteBackups *s3backup.Store
	if cfg.S3Bucket != "" {
		remoteBackups, err = s3backup.New(context.Background(), s3backup.Config{
			Bucket: cfg.S3Bucket, Prefix: cfg.S3Prefix, Region: cfg.S3Region, Endpoint: cfg.S3Endpoint,
			AccessKeyID: cfg.S3AccessKeyID, SecretAccessKey: cfg.S3SecretAccessKey, SessionToken: cfg.S3SessionToken,
		})
		if err != nil {
			return err
		}
		restoreStarted := time.Now()
		restoreCtx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
		restored, key, restoreErr := remoteBackups.RestoreIfMissing(restoreCtx, cfg.DatabasePath)
		cancel()
		if restoreErr != nil {
			return fmt.Errorf("restore database from S3: %w", restoreErr)
		}
		if restored {
			logger.Named("backup").Info("数据库已从 S3 恢复", "key", key, "duration", time.Since(restoreStarted))
		} else {
			logger.Named("backup").Info("S3 恢复已检查", "result", "local_exists_or_remote_empty", "duration", time.Since(restoreStarted))
		}
	}
	databaseStarted := time.Now()
	store, err := storage.Open(cfg.DatabasePath)
	if err != nil {
		return err
	}
	defer func() {
		if err := store.Close(); err != nil {
			logger.Warn("数据库关闭失败", "error", err)
		}
	}()
	if err := store.IntegrityCheck(context.Background()); err != nil {
		return err
	}
	logger.Named("storage").Info("数据库就绪", "path", cfg.DatabasePath, "integrity", "ok", "duration", time.Since(databaseStarted))
	httpClient := &http.Client{Timeout: cfg.RequestTimeout}
	llmClient := llm.New(cfg.DeepSeekAPIKey, httpClient)
	memoryLLMClient := llm.NewMemory(cfg.DeepSeekAPIKey, httpClient)
	searchClient := search.New(cfg.TavilyAPIKey, httpClient)
	memoryManager := memory.New(store, memoryLLMClient)
	companion := agent.NewWithOptions(store, llmClient, searchClient, memoryManager, cfg.SystemPrompt, cfg.PersonaPrompt, agent.Options{
		RecentMessages: cfg.RecentMessages, MaxMemories: cfg.MaxMemories, MaxToolCalls: cfg.MaxToolCalls,
		MemoryQueueSize: cfg.MemoryQueueSize, SummaryEvery: cfg.SummaryEvery,
	}, logger.Named("agent"))
	defer func() {
		logger.Named("agent").Info("关闭开始", "memory_jobs", "draining")
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
		defer cancel()
		if err := companion.Close(ctx); err != nil {
			logger.Named("agent").Warn("关闭失败", "error", err)
		} else {
			logger.Named("agent").Info("关闭完成")
		}
	}()
	logger.Named("agent").Info("就绪", "memory_worker", "running")
	backupCtx, stopBackups := context.WithCancel(context.Background())
	var backupWG sync.WaitGroup
	backupWG.Add(1)
	go func() {
		defer backupWG.Done()
		runBackups(backupCtx, store, remoteBackups, cfg.BackupDir, cfg.BackupInterval, cfg.BackupRetention, logger.Named("backup"))
	}()
	logger.Named("backup").Info("调度器已启动", "directory", cfg.BackupDir, "interval", cfg.BackupInterval, "retention", cfg.BackupRetention, "s3", remoteBackups != nil)
	defer func() {
		stopBackups()
		backupWG.Wait()
	}()

	switch os.Args[1] {
	case "chat":
		return runChat(companion)
	case "serve":
		if err := cfg.ValidateQQ(); err != nil {
			return err
		}
		qq := qqbot.New(cfg.QQAppID, cfg.QQAppSecret, companion, logger.Named("qq"))
		return runServer(cfg.ListenAddr, companion, store, qq, logger)
	default:
		return nil
	}
}

func runChat(companion *agent.Agent) error {
	fmt.Println("Companion 已启动。输入 /memories 查看记忆，Ctrl+D 退出。")
	scanner := bufio.NewScanner(os.Stdin)
	scanner.Buffer(make([]byte, 1024), 1024*1024)
	for {
		fmt.Print("You > ")
		if !scanner.Scan() {
			fmt.Println()
			return scanner.Err()
		}
		input := strings.TrimSpace(scanner.Text())
		if input == "" {
			continue
		}
		reply, err := companion.ChatInput(context.Background(), agent.Input{Channel: "cli", Content: input, ReceivedAt: time.Now()})
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error > %v\n", err)
			continue
		}
		fmt.Printf("Agent > %s\n\n", reply)
	}
}

func runBackups(ctx context.Context, store *storage.Store, remote *s3backup.Store, directory string, interval, retention time.Duration, logger *logging.Logger) {
	lastUploaded := ""
	backup := func() {
		started := time.Now()
		requestCtx, cancel := context.WithTimeout(ctx, 10*time.Minute)
		defer cancel()
		backupPath, err := store.BackupIfDue(requestCtx, directory, interval, retention)
		if err != nil && ctx.Err() == nil {
			logger.Warn("数据库备份失败", "duration", time.Since(started), "error", err)
			return
		} else if backupPath != "" {
			logger.Info("数据库备份已创建", "path", backupPath, "duration", time.Since(started))
		} else if ctx.Err() == nil {
			logger.Debug("数据库备份已检查", "result", "not_due", "duration", time.Since(started))
		}
		if remote == nil || ctx.Err() != nil {
			return
		}
		if backupPath == "" && lastUploaded == "" {
			backupPath, err = newestLocalBackup(directory)
			if err != nil {
				logger.Warn("查找本地备份失败", "error", err)
				return
			}
		}
		if backupPath == "" || backupPath == lastUploaded {
			return
		}
		key, err := remote.Upload(requestCtx, backupPath)
		if err != nil {
			if ctx.Err() == nil {
				logger.Warn("S3 备份上传失败", "path", backupPath, "duration", time.Since(started), "error", err)
			}
			return
		}
		lastUploaded = backupPath
		logger.Info("数据库备份已上传到 S3", "path", backupPath, "key", key, "duration", time.Since(started))
	}
	backup()
	checkInterval := interval
	if remote != nil && checkInterval > 15*time.Minute {
		checkInterval = 15 * time.Minute
	}
	ticker := time.NewTicker(checkInterval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			backup()
		}
	}
}

func newestLocalBackup(directory string) (string, error) {
	entries, err := os.ReadDir(directory)
	if err != nil {
		return "", err
	}
	var newestPath string
	var newestTime time.Time
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasPrefix(entry.Name(), "mneme-") || !strings.HasSuffix(entry.Name(), ".db") {
			continue
		}
		info, err := entry.Info()
		if err != nil {
			continue
		}
		if info.ModTime().After(newestTime) {
			newestPath = filepath.Join(directory, entry.Name())
			newestTime = info.ModTime()
		}
	}
	return newestPath, nil
}

func runServer(addr string, companion *agent.Agent, store *storage.Store, qq *qqbot.Bot, logger *logging.Logger) error {
	server := &http.Server{
		Addr:              addr,
		Handler:           httpapi.New(companion, store, logger.Named("http")),
		ReadHeaderTimeout: 5 * time.Second,
		IdleTimeout:       60 * time.Second,
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	errCh := make(chan error, 2)
	go func() {
		logger.Named("http").Info("服务已启动", "address", "http://"+addr)
		if err := server.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			errCh <- fmt.Errorf("HTTP server: %w", err)
		}
	}()
	go func() {
		err := qq.Run(ctx)
		if ctx.Err() != nil {
			return
		}
		if err == nil {
			err = fmt.Errorf("QQ websocket stopped unexpectedly")
		}
		errCh <- err
	}()

	var runErr error
	select {
	case <-ctx.Done():
		logger.Info("收到关闭请求", "reason", ctx.Err())
	case runErr = <-errCh:
		logger.Error("服务组件失败", "error", runErr)
		stop()
	}
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := server.Shutdown(shutdownCtx); err != nil {
		logger.Named("http").Warn("服务关闭失败", "error", err)
	} else {
		logger.Named("http").Info("服务已停止")
	}
	return runErr
}

func shortCommit(value string) string {
	value = strings.TrimSpace(value)
	if len(value) > 12 {
		return value[:12]
	}
	if value == "" {
		return "unknown"
	}
	return value
}
