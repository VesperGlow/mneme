package logging

import (
	"bytes"
	"encoding/json"
	"log/slog"
	"strings"
	"testing"
)

func TestPrettyLoggerFormatsReadableStructuredLine(t *testing.T) {
	var output bytes.Buffer
	logger := New(&output, Options{Format: FormatPretty, Level: slog.LevelInfo}).Named("agent")
	logger.Info("收到聊天", "request", 7, "channel", "qq", "input_chars", 18)

	line := output.String()
	for _, expected := range []string{"INFO", "agent", "│ 收到聊天", "request=7", "channel=qq", "input_chars=18"} {
		if !strings.Contains(line, expected) {
			t.Fatalf("expected %q in log line %q", expected, line)
		}
	}
	if strings.Contains(line, "级别=") || strings.Contains(line, "事件=") {
		t.Fatalf("legacy key-value prefix leaked into pretty log: %q", line)
	}
}

func TestPrettyLoggerQuotesUnsafeValues(t *testing.T) {
	var output bytes.Buffer
	New(&output, Options{Format: FormatPretty, Level: slog.LevelInfo}).Info("测试", "reason", "queue full")
	if !strings.Contains(output.String(), `reason="queue full"`) {
		t.Fatalf("unsafe value was not quoted: %q", output.String())
	}
}

func TestJSONLoggerKeepsStructuredFields(t *testing.T) {
	var output bytes.Buffer
	New(&output, Options{Format: FormatJSON, Level: slog.LevelInfo}).Named("http").Warn("请求失败", "status", 500)

	var record map[string]any
	if err := json.Unmarshal(output.Bytes(), &record); err != nil {
		t.Fatal(err)
	}
	if record["level"] != "WARN" || record["msg"] != "请求失败" || record["component"] != "http" || record["status"] != float64(500) {
		t.Fatalf("unexpected JSON record: %#v", record)
	}
}

func TestNilLoggerIsNoop(t *testing.T) {
	var logger *Logger
	logger.With("request", 1).Info("ignored")
	logger.Warn("ignored")
	logger.Error("ignored")
}
