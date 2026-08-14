package logging

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

// Format controls how log records are rendered.
type Format string

const (
	FormatPretty Format = "pretty"
	FormatJSON   Format = "json"
)

// Options configures a Logger.
type Options struct {
	Format Format
	Level  slog.Level
	Color  bool
}

// Logger is a small structured-logging facade used throughout Mneme.
// Its methods are deliberately safe to call on a nil receiver.
type Logger struct {
	logger    *slog.Logger
	component string
}

// New creates a logger using explicit options.
func New(writer io.Writer, options Options) *Logger {
	if writer == nil {
		writer = io.Discard
	}
	var handler slog.Handler
	if options.Format == FormatJSON {
		handler = slog.NewJSONHandler(writer, &slog.HandlerOptions{Level: options.Level})
	} else {
		handler = newPrettyHandler(writer, options.Level, options.Color)
	}
	return &Logger{logger: slog.New(handler), component: "app"}
}

// NewFromEnv creates the application logger. COMPANION_LOG_FORMAT accepts
// "pretty" (default) or "json"; COMPANION_LOG_LEVEL accepts debug, info,
// warn, or error. ANSI colors are enabled only for an interactive terminal.
func NewFromEnv(writer *os.File) (*Logger, error) {
	format := Format(strings.ToLower(strings.TrimSpace(os.Getenv("COMPANION_LOG_FORMAT"))))
	if format == "" {
		format = FormatPretty
	}
	if format != FormatPretty && format != FormatJSON {
		return nil, fmt.Errorf("COMPANION_LOG_FORMAT must be pretty or json")
	}
	level, err := parseLevel(os.Getenv("COMPANION_LOG_LEVEL"))
	if err != nil {
		return nil, err
	}
	color := false
	if writer != nil && format == FormatPretty && os.Getenv("NO_COLOR") == "" {
		if info, statErr := writer.Stat(); statErr == nil {
			color = info.Mode()&os.ModeCharDevice != 0
		}
	}
	return New(writer, Options{Format: format, Level: level, Color: color}), nil
}

func parseLevel(value string) (slog.Level, error) {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", "info":
		return slog.LevelInfo, nil
	case "debug":
		return slog.LevelDebug, nil
	case "warn", "warning":
		return slog.LevelWarn, nil
	case "error":
		return slog.LevelError, nil
	default:
		return 0, fmt.Errorf("COMPANION_LOG_LEVEL must be debug, info, warn, or error")
	}
}

// Named adds a stable component name to subsequent records.
func (l *Logger) Named(component string) *Logger {
	if l == nil || l.logger == nil {
		return nil
	}
	return &Logger{logger: l.logger, component: component}
}

// With returns a logger carrying the supplied structured fields.
func (l *Logger) With(args ...any) *Logger {
	if l == nil || l.logger == nil {
		return nil
	}
	return &Logger{logger: l.logger.With(args...), component: l.component}
}

func (l *Logger) Debug(message string, args ...any) {
	if l != nil && l.logger != nil {
		l.logger.Debug(message, append([]any{"component", l.component}, args...)...)
	}
}

func (l *Logger) Info(message string, args ...any) {
	if l != nil && l.logger != nil {
		l.logger.Info(message, append([]any{"component", l.component}, args...)...)
	}
}

func (l *Logger) Warn(message string, args ...any) {
	if l != nil && l.logger != nil {
		l.logger.Warn(message, append([]any{"component", l.component}, args...)...)
	}
}

func (l *Logger) Error(message string, args ...any) {
	if l != nil && l.logger != nil {
		l.logger.Error(message, append([]any{"component", l.component}, args...)...)
	}
}

type prettyHandler struct {
	writer io.Writer
	level  slog.Level
	color  bool
	attrs  []slog.Attr
	groups []string
	mu     *sync.Mutex
}

func newPrettyHandler(writer io.Writer, level slog.Level, color bool) *prettyHandler {
	return &prettyHandler{writer: writer, level: level, color: color, mu: &sync.Mutex{}}
}

func (h *prettyHandler) Enabled(_ context.Context, level slog.Level) bool {
	return level >= h.level
}

