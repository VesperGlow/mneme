package memory

import "testing"

func TestParseDecisionWithCodeFence(t *testing.T) {
	parsed, err := parseDecision("```json\n{\"actions\":[{\"action\":\"add\",\"content\":\"用户喜欢茶\"}]}\n```")
	if err != nil {
		t.Fatal(err)
	}
	if len(parsed.Actions) != 1 || parsed.Actions[0].Content != "用户喜欢茶" {
		t.Fatalf("unexpected decision: %#v", parsed)
	}
}
