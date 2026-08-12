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
