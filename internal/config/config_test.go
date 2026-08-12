package config

import "testing"

func TestLoadAndValidateQQ(t *testing.T) {
	t.Setenv("DEEPSEEK_API_KEY", "deepseek-test")
	t.Setenv("TAVILY_API_KEY", "tavily-test")
	t.Setenv("QQ_APP_ID", "qq-app")
	t.Setenv("QQ_APP_SECRET", "qq-secret")

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
}

func TestLoadRequiresAPIKeys(t *testing.T) {
	t.Setenv("DEEPSEEK_API_KEY", "")
	t.Setenv("TAVILY_API_KEY", "")
	if _, err := Load(); err == nil {
		t.Fatal("expected missing API key error")
	}
}
