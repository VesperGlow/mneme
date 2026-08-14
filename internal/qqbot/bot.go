package qqbot

import (
	"context"
	"fmt"
	"log"
	"strings"
	"sync/atomic"
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
	appID      string
	appSecret  string
	agent      *agent.Agent
	logger     *log.Logger
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
	b.logger.Printf("级别=信息 事件=QQ令牌刷新已启动")
	b.api = botgo.NewOpenAPI(b.appID, tokenSource).WithTimeout(15 * time.Second)

	intents := event.RegisterHandlers(
		b.c2cHandler(ctx),
	)
	websocketInfo, err := b.api.WS(ctx, nil, "")
	if err != nil {
		return fmt.Errorf("get QQ websocket gateway: %w", err)
	}
	b.logger.Printf("级别=信息 事件=QQ网关就绪 分片数=%d", websocketInfo.Shards)
	go b.work(ctx)
	b.logger.Printf("级别=信息 事件=QQ_WebSocket已启动 分片数=%d", websocketInfo.Shards)
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
func (l sdkLogger) Warn(v ...interface{}) {
	l.logger.Printf("级别=警告 事件=QQ_SDK警告 消息=%q", fmt.Sprint(v...))
}
func (l sdkLogger) Error(v ...interface{}) {
	l.logger.Printf("级别=错误 事件=QQ_SDK错误 消息=%q", fmt.Sprint(v...))
}
func (l sdkLogger) Warnf(format string, v ...interface{}) {
	l.logger.Printf("级别=警告 事件=QQ_SDK警告 消息=%q", fmt.Sprintf(format, v...))
}
func (l sdkLogger) Errorf(format string, v ...interface{}) {
	l.logger.Printf("级别=错误 事件=QQ_SDK错误 消息=%q", fmt.Sprintf(format, v...))
}
func (sdkLogger) Sync() error { return nil }

func (b *Bot) work(ctx context.Context) {
	b.logger.Printf("级别=信息 事件=QQ工作器已启动 队列容量=%d", cap(b.jobs))
	defer b.logger.Printf("级别=信息 事件=QQ工作器已停止")
	for {
		select {
		case <-ctx.Done():
			return
		case job := <-b.jobs:
			started := time.Now()
			b.logger.Printf("级别=信息 事件=QQ消息处理中 传输=%d 队列深度=%d", job.transportID, len(b.jobs))
			requestCtx, cancel := context.WithTimeout(ctx, 5*time.Minute)
			reply, err := b.agent.ChatInput(requestCtx, agent.Input{Channel: "qq", MessageID: job.messageID, Content: job.content, ReceivedAt: time.Now()})
			if err != nil {
				b.logger.Printf("级别=错误 事件=QQ消息处理失败 传输=%d 耗时毫秒=%d 错误=%q", job.transportID, time.Since(started).Milliseconds(), err)
				reply = "处理消息时出错了，请稍后再试。"
			}
			if err := job.reply(requestCtx, reply); err != nil {
				b.logger.Printf("级别=错误 事件=QQ回复失败 传输=%d 耗时毫秒=%d 错误=%q", job.transportID, time.Since(started).Milliseconds(), err)
			} else {
				b.logger.Printf("级别=信息 事件=QQ回复已发送 传输=%d 输出字符数=%d 分段数=%d 耗时毫秒=%d", job.transportID, utf8.RuneCountInString(reply), len(splitText(reply, maxReplyBytes)), time.Since(started).Milliseconds())
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
		b.logger.Printf("级别=信息 事件=收到QQ消息 传输=%d 输入字符数=%d 队列深度=%d", job.transportID, utf8.RuneCountInString(job.content), len(b.jobs))
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
