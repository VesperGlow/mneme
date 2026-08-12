package config

import "testing"

func TestLoadAndValidateQQ(t *testing.T) {
	t.Setenv("DEEPSEEK_API_KEY", "deepseek-test")
	t.Setenv("TAVILY_API_KEY", "tavily-test")
	t.Setenv("QQ_APP_ID", "qq-app")
	t.Setenv("QQ_APP_SECRET", "qq-secret")
	t.Setenv("system", "system prompt")
	t.Setenv("persona", `line one\nline two`)

	cfg, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if err := cfg.ValidateQQ(); err != nil {
		t.Fatal(err)
	}
	if cfg.DeepSeekAPIKey != "deepseek-test" || cfg.QQAppID != "qq-app" {
		t.Fatalf("unexpected config: %#v", cfg)
	}
	if cfg.SystemPrompt != "system prompt" || cfg.PersonaPrompt != "line one\nline two" {
		t.Fatalf("unexpected prompts: %#v", cfg)
	}
}

func TestLoadRequiresAPIKeys(t *testing.T) {
	t.Setenv("DEEPSEEK_API_KEY", "")
	t.Setenv("TAVILY_API_KEY", "")
	t.Setenv("system", "system prompt")
	t.Setenv("persona", "persona prompt")
	if _, err := Load(); err == nil {
		t.Fatal("expected missing API key error")
	}
}

func TestLoadRequiresPrompts(t *testing.T) {
	t.Setenv("DEEPSEEK_API_KEY", "deepseek-test")
	t.Setenv("TAVILY_API_KEY", "tavily-test")
	t.Setenv("system", "")
	t.Setenv("persona", "")
	if _, err := Load(); err == nil {
		t.Fatal("expected missing prompt error")
	}
}
