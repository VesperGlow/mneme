package qqbot

import (
	"context"
	"fmt"
	"log"
	"strings"
	"time"
	"unicode/utf8"

	"companion/internal/agent"

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
	appID     string
	appSecret string
	agent     *agent.Agent
	logger    *log.Logger
	api       openapi.OpenAPI
	jobs      chan incomingMessage
}

type incomingMessage struct {
	content   string
	messageID string
	reply     func(context.Context, string) error
}

func New(appID, appSecret string, companion *agent.Agent, logger *log.Logger) *Bot {
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
	b.api = botgo.NewOpenAPI(b.appID, tokenSource).WithTimeout(15 * time.Second)

	intents := event.RegisterHandlers(
		b.c2cHandler(ctx),
	)
	websocketInfo, err := b.api.WS(ctx, nil, "")
	if err != nil {
		return fmt.Errorf("get QQ websocket gateway: %w", err)
	}
	go b.work(ctx)
	b.logger.Printf("QQ websocket starting with %d shard(s)", websocketInfo.Shards)
	if err := botgo.NewSessionManager().Start(websocketInfo, tokenSource, &intents); err != nil {
		return fmt.Errorf("run QQ websocket: %w", err)
	}
	return nil
}

type sdkLogger struct {
	logger *log.Logger
}

func (sdkLogger) Debug(...interface{})          {}
func (sdkLogger) Info(...interface{})           {}
func (sdkLogger) Debugf(string, ...interface{}) {}
func (sdkLogger) Infof(string, ...interface{})  {}
func (l sdkLogger) Warn(v ...interface{})       { l.logger.Print(v...) }
func (l sdkLogger) Error(v ...interface{})      { l.logger.Print(v...) }
func (l sdkLogger) Warnf(format string, v ...interface{}) {
	l.logger.Printf(format, v...)
}
func (l sdkLogger) Errorf(format string, v ...interface{}) {
	l.logger.Printf(format, v...)
}
func (sdkLogger) Sync() error { return nil }

func (b *Bot) work(ctx context.Context) {
	for {
		select {
		case <-ctx.Done():
			return
		case job := <-b.jobs:
			requestCtx, cancel := context.WithTimeout(ctx, 5*time.Minute)
			reply, err := b.agent.ChatInput(requestCtx, agent.Input{Channel: "qq", MessageID: job.messageID, Content: job.content, ReceivedAt: time.Now()})
			if err != nil {
				b.logger.Printf("QQ message failed: %v", err)
				reply = "处理消息时出错了，请稍后再试。"
			}
			if err := job.reply(requestCtx, reply); err != nil {
				b.logger.Printf("QQ reply failed: %v", err)
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
	select {
	case b.jobs <- job:
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
