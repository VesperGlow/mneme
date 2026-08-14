package main

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"net/http"
	"os"
	"os/signal"
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
	logger.Info("配置已加载", "mode", mode, "address", cfg.ListenAddr, "chat_model", llm.DeepSeekChatModel, "chat_effort", "max", "memory_model", llm.DeepSeekMemoryModel, "memory_effort", "high", "recent_messages", cfg.RecentMessages, "max_memories", cfg.MaxMemories, "max_tool_calls", cfg.MaxToolCalls, "memory_queue_capacity", cfg.MemoryQueueSize, "summary_every", cfg.SummaryEvery, "web_search", cfg.TavilyAPIKey != "", "open_url", cfg.TavilyAPIKey != "")
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
		runBackups(backupCtx, store, cfg.BackupDir, cfg.BackupInterval, cfg.BackupRetention, logger.Named("backup"))
	}()
	logger.Named("backup").Info("调度器已启动", "directory", cfg.BackupDir, "interval", cfg.BackupInterval, "retention", cfg.BackupRetention)
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

func runBackups(ctx context.Context, store *storage.Store, directory string, interval, retention time.Duration, logger *logging.Logger) {
	backup := func() {
		started := time.Now()
		requestCtx, cancel := context.WithTimeout(ctx, 10*time.Minute)
		defer cancel()
		path, err := store.BackupIfDue(requestCtx, directory, interval, retention)
		if err != nil && ctx.Err() == nil {
			logger.Warn("数据库备份失败", "duration", time.Since(started), "error", err)
		} else if path != "" {
			logger.Info("数据库备份已创建", "path", path, "duration", time.Since(started))
		} else if ctx.Err() == nil {
			logger.Debug("数据库备份已检查", "result", "not_due", "duration", time.Since(started))
		}
	}
	backup()
	ticker := time.NewTicker(interval)
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
