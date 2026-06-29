package main

import (
	"context"
	"crypto/sha256"
	"fmt"
	"log"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/tencent-connect/botgo/dto"
	"github.com/tencent-connect/botgo/openapi"
)

type ScopeKind string

// 本机器人定位为纯私聊情感陪伴，只保留 C2C 一种场景。
const ScopeC2C ScopeKind = "c2c"

type MessageJob struct {
	Kind           ScopeKind
	MessageID      string
	ReplyTarget    string
	UserID         string
	ConversationID string
	Content        string
	HasAttachments bool
}

type Processor struct {
	api     openapi.OpenAPI
	ai      *AIClient
	cfg     Config
	jobs    chan MessageJob
	deduper *Deduper
	locks   sync.Map
}

func NewProcessor(api openapi.OpenAPI, cfg Config) *Processor {
	p := &Processor{
		api:     api,
		ai:      NewAIClient(cfg.AIURL, cfg.AIAPIKey, cfg.AITimeout),
		cfg:     cfg,
		jobs:    make(chan MessageJob, cfg.QueueSize),
		deduper: NewDeduper(cfg.DedupTTL),
	}
	for i := 0; i < cfg.Workers; i++ {
		go p.worker(i + 1)
	}
	return p
}

func (p *Processor) Submit(job MessageJob) {
	if job.MessageID == "" || job.ReplyTarget == "" || job.UserID == "" {
		log.Printf("忽略字段不完整的 QQ 消息: kind=%s msg=%q target=%q user=%q", job.Kind, job.MessageID, job.ReplyTarget, job.UserID)
		return
	}
	if !p.deduper.Accept(string(job.Kind) + ":" + job.MessageID) {
		log.Printf("忽略 QQ 重复事件: %s", job.MessageID)
		return
	}
	select {
	case p.jobs <- job:
	default:
		log.Printf("QQ 消息队列已满，丢弃消息: %s", job.MessageID)
	}
}

func (p *Processor) worker(id int) {
	for job := range p.jobs {
		lockValue, _ := p.locks.LoadOrStore(job.ConversationID, &sync.Mutex{})
		lock := lockValue.(*sync.Mutex)
		lock.Lock()
		p.process(job)
		lock.Unlock()
	}
}

func (p *Processor) process(job MessageJob) {
	content := strings.TrimSpace(job.Content)
	if content == "" {
		if job.HasAttachments {
			_ = p.sendText(context.Background(), job, "我目前只能处理文字消息，暂时还看不了这个附件。")
		}
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), p.cfg.AITimeout)
	defer cancel()
	reply, err := p.ai.Chat(ctx, job.UserID, job.ConversationID, content)
	if err != nil {
		log.Printf("AI 处理 QQ 消息失败: msg=%s err=%v", job.MessageID, err)
		if sendErr := p.sendText(context.Background(), job, "这次处理失败了，请稍后再试。"); sendErr != nil {
			log.Printf("发送 QQ 错误提示失败: msg=%s err=%v", job.MessageID, sendErr)
		}
		return
	}
	if err := p.sendText(context.Background(), job, reply); err != nil {
		log.Printf("回复 QQ 消息失败: msg=%s err=%v", job.MessageID, err)
	}
}

func (p *Processor) sendText(ctx context.Context, job MessageJob, text string) error {
	parts := splitMessage(text, p.cfg.ReplyMaxRunes, p.cfg.ReplyMaxParts)
	for index, part := range parts {
		msg := dto.MessageToCreate{
			Content: part,
			MsgType: dto.TextMsg,
			MsgID:   job.MessageID,
			MsgSeq:  uint32(index + 1),
		}
		var err error
		switch job.Kind {
		case ScopeC2C:
			_, err = p.api.PostC2CMessage(ctx, job.ReplyTarget, msg)
		default:
			return fmt.Errorf("未知 QQ 消息场景: %s", job.Kind)
		}
		if err != nil {
			return err
		}
		if index < len(parts)-1 {
			time.Sleep(250 * time.Millisecond)
		}
	}
	return nil
}

func splitMessage(text string, maxRunes, maxParts int) []string {
	text = strings.TrimSpace(text)
	if text == "" {
		return []string{"（空回复）"}
	}
	runes := []rune(text)
	if len(runes) <= maxRunes {
		return []string{text}
	}
	chunkSize := maxRunes - 16
	if chunkSize < 1 {
		chunkSize = maxRunes
	}
	truncatedMarker := []rune("…（回复过长，已截断）")
	capacity := chunkSize * maxParts
	if len(runes) > capacity {
		keep := capacity - len(truncatedMarker)
		if keep < 1 {
			keep = capacity
		}
		runes = append(append([]rune{}, runes[:keep]...), truncatedMarker...)
	}
	count := (len(runes) + chunkSize - 1) / chunkSize
	parts := make([]string, 0, count)
	for start, index := 0, 1; start < len(runes); start, index = start+chunkSize, index+1 {
		end := start + chunkSize
		if end > len(runes) {
			end = len(runes)
		}
		parts = append(parts, fmt.Sprintf("（%d/%d）%s", index, count, string(runes[start:end])))
	}
	return parts
}

func stableIDs(scope ScopeKind, targetID, senderID string) (string, string) {
	userHash := sha256.Sum256([]byte(string(scope) + "\x00" + targetID + "\x00" + senderID))
	conversationHash := sha256.Sum256([]byte("conversation\x00" + string(scope) + "\x00" + targetID + "\x00" + senderID))
	return fmt.Sprintf("qq:%s:%x", scope, userHash[:16]), fmt.Sprintf("qqc:%s:%x", scope, conversationHash[:16])
}

func attachmentPresent(items []*dto.MessageAttachment) bool {
	return len(items) > 0
}

func validUTF8(value string) string {
	if utf8.ValidString(value) {
		return value
	}
	return strings.ToValidUTF8(value, "�")
}
