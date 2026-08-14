package main

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"log"
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
	"companion/internal/memory"
	"companion/internal/qqbot"
	"companion/internal/search"
	"companion/internal/storage"
)

var buildCommit = "dev"

func main() {
	if err := run(); err != nil {
		log.Printf("error: %v", err)
		os.Exit(1)
	}
}

func run() error {
	if len(os.Args) != 2 || (os.Args[1] != "chat" && os.Args[1] != "serve") {
		return fmt.Errorf("usage: companion <chat|serve>")
	}
	logger := log.New(os.Stderr, "companion: ", log.LstdFlags|log.Lmicroseconds)
	mode := os.Args[1]
	logger.Printf("level=info event=startup_started mode=%q commit=%q", mode, shortCommit(buildCommit))
	cfg, err := config.Load()
	if err != nil {
		return err
	}
	logger.Printf("level=info event=config_loaded mode=%q listen=%q chat_model=%q chat_effort=max memory_model=%q memory_effort=high recent_messages=%d max_memories=%d max_tool_calls=%d memory_queue=%d summary_every=%d web_search=%t", mode, cfg.ListenAddr, llm.DeepSeekChatModel, llm.DeepSeekMemoryModel, cfg.RecentMessages, cfg.MaxMemories, cfg.MaxToolCalls, cfg.MemoryQueueSize, cfg.SummaryEvery, cfg.TavilyAPIKey != "")
	databaseStarted := time.Now()
	store, err := storage.Open(cfg.DatabasePath)
	if err != nil {
		return err
	}
	defer func() {
		if err := store.Close(); err != nil {
			logger.Printf("level=warn event=database_close_failed error=%q", err)
		}
	}()
	if err := store.IntegrityCheck(context.Background()); err != nil {
		return err
	}
	logger.Printf("level=info event=database_ready path=%q integrity=ok duration_ms=%d", cfg.DatabasePath, time.Since(databaseStarted).Milliseconds())
	httpClient := &http.Client{Timeout: cfg.RequestTimeout}
	llmClient := llm.New(cfg.DeepSeekAPIKey, httpClient)
	memoryLLMClient := llm.NewMemory(cfg.DeepSeekAPIKey, httpClient)
	searchClient := search.New(cfg.TavilyAPIKey, httpClient)
	memoryManager := memory.New(store, memoryLLMClient)
	companion := agent.NewWithOptions(store, llmClient, searchClient, memoryManager, cfg.SystemPrompt, cfg.PersonaPrompt, agent.Options{
		RecentMessages: cfg.RecentMessages, MaxMemories: cfg.MaxMemories, MaxToolCalls: cfg.MaxToolCalls,
		MemoryQueueSize: cfg.MemoryQueueSize, SummaryEvery: cfg.SummaryEvery,
	}, logger)
	defer func() {
		logger.Printf("level=info event=agent_shutdown_started pending_memory_jobs=draining")
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
		defer cancel()
		if err := companion.Close(ctx); err != nil {
			logger.Printf("level=warn event=agent_shutdown_failed error=%q", err)
		} else {
			logger.Printf("level=info event=agent_shutdown_completed")
		}
	}()
	logger.Printf("level=info event=agent_ready memory_worker=running")
	backupCtx, stopBackups := context.WithCancel(context.Background())
	var backupWG sync.WaitGroup
	backupWG.Add(1)
	go func() {
		defer backupWG.Done()
		runBackups(backupCtx, store, cfg.BackupDir, cfg.BackupInterval, cfg.BackupRetention, logger)
	}()
	logger.Printf("level=info event=backup_scheduler_started directory=%q interval=%q retention=%q", cfg.BackupDir, cfg.BackupInterval, cfg.BackupRetention)
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
		qq := qqbot.New(cfg.QQAppID, cfg.QQAppSecret, companion, logger)
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

func runBackups(ctx context.Context, store *storage.Store, directory string, interval, retention time.Duration, logger *log.Logger) {
	backup := func() {
		started := time.Now()
		requestCtx, cancel := context.WithTimeout(ctx, 10*time.Minute)
		defer cancel()
		path, err := store.BackupIfDue(requestCtx, directory, interval, retention)
		if err != nil && ctx.Err() == nil {
			logger.Printf("level=warn event=database_backup_failed duration_ms=%d error=%q", time.Since(started).Milliseconds(), err)
		} else if path != "" {
			logger.Printf("level=info event=database_backup_created path=%q duration_ms=%d", path, time.Since(started).Milliseconds())
		} else if ctx.Err() == nil {
			logger.Printf("level=info event=database_backup_checked outcome=not_due duration_ms=%d", time.Since(started).Milliseconds())
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

func runServer(addr string, companion *agent.Agent, store *storage.Store, qq *qqbot.Bot, logger *log.Logger) error {
	server := &http.Server{
		Addr:              addr,
		Handler:           httpapi.New(companion, store, logger),
		ReadHeaderTimeout: 5 * time.Second,
		IdleTimeout:       60 * time.Second,
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	errCh := make(chan error, 2)
	go func() {
		logger.Printf("level=info event=http_server_started address=%q", "http://"+addr)
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
		logger.Printf("level=info event=shutdown_requested reason=%q", ctx.Err())
	case runErr = <-errCh:
		logger.Printf("level=error event=service_component_failed error=%q", runErr)
		stop()
	}
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := server.Shutdown(shutdownCtx); err != nil {
		logger.Printf("level=warn event=http_server_shutdown_failed error=%q", err)
	} else {
		logger.Printf("level=info event=http_server_stopped")
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
