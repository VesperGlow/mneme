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
		log.Printf("错误：%v", err)
		os.Exit(1)
	}
}

func run() error {
	if len(os.Args) != 2 || (os.Args[1] != "chat" && os.Args[1] != "serve") {
		return fmt.Errorf("usage: companion <chat|serve>")
	}
	logger := log.New(os.Stderr, "companion: ", log.LstdFlags|log.Lmicroseconds)
	mode := os.Args[1]
	logger.Printf("级别=信息 事件=启动开始 模式=%q 提交=%q", mode, shortCommit(buildCommit))
	cfg, err := config.Load()
	if err != nil {
		return err
	}
	logger.Printf("级别=信息 事件=配置已加载 模式=%q 监听地址=%q 聊天模型=%q 聊天推理强度=max 记忆模型=%q 记忆推理强度=high 最近消息数=%d 最大记忆数=%d 最大工具调用数=%d 记忆队列容量=%d 摘要间隔轮数=%d 网页搜索=%t 打开链接=%t", mode, cfg.ListenAddr, llm.DeepSeekChatModel, llm.DeepSeekMemoryModel, cfg.RecentMessages, cfg.MaxMemories, cfg.MaxToolCalls, cfg.MemoryQueueSize, cfg.SummaryEvery, cfg.TavilyAPIKey != "", cfg.TavilyAPIKey != "")
	databaseStarted := time.Now()
	store, err := storage.Open(cfg.DatabasePath)
	if err != nil {
		return err
	}
	defer func() {
		if err := store.Close(); err != nil {
			logger.Printf("级别=警告 事件=数据库关闭失败 错误=%q", err)
		}
	}()
	if err := store.IntegrityCheck(context.Background()); err != nil {
		return err
	}
	logger.Printf("级别=信息 事件=数据库就绪 路径=%q 完整性=正常 耗时毫秒=%d", cfg.DatabasePath, time.Since(databaseStarted).Milliseconds())
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
		logger.Printf("级别=信息 事件=Agent关闭开始 待处理记忆任务=正在清空")
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
		defer cancel()
		if err := companion.Close(ctx); err != nil {
			logger.Printf("级别=警告 事件=Agent关闭失败 错误=%q", err)
		} else {
			logger.Printf("级别=信息 事件=Agent关闭完成")
		}
	}()
	logger.Printf("级别=信息 事件=Agent就绪 记忆工作器=运行中")
	backupCtx, stopBackups := context.WithCancel(context.Background())
	var backupWG sync.WaitGroup
	backupWG.Add(1)
	go func() {
		defer backupWG.Done()
		runBackups(backupCtx, store, cfg.BackupDir, cfg.BackupInterval, cfg.BackupRetention, logger)
	}()
	logger.Printf("级别=信息 事件=备份调度器已启动 目录=%q 间隔=%q 保留期=%q", cfg.BackupDir, cfg.BackupInterval, cfg.BackupRetention)
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
			logger.Printf("级别=警告 事件=数据库备份失败 耗时毫秒=%d 错误=%q", time.Since(started).Milliseconds(), err)
		} else if path != "" {
			logger.Printf("级别=信息 事件=数据库备份已创建 路径=%q 耗时毫秒=%d", path, time.Since(started).Milliseconds())
		} else if ctx.Err() == nil {
			logger.Printf("级别=信息 事件=数据库备份已检查 结果=尚未到期 耗时毫秒=%d", time.Since(started).Milliseconds())
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
		logger.Printf("级别=信息 事件=HTTP服务已启动 地址=%q", "http://"+addr)
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
		logger.Printf("级别=信息 事件=收到关闭请求 原因=%q", ctx.Err())
	case runErr = <-errCh:
		logger.Printf("级别=错误 事件=服务组件失败 错误=%q", runErr)
		stop()
	}
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := server.Shutdown(shutdownCtx); err != nil {
		logger.Printf("级别=警告 事件=HTTP服务关闭失败 错误=%q", err)
	} else {
		logger.Printf("级别=信息 事件=HTTP服务已停止")
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
