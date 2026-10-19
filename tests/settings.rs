mod common;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use mneme::{
    App, api, config::Config, console_store::MemoryWrite, domain::*, qq_store::Incoming,
    store::BeginTurn,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "settings-test-token-01234567890123456789";
const PASSWORD: &str = " 我的 WebUI 密码 12345 ";

fn config(temp: &tempfile::TempDir, base: &str) -> Config {
    let mut config: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    config.memory.reconcile_updates = false; // This fixture does not implement semantic adjudication.
    config.memory.verify_recall = false;
    config.api_token = TOKEN.into();
    config.llm_key = "fixture".into();
    config.neo4j_password = "fixture".into();
    config.database = temp.path().join("db").to_string_lossy().into();
    config.models.base_url = base.into();
    config.neo4j.base_url = base.into();
    config.llm.model = "fixture-model".into();
    config.llm.base_url = base.into();
    config.memory.poll_seconds = 1;
    config.memory.extract_every_tokens = 1;
    config.llm.context_tokens = 16000;
    config.llm.history_turns = 16;
    config
}
async fn call(
    router: &Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(
                    if token.starts_with("mneme_console=") {
                        "cookie"
                    } else {
                        "authorization"
                    },
                    if token.starts_with("mneme_console=") {
                        token.to_owned()
                    } else {
                        format!("Bearer {token}")
                    },
                )
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn login(router: &Router, password: &str) -> String {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/console/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"password":password}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Max-Age=2592000")
    );
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert!(body.get("token").is_none());
    cookie.split(';').next().unwrap().to_owned()
}

#[tokio::test]
async fn output_format_is_optional_in_legacy_configuration_and_saved_personas() {
    let legacy: Config =
        toml::from_str(&include_str!("../config/default.toml").replace("output_format = \"\"", ""))
            .unwrap();
    assert!(legacy.persona.output_format.is_empty());
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let config = config(&temp, &base);
    let mut saved = serde_json::to_value(&legacy.persona).unwrap();
    saved.as_object_mut().unwrap().remove("output_format");
    saved["name"] = json!("旧版保存的性格");
    let store = mneme::store::Store::open(&config.database).await.unwrap();
    sqlx::query("INSERT INTO metadata(key,value) VALUES('console.persona',?)")
        .bind(saved.to_string())
        .execute(&store.pool)
        .await
        .unwrap();
    store.pool.close().await;
    drop(store);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, rx).await.unwrap();
    let router = api::router(app.clone());
    let settings = call(&router, TOKEN, "GET", "/v1/console/settings", json!({}))
        .await
        .1;
    assert_eq!(settings["persona"]["name"], saved["name"]);
    assert_eq!(settings["persona"]["output_format"], "");
    let user = Message {
        id: id(),
        session_id: id(),
        turn_id: id(),
        role: "user".into(),
        content: "你好".into(),
        created_at: now(),
    };
    assert!(
        !api::build_context(&app, vec![], &user, &[])[0]
            .content
            .contains("输出格式要求：")
    );
    let mut formatted = saved.clone();
    formatted["output_format"] = json!("只使用纯文本回答。");
    assert_eq!(
        call(&router, TOKEN, "PUT", "/v1/console/persona", formatted)
            .await
            .0,
        StatusCode::OK
    );
    assert!(
        api::build_context(&app, vec![], &user, &[])[0]
            .content
            .contains("只使用纯文本回答。")
    );
    // Older clients can omit the field; clearing it removes the extra system instruction.
    let (status, cleared) = call(&router, TOKEN, "PUT", "/v1/console/persona", saved).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["persona"]["output_format"], "");
    assert!(
        !api::build_context(&app, vec![], &user, &[])[0]
            .content
            .contains("输出格式要求：")
    );
    fake.abort();
}

