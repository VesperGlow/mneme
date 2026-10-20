use axum::{Json, Router, http::StatusCode, routing::get};
use mneme::diagnostics::describe;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn http_errors_preserve_status_without_leaking_urls_or_response_content() {
    let router = Router::new().route(
        "/denied",
        get(|| async {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error":"private response text"})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/denied?key=private-url-key",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let error = reqwest::get(url)
        .await
        .unwrap()
        .error_for_status()
        .unwrap_err();
    let failure = describe(&error.into());
    assert_eq!(failure.category, "http");
    assert_eq!(failure.http_status, Some(401));
    let serialized = serde_json::to_string(&failure).unwrap();
    assert!(!serialized.contains("private"));
    assert!(!serialized.contains("127.0.0.1"));
    server.abort();
}

#[tokio::test]
async fn transport_timeouts_are_reported_separately_from_http_rejections() {
    let router = Router::new().route(
        "/slow",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            "ok"
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/slow", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let error = reqwest::Client::builder()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap()
        .get(url)
        .send()
        .await
        .unwrap_err();
    let failure = describe(&error.into());
    assert_eq!(failure.category, "timeout");
    assert_eq!(failure.http_status, None);
    server.abort();
}

#[test]
fn arbitrary_error_messages_and_stream_payloads_are_not_returned() {
    let error = anyhow::anyhow!("private prompt and sk-private-secret").context("private context");
    assert_eq!(describe(&error).category, "internal");
    assert!(
        !serde_json::to_string(&describe(&error))
            .unwrap()
            .contains("private")
    );
    let parse = serde_json::from_str::<serde_json::Value>("private stream content").unwrap_err();
    assert_eq!(describe(&parse.into()).category, "invalid_json");
    assert_eq!(
        describe(&anyhow::anyhow!("upstream stream ended before DONE")).category,
        "incomplete_stream"
    );
    let error = eventsource_stream::EventStreamError::<reqwest::Error>::Utf8(
        String::from_utf8(vec![0xff]).unwrap_err(),
    );
    assert_eq!(describe(&error.into()).category, "invalid_stream");
}

#[test]
fn sticker_failures_have_fixed_safe_categories() {
    for (message, category) in [
        (
            "incomplete image description",
            "incomplete_image_description",
        ),
        ("missing image description", "empty_image_description"),
        ("empty image description", "empty_image_description"),
        ("invalid image description", "invalid_image_description"),
        ("image description timeout", "timeout"),
    ] {
        assert_eq!(describe(&anyhow::anyhow!(message)).category, category);
    }
}
