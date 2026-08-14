package httpapi

import (
	"context"
	"encoding/json"
	"log"
	"net/http"
	"strconv"
	"time"

	"companion/internal/agent"
	"companion/internal/storage"
)

type Server struct {
	agent  *agent.Agent
	store  *storage.Store
	logger *log.Logger
}

func New(agent *agent.Agent, store *storage.Store, logger *log.Logger) http.Handler {
	s := &Server{agent: agent, store: store, logger: logger}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /chat", s.chat)
	mux.HandleFunc("GET /health", s.health)
	mux.HandleFunc("GET /memories", s.memories)
	mux.HandleFunc("GET /export", s.export)
	return s.logRequests(mux)
}

func (s *Server) chat(w http.ResponseWriter, r *http.Request) {
	r.Body = http.MaxBytesReader(w, r.Body, 1<<20)
	var request struct {
		Message string `json:"message"`
	}
	decoder := json.NewDecoder(r.Body)
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&request); err != nil {
		writeError(w, http.StatusBadRequest, "invalid JSON: "+err.Error())
		return
	}
	reply, err := s.agent.ChatInput(r.Context(), agent.Input{
		Channel: "http", MessageID: r.Header.Get("Idempotency-Key"), Content: request.Message, ReceivedAt: time.Now(),
	})
	if err != nil {
		s.writeInternalError(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"reply": reply})
}

func (s *Server) health(w http.ResponseWriter, r *http.Request) {
	ctx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
	defer cancel()
	if err := s.store.Ping(ctx); err != nil {
		writeError(w, http.StatusServiceUnavailable, "database unavailable")
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

func (s *Server) memories(w http.ResponseWriter, r *http.Request) {
	limit := 100
	if raw := r.URL.Query().Get("limit"); raw != "" {
		parsed, err := strconv.Atoi(raw)
		if err != nil || parsed < 1 || parsed > 1000 {
			writeError(w, http.StatusBadRequest, "limit must be between 1 and 1000")
			return
		}
		limit = parsed
	}
	items, err := s.store.ListMemories(r.Context(), limit, false)
	if err != nil {
		s.writeInternalError(w, err)
		return
	}
	if items == nil {
		items = []storage.Memory{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"memories": items})
}

func (s *Server) export(w http.ResponseWriter, r *http.Request) {
	raw, err := s.store.ExportJSON(r.Context())
	if err != nil {
		s.writeInternalError(w, err)
		return
	}
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.Header().Set("Content-Disposition", `attachment; filename="mneme-export.json"`)
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(raw)
	_, _ = w.Write([]byte("\n"))
}

func writeJSON(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}

func writeError(w http.ResponseWriter, status int, message string) {
	writeJSON(w, status, map[string]string{"error": message})
}

func (s *Server) writeInternalError(w http.ResponseWriter, err error) {
	if s.logger != nil {
		s.logger.Printf("level=error event=http_request_failed error=%q", err)
	}
	writeError(w, http.StatusInternalServerError, "internal server error")
}

type responseStatus struct {
	http.ResponseWriter
	status int
}

func (w *responseStatus) WriteHeader(status int) {
	w.status = status
	w.ResponseWriter.WriteHeader(status)
}

func (s *Server) logRequests(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		started := time.Now()
		wrapped := &responseStatus{ResponseWriter: w, status: http.StatusOK}
		next.ServeHTTP(wrapped, r)
		if s.logger != nil && (r.URL.Path != "/health" || wrapped.status >= http.StatusBadRequest) {
			s.logger.Printf("level=info event=http_request_completed method=%q path=%q status=%d duration_ms=%d", r.Method, r.URL.Path, wrapped.status, time.Since(started).Milliseconds())
		}
	})
}