func (h *prettyHandler) Handle(_ context.Context, record slog.Record) error {
	component := "app"
	fields := make([]field, 0, len(h.attrs)+record.NumAttrs())
	for _, attr := range h.attrs {
		h.collect(&fields, &component, h.groups, attr)
	}
	record.Attrs(func(attr slog.Attr) bool {
		h.collect(&fields, &component, h.groups, attr)
		return true
	})

	level := fmt.Sprintf("%-5s", levelLabel(record.Level))
	if h.color {
		level = levelColor(record.Level) + level + "\x1b[0m"
	}
	timestamp := record.Time.Local().Format("2006-01-02 15:04:05.000")
	var line strings.Builder
	fmt.Fprintf(&line, "%s  %s %-9s │ %s", timestamp, level, component, record.Message)
	for _, item := range fields {
		line.WriteString("  ")
		line.WriteString(item.key)
		line.WriteByte('=')
		line.WriteString(item.value)
	}
	line.WriteByte('\n')

	h.mu.Lock()
	defer h.mu.Unlock()
	_, err := io.WriteString(h.writer, line.String())
	return err
}

func (h *prettyHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	clone := *h
	clone.attrs = append(append([]slog.Attr(nil), h.attrs...), attrs...)
	return &clone
}

func (h *prettyHandler) WithGroup(name string) slog.Handler {
	if name == "" {
		return h
	}
	clone := *h
	clone.groups = append(append([]string(nil), h.groups...), name)
	return &clone
}

type field struct {
	key   string
	value string
}

func (h *prettyHandler) collect(fields *[]field, component *string, groups []string, attr slog.Attr) {
	attr.Value = attr.Value.Resolve()
	if attr.Equal(slog.Attr{}) {
		return
	}
	if attr.Value.Kind() == slog.KindGroup {
		nextGroups := groups
		if attr.Key != "" {
			nextGroups = append(append([]string(nil), groups...), attr.Key)
		}
		for _, child := range attr.Value.Group() {
			h.collect(fields, component, nextGroups, child)
		}
		return
	}
	key := strings.Join(append(append([]string(nil), groups...), attr.Key), ".")
	if key == "component" {
		*component = attr.Value.String()
		return
	}
	*fields = append(*fields, field{key: key, value: formatValue(attr.Value)})
}

func formatValue(value slog.Value) string {
	switch value.Kind() {
	case slog.KindString:
		return quoteIfNeeded(value.String())
	case slog.KindInt64:
		return strconv.FormatInt(value.Int64(), 10)
	case slog.KindUint64:
		return strconv.FormatUint(value.Uint64(), 10)
	case slog.KindFloat64:
		return strconv.FormatFloat(value.Float64(), 'g', -1, 64)
	case slog.KindBool:
		return strconv.FormatBool(value.Bool())
	case slog.KindDuration:
		return value.Duration().String()
	case slog.KindTime:
		return value.Time().Format(time.RFC3339Nano)
	case slog.KindAny:
		if err, ok := value.Any().(error); ok {
			return quoteIfNeeded(err.Error())
		}
		raw, err := json.Marshal(value.Any())
		if err == nil {
			return string(raw)
		}
		return quoteIfNeeded(fmt.Sprint(value.Any()))
	default:
		return quoteIfNeeded(value.String())
	}
}

func quoteIfNeeded(value string) string {
	if value == "" || strings.ContainsAny(value, " \t\r\n=\"") {
		return strconv.Quote(value)
	}
	return value
}

func levelLabel(level slog.Level) string {
	switch {
	case level >= slog.LevelError:
		return "ERROR"
	case level >= slog.LevelWarn:
		return "WARN"
	case level >= slog.LevelInfo:
		return "INFO"
	default:
		return "DEBUG"
	}
}

func levelColor(level slog.Level) string {
	switch {
	case level >= slog.LevelError:
		return "\x1b[31m"
	case level >= slog.LevelWarn:
		return "\x1b[33m"
	case level >= slog.LevelInfo:
		return "\x1b[36m"
	default:
		return "\x1b[90m"
	}
}
