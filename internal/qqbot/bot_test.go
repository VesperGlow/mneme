package qqbot

import (
	"strings"
	"testing"
)

func TestSplitTextKeepsUTF8AndLimit(t *testing.T) {
	input := strings.Repeat("你好a", 20)
	parts := splitText(input, 13)
	if len(parts) < 2 {
		t.Fatalf("expected multiple parts, got %d", len(parts))
	}
	if strings.Join(parts, "") != input {
		t.Fatalf("split changed content: %#v", parts)
	}
	for _, part := range parts {
		if len(part) > 13 {
			t.Fatalf("part exceeds byte limit: %q", part)
		}
	}
}

func TestSendChunksContinuesSequenceAcrossReplies(t *testing.T) {
	var sequence uint32
	var received []uint32
	send := func(_ string, partSequence uint32) error {
		received = append(received, partSequence)
		return nil
	}
	if err := sendChunks(strings.Repeat("a", maxReplyBytes+1), &sequence, send); err != nil {
		t.Fatal(err)
	}
	if err := sendChunks("继续打开公告", &sequence, send); err != nil {
		t.Fatal(err)
	}
	if err := sendChunks("这是最终答案", &sequence, send); err != nil {
		t.Fatal(err)
	}
	want := []uint32{1, 2, 3, 4}
	if len(received) != len(want) {
		t.Fatalf("unexpected sequences: received=%v current=%d", received, sequence)
	}
	for index := range want {
		if received[index] != want[index] {
			t.Fatalf("unexpected sequences: received=%v current=%d", received, sequence)
		}
	}
	if sequence != 4 {
		t.Fatalf("unexpected sequences: received=%v current=%d", received, sequence)
	}
}
