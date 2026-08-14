package qqbot

import (
	"context"
	"fmt"
	"strings"
	"sync/atomic"
	"time"
	"unicode/utf8"

	"companion/internal/agent"
	"companion/internal/logging"

	"github.com/tencent-connect/botgo"
	"github.com/tencent-connect/botgo/dto"
	"github.com/tencent-connect/botgo/dto/message"
	"github.com/tencent-connect/botgo/event"
	botlog "github.com/tencent-connect/botgo/log"
	"github.com/tencent-connect/botgo/openapi"
	"github.com/tencent-connect/botgo/token"
)

const maxReplyBytes = 1800

type Bot struct {
	appID      string
	appSecret  string
	agent      *agent.Agent
	logger     *logging.Logger
	api        openapi.OpenAPI
	jobs       chan incomingMessage
	messageSeq atomic.Uint64
}

type incomingMessage struct {
	transportID uint64
	content     string
	messageID   string
	reply       func(context.Context, string) error
}

func New(appID, appSecret string, companion *agent.Agent, logger *logging.Logger) *Bot {
	return &Bot{
		appID:     appID,
		appSecret: appSecret,
		agent:     companion,
		logger:    logger,
		jobs:      make(chan incomingMessage, 32),
	}
}

func (b *Bot) Run(ctx context.Context) error {
	botlog.DefaultLogger = sdkLogger{logger: b.logger}
	credentials := &token.QQBotCredentials{AppID: b.appID, AppSecret: b.appSecret}
	tokenSource := token.NewQQBotTokenSource(credentials)
	if err := token.StartRefreshAccessToken(ctx, tokenSource); err != nil {
		return fmt.Errorf("start QQ access token refresh: %w", err)
	}
	b.logger.Info("令牌刷新已启动")
	b.api = botgo.NewOpenAPI(b.appID, tokenSource).WithTimeout(15 * time.Second)

	intents := event.RegisterHandlers(
		b.c2cHandler(ctx),
	)
	websocketInfo, err := b.api.WS(ctx, nil, "")
	if err != nil {
		return fmt.Errorf("get QQ websocket gateway: %w", err)
	}
	b.logger.Info("网关就绪", "shards", websocketInfo.Shards)
	go b.work(ctx)
	b.logger.Info("WebSocket 已启动", "shards", websocketInfo.Shards)
	if err := botgo.NewSessionManager().Start(websocketInfo, tokenSource, &intents); err != nil {
		return fmt.Errorf("run QQ websocket: %w", err)
	}
	return nil
}

type sdkLogger struct {
	logger *logging.Logger
}

func (sdkLogger) Debug(...interface{})          {}
func (sdkLogger) Info(...interface{})           {}
func (sdkLogger) Debugf(string, ...interface{}) {}
func (sdkLogger) Infof(string, ...interface{})  {}
func (l sdkLogger) Warn(v ...interface{}) {
	l.logger.Warn("SDK 警告", "message", fmt.Sprint(v...))
}
func (l sdkLogger) Error(v ...interface{}) {
	l.logger.Error("SDK 错误", "message", fmt.Sprint(v...))
}
func (l sdkLogger) Warnf(format string, v ...interface{}) {
	l.logger.Warn("SDK 警告", "message", fmt.Sprintf(format, v...))
}
func (l sdkLogger) Errorf(format string, v ...interface{}) {
	l.logger.Error("SDK 错误", "message", fmt.Sprintf(format, v...))
}
func (sdkLogger) Sync() error { return nil }

func (b *Bot) work(ctx context.Context) {
	b.logger.Info("工作器已启动", "queue_capacity", cap(b.jobs))
	defer b.logger.Info("工作器已停止")
	for {
		select {
		case <-ctx.Done():
			return
		case job := <-b.jobs:
			started := time.Now()
			logger := b.logger.With("transport", job.transportID)
			logger.Debug("消息处理中", "queue_depth", len(b.jobs))
			requestCtx, cancel := context.WithTimeout(ctx, 5*time.Minute)
			reply, err := b.agent.ChatInput(requestCtx, agent.Input{Channel: "qq", MessageID: job.messageID, Content: job.content, ReceivedAt: time.Now()})
			if err != nil {
				logger.Error("消息处理失败", "duration", time.Since(started), "error", err)
				reply = "处理消息时出错了，请稍后再试。"
			}
			if err := job.reply(requestCtx, reply); err != nil {
				logger.Error("回复失败", "duration", time.Since(started), "error", err)
			} else {
				logger.Info("回复已发送", "output_chars", utf8.RuneCountInString(reply), "chunks", len(splitText(reply, maxReplyBytes)), "duration", time.Since(started))
			}
			cancel()
		}
	}
}

func (b *Bot) enqueue(ctx context.Context, job incomingMessage) error {
	job.content = strings.TrimSpace(message.ETLInput(job.content))
	if job.content == "" {
		return nil
	}
	job.transportID = b.messageSeq.Add(1)
	select {
	case b.jobs <- job:
		b.logger.Info("收到消息", "transport", job.transportID, "input_chars", utf8.RuneCountInString(job.content), "queue_depth", len(b.jobs))
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (b *Bot) c2cHandler(ctx context.Context) event.C2CMessageEventHandler {
	return func(_ *dto.WSPayload, data *dto.WSC2CMessageData) error {
		if data.Author == nil || data.Author.ID == "" {
			return fmt.Errorf("QQ C2C message has no author")
		}
		userID, messageID := data.Author.ID, data.ID
		return b.enqueue(ctx, incomingMessage{
			content:   data.Content,
			messageID: messageID,
			reply: func(replyCtx context.Context, content string) error {
				return sendChunks(content, func(part string, sequence uint32) error {
					_, err := b.api.PostC2CMessage(replyCtx, userID, dto.MessageToCreate{Content: part, MsgID: messageID, MsgSeq: sequence})
					return err
				})
			},
		})
	}
}

func sendChunks(content string, send func(string, uint32) error) error {
	parts := splitText(content, maxReplyBytes)
	for i, part := range parts {
		if err := send(part, uint32(i+1)); err != nil {
			return err
		}
	}
	return nil
}

func splitText(content string, maxBytes int) []string {
	content = strings.TrimSpace(content)
	if content == "" || maxBytes <= 0 {
		return nil
	}
	var parts []string
	start, size := 0, 0
	for index, r := range content {
		runeSize := utf8.RuneLen(r)
		if size > 0 && size+runeSize > maxBytes {
			parts = append(parts, content[start:index])
			start, size = index, 0
		}
		size += runeSize
	}
	if start < len(content) {
		parts = append(parts, content[start:])
	}
	return parts
}
