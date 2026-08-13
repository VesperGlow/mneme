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
	cfg, err := config.Load()
	if err != nil {
		return err
	}
	store, err := storage.Open(cfg.DatabasePath)
	if err != nil {
		return err
	}
	defer store.Close()
	if err := store.IntegrityCheck(context.Background()); err != nil {
		return err
	}
	httpClient := &http.Client{Timeout: cfg.RequestTimeout}
	llmClient := llm.New(cfg.DeepSeekAPIKey, httpClient)
	memoryLLMClient := llm.NewMemory(cfg.DeepSeekAPIKey, httpClient)
	searchClient := search.New(cfg.TavilyAPIKey, httpClient)
	memoryManager := memory.New(store, memoryLLMClient)
	logger := log.New(os.Stderr, "companion: ", log.LstdFlags)
	companion := agent.NewWithOptions(store, llmClient, searchClient, memoryManager, cfg.SystemPrompt, cfg.PersonaPrompt, agent.Options{
		RecentMessages: cfg.RecentMessages, MaxMemories: cfg.MaxMemories, MaxToolCalls: cfg.MaxToolCalls,
		MemoryQueueSize: cfg.MemoryQueueSize, SummaryEvery: cfg.SummaryEvery,
	}, logger)
	defer func() {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
		defer cancel()
		if err := companion.Close(ctx); err != nil {
			logger.Printf("agent shutdown: %v", err)
		}
	}()
	backupCtx, stopBackups := context.WithCancel(context.Background())
	var backupWG sync.WaitGroup
	backupWG.Add(1)
	go func() {
		defer backupWG.Done()
		runBackups(backupCtx, store, cfg.BackupDir, cfg.BackupInterval, cfg.BackupRetention, logger)
	}()
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
		requestCtx, cancel := context.WithTimeout(ctx, 10*time.Minute)
		defer cancel()
		path, err := store.BackupIfDue(requestCtx, directory, interval, retention)
		if err != nil && ctx.Err() == nil {
			logger.Printf("database backup: %v", err)
		} else if path != "" {
			logger.Printf("database backup created: %s", path)
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
		Handler:           httpapi.New(companion, store),
		ReadHeaderTimeout: 5 * time.Second,
		IdleTimeout:       60 * time.Second,
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	errCh := make(chan error, 2)
	go func() {
		logger.Printf("HTTP API listening on http://%s", addr)
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
	case runErr = <-errCh:
		stop()
	}
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := server.Shutdown(shutdownCtx); err != nil {
		logger.Printf("server shutdown: %v", err)
	}
	return runErr
}