#[tokio::test]
async fn persona_applies_to_context_and_survives_restart() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let config = config(&temp, &base);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config.clone(), rx.clone()).await.unwrap();
    let router = api::router(app.clone());
    for (method, path) in [
        ("GET", "/v1/console/settings"),
        ("PUT", "/v1/console/persona"),
        ("PUT", "/v1/console/password"),
        ("POST", "/v1/admin/reset"),
    ] {
        assert_eq!(
            call(&router, "", method, path, json!({})).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    let mut persona = serde_json::to_value(&config.persona).unwrap();
    persona["name"] = json!("小满 <script>");
    persona["style"] = json!("简洁、俏皮，认真倾听。");
    persona["output_format"] = json!("先给结论，再用纯文本分段说明。\n不使用 Markdown 表格。");
    assert_eq!(
        call(
            &router,
            TOKEN,
            "PUT",
            "/v1/console/persona",
            persona.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    let user = Message {
        id: id(),
        session_id: id(),
        turn_id: id(),
        role: "user".into(),
        content: "你好".into(),
        created_at: now(),
    };
    let context = api::build_context(&app, vec![], &user, &[]);
    assert!(context[0].content.contains("小满 <script>"));
    assert!(context[0].content.contains("简洁、俏皮"));
    assert!(
        context[0]
            .content
            .contains("输出格式要求：\n先给结论，再用纯文本分段说明。\n不使用 Markdown 表格。")
    );
    let mut invalid = persona.clone();
    invalid["name"] = json!(" ");
    assert_eq!(
        call(&router, TOKEN, "PUT", "/v1/console/persona", invalid)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    for length in [6000, 7000] {
        let mut oversized = persona.clone();
        oversized["output_format"] = json!("字".repeat(length));
        assert_eq!(
            call(&router, TOKEN, "PUT", "/v1/console/persona", oversized)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        call(&router, TOKEN, "GET", "/v1/console/settings", json!({}))
            .await
            .1["persona"],
        persona
    );
    drop(router);
    app.store.pool.close().await;
    drop(app);
    let reopened = App::new(config, rx).await.unwrap();
    assert_eq!(
        serde_json::to_value(reopened.persona.read().unwrap().clone()).unwrap(),
        persona
    );
    fake.abort();
}

#[tokio::test]
async fn password_login_rotation_logout_restart_and_token_recovery() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let config = config(&temp, &base);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config.clone(), rx.clone()).await.unwrap();
    let router = api::router(app.clone());
    assert_eq!(
        call(
            &router,
            TOKEN,
            "PUT",
            "/v1/console/password",
            json!({"new_password":"short"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &router,
            TOKEN,
            "PUT",
            "/v1/console/password",
            json!({"new_password":PASSWORD})
        )
        .await
        .0,
        StatusCode::OK
    );
    let stored: String =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key='console.password'")
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
    assert!(!stored.contains(PASSWORD));
    let session = login(&router, PASSWORD).await;
    assert_ne!(session, TOKEN);
    let settings = call(&router, &session, "GET", "/v1/console/settings", json!({})).await;
    assert_eq!(settings.0, StatusCode::OK);
    assert_eq!(settings.1["password_configured"], true);
    assert!(!settings.1.to_string().contains(&stored));
    assert_eq!(
        call(
            &router,
            &session,
            "PUT",
            "/v1/console/password",
            json!({"new_password":"replacement-password", "current_password":"wrong"})
        )
        .await
        .1["error"],
        "current_password_incorrect"
    );
    assert_eq!(
        call(
            &router,
            &session,
            "PUT",
            "/v1/console/password",
            json!({"new_password":"replacement-password", "current_password":PASSWORD})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&router, &session, "GET", "/v1/console/auth", json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let session = login(&router, "replacement-password").await;
    call(&router, &session, "POST", "/v1/console/logout", json!({})).await;
    assert_eq!(
        call(&router, &session, "GET", "/v1/console/auth", json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let session = login(&router, "replacement-password").await;
    drop(router);
    app.store.pool.close().await;
    drop(app);
    let app = App::new(config, rx).await.unwrap();
    let router = api::router(app);
    assert_eq!(
        call(&router, &session, "GET", "/v1/console/auth", json!({}))
            .await
            .0,
        StatusCode::OK
    );
    login(&router, "replacement-password").await;
    // An empty password cannot disable password-only login. API recovery rotates it.
    assert_eq!(
        call(
            &router,
            TOKEN,
            "PUT",
            "/v1/console/password",
            json!({"new_password":""})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &router,
            TOKEN,
            "PUT",
            "/v1/console/password",
            json!({"new_password":"recovered-password"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &router,
            "",
            "POST",
            "/v1/console/login",
            json!({"password":"replacement-password"})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &router,
            "",
            "POST",
            "/v1/console/login",
            json!({"password":"replacement-password"})
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        call(&router, TOKEN, "GET", "/v1/console/auth", json!({}))
            .await
            .0,
        StatusCode::OK
    );
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/console/login")
                .header("sec-fetch-site", "cross-site")
                .header("content-type", "application/json")
                .body(Body::from(json!({"password":PASSWORD}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    fake.abort();
}

#[tokio::test]
async fn reset_is_atomic_preserves_credentials_and_cleans_graph_after_restart() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let config = config(&temp, &base);
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config.clone(), rx.clone()).await.unwrap();
    let router = api::router(app.clone());
    let session = app.store.create_session().await.unwrap();
    let BeginTurn::Started(user) = app
        .store
        .begin_turn(&session, &id(), "我住在成都")
        .await
        .unwrap()
    else {
        panic!()
    };
    app.store.complete_turn(&user, "记住了").await.unwrap();
    let memory: MemoryWrite = serde_json::from_value(json!({"content":"用户住在成都。","kind":"fact","importance":0.9,"confidence":1.0,"entities":["成都"],"valid_until":null})).unwrap();
    app.store.manual_memory(None, &memory).await.unwrap();
    let memory = app.store.active_memories(10).await.unwrap().remove(0);
    app.graph.project(&memory).await.unwrap();
    app.store
        .qq_accept(
            "12345678",
            Incoming {
                message_id: "old".into(),
                sender: "owner".into(),
                content: "旧消息".into(),
                expires_at: now() + 290,
            },
        )
        .await
        .unwrap();
    let mut persona = serde_json::to_value(&config.persona).unwrap();
    persona["name"] = json!("小满");
    call(
        &router,
        TOKEN,
        "PUT",
        "/v1/console/persona",
        persona.clone(),
    )
    .await;
    call(
        &router,
        TOKEN,
        "PUT",
        "/v1/console/password",
        json!({"new_password":PASSWORD}),
    )
    .await;
    let web_session = login(&router, PASSWORD).await;
    assert_eq!(
        call(
            &router,
            TOKEN,
            "POST",
            "/v1/admin/reset",
            json!({"confirmation":"wrong"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    sqlx::query("CREATE TRIGGER reject_reset BEFORE DELETE ON messages BEGIN SELECT RAISE(ABORT,'fixture reset failure'); END").execute(&app.store.pool).await.unwrap();
    assert_eq!(
        call(
            &router,
            TOKEN,
            "POST",
            "/v1/admin/reset",
            json!({"confirmation":"RESET"})
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(app.store.active_memories(10).await.unwrap().len(), 1);
    assert_eq!(app.store.history(&session, 10).await.unwrap().len(), 2);
    sqlx::query("DROP TRIGGER reject_reset")
        .execute(&app.store.pool)
        .await
        .unwrap();
    let (status, result) = call(
        &router,
        &web_session,
        "POST",
        "/v1/admin/reset",
        json!({"confirmation":"RESET"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["deleted_messages"], 2);
    assert_eq!(result["deleted_memories"], 1);
    for table in [
        "sessions",
        "turns",
        "messages",
        "memories",
        "memory_sources",
        "memory_revisions",
        "jobs",
        "qq_inbox",
        "qq_sessions",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&app.store.pool)
                .await
                .unwrap(),
            0,
            "{table}"
        );
    }
    let settings = call(
        &router,
        &web_session,
        "GET",
        "/v1/console/settings",
        json!({}),
    )
    .await
    .1;
    assert_eq!(settings["persona"], persona);
    assert_eq!(settings["password_configured"], true);
    assert_eq!(settings["graph_cleanup_pending"], true);
    let qq = app.store.qq_status("12345678").await.unwrap();
    assert_eq!(qq["bound"], true);
    assert_eq!(qq["has_session"], false);
    assert_eq!(
        call(
            &router,
            TOKEN,
            "GET",
            &format!("/v1/sessions/{session}/messages"),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert!(
        mneme::memory::recall(&app, "成都")
            .await
            .unwrap()
            .memories
            .is_empty()
    );
    drop(router);
    app.store.pool.close().await;
    drop(app);
    let app = App::new(config.clone(), rx).await.unwrap();
    let router = api::router(app.clone());
    login(&router, PASSWORD).await;
    let worker = tokio::spawn(mneme::memory::worker(app.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let pending: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM metadata WHERE key='agent.graph_reset_pending')",
            )
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
            if !pending {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(app.graph.keyword_hits("成都", 10).await.unwrap().is_empty());
    stop.send(true).unwrap();
    worker.await.unwrap();
    assert_eq!(
        call(
            &router,
            TOKEN,
            "POST",
            "/v1/admin/reset",
            json!({"confirmation":"RESET", "reset_persona":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&router, TOKEN, "GET", "/v1/console/settings", json!({}))
            .await
            .1["persona"],
        serde_json::to_value(config.persona).unwrap()
    );
    fake.abort();
}

#[tokio::test]
async fn reset_with_graph_offline_still_allows_new_memory_extraction() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let mut config = config(&temp, &base);
    config.neo4j.base_url = "http://127.0.0.1:1".into();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, rx).await.unwrap();
    let router = api::router(app.clone());
    assert_eq!(
        call(
            &router,
            TOKEN,
            "POST",
            "/v1/admin/reset",
            json!({"confirmation":"RESET"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let session = app.store.create_session().await.unwrap();
    let BeginTurn::Started(user) = app
        .store
        .begin_turn(&session, &id(), "我住在成都")
        .await
        .unwrap()
    else {
        panic!()
    };
    app.store.complete_turn(&user, "收到").await.unwrap();
    let worker = tokio::spawn(mneme::memory::worker(app.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if !app.store.active_memories(10).await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        call(&router, TOKEN, "GET", "/v1/console/settings", json!({}))
            .await
            .1["graph_cleanup_pending"],
        true
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
}

#[tokio::test]
async fn reset_refuses_a_generation_until_its_stream_finishes() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let gate = release.clone();
    let llm = Router::new().route("/chat/completions", axum::routing::post(move || {
        let gate = gate.clone();
        async move { gate.notified().await; ([("content-type", "text/event-stream")], "data: {\"choices\":[{\"delta\":{\"content\":\"完成\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n") }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = config(&temp, &base);
    config.llm.base_url = format!("http://{}", listener.local_addr().unwrap());
    let slow = tokio::spawn(async move { axum::serve(listener, llm).await.unwrap() });
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, rx).await.unwrap();
    let router = api::router(app.clone());
    let session = app.store.create_session().await.unwrap();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session}/chat"))
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"request_id":id(), "message":"你好"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        call(
            &router,
            TOKEN,
            "POST",
            "/v1/admin/reset",
            json!({"confirmation":"RESET"})
        )
        .await
        .1["error"],
        "agent_busy"
    );
    release.notify_one();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("event: done"));
    assert_eq!(
        call(
            &router,
            TOKEN,
            "POST",
            "/v1/admin/reset",
            json!({"confirmation":"RESET"})
        )
        .await
        .0,
        StatusCode::OK
    );
    fake.abort();
    slow.abort();
}

#[tokio::test]
async fn initial_password_import_cookie_expiry_renewal_and_csrf_protection() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let mut config = config(&temp, &base);
    config.console.password = PASSWORD.into();
    config.console.secure_cookie = true;
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, rx).await.unwrap();
    let router = api::router(app.clone());
    let cookie = login(&router, PASSWORD).await;
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM console_sessions")
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert!(!cookie.contains(&stored));
    sqlx::query("UPDATE console_sessions SET expires_at=?")
        .bind(now() + 10)
        .execute(&app.store.pool)
        .await
        .unwrap();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/console/auth")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("; Secure")
    );
    let expiry: i64 = sqlx::query_scalar("SELECT expires_at FROM console_sessions")
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert!(expiry >= now() + 30 * 86400 - 2);
    for (key, value) in [
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-site", "same-site"),
        ("origin", "https://attacker.invalid"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/console/logout")
                    .header("cookie", &cookie)
                    .header("host", "console.invalid")
                    .header(key, value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(
        call(&router, &cookie, "GET", "/v1/console/auth", json!({}))
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE console_sessions SET expires_at=?")
        .bind(now() - 1)
        .execute(&app.store.pool)
        .await
        .unwrap();
    assert_eq!(
        call(&router, &cookie, "GET", "/v1/console/auth", json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // A session cookie cannot be used as an API bearer credential.
    assert_eq!(
        call(
            &router,
            cookie.split_once('=').unwrap().1,
            "GET",
            "/v1/console/auth",
            json!({})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    fake.abort();
}

#[test]
fn legacy_config_gets_continuity_defaults_and_rejects_invalid_limits() {
    let text = include_str!("../config/default.toml");
    let before = text
        .split("[console]")
        .next()
        .unwrap()
        .replace("summary_tokens = 2000", "")
        .replace("context_tokens = 100000", "context_chars = 16000");
    let after = text.split("[models]").nth(1).unwrap();
    let legacy: Config = toml::from_str(&format!("{before}\n[models]{after}")).unwrap();
    assert_eq!(legacy.llm.summary_tokens, 2000);
    assert_eq!(legacy.llm.context_tokens, 100_000);
    assert_eq!(legacy.llm.legacy_context_chars, Some(16_000));
    assert_eq!(legacy.console.session_days, 30);
    assert!(!legacy.console.secure_cookie);
    let temp = tempfile::TempDir::new().unwrap();
    let mut c = config(&temp, "http://127.0.0.1:8090");
    c.validate().unwrap();
    c.console.session_days = 0;
    assert!(c.validate().is_err());
    c.console.session_days = 30;
    c.llm.summary_tokens = 100;
    assert!(c.validate().is_err());
    c.llm.summary_tokens = 2000;
    c.console.password = "short".into();
    assert!(c.validate().is_err());
}
