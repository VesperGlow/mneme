//! Safe operational errors: never format URLs, request headers, bodies or raw upstream errors.
use eventsource_stream::EventStreamError;
use serde::Serialize;
use std::error::Error;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Failure {
    pub category: &'static str,
    pub http_status: Option<u16>,
}

fn transport(error: &reqwest::Error) -> Failure {
    let mut category = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_status() {
        "http"
    } else if error.is_body() {
        "response_body"
    } else {
        "request"
    };
    if category == "connect" {
        // Inspect causes only to select fixed labels. Their text can contain secrets.
        let mut cause = error.source();
        while let Some(current) = cause {
            let text = current.to_string().to_ascii_lowercase();
            if text.contains("dns") || text.contains("failed to lookup address") {
                category = "dns";
            } else if text.contains("certificate") || text.contains("tls") {
                category = "tls";
            } else if text.contains("connection refused") {
                category = "connection_refused";
            } else if text.contains("network is unreachable") {
                category = "network_unreachable";
            }
            cause = current.source();
        }
    }
    Failure {
        category,
        http_status: error.status().map(|s| s.as_u16()),
    }
}

pub fn describe(error: &anyhow::Error) -> Failure {
    for cause in error.chain() {
        if let Some(error) = cause.downcast_ref::<reqwest::Error>() {
            return transport(error);
        }
        if let Some(error) = cause.downcast_ref::<EventStreamError<reqwest::Error>>() {
            return match error {
                EventStreamError::Transport(error) => transport(error),
                _ => Failure {
                    category: "invalid_stream",
                    http_status: None,
                },
            };
        }
        let category = if cause.is::<serde_json::Error>() {
            "invalid_json"
        } else if cause.is::<sqlx::Error>() {
            "storage"
        } else {
            match cause.to_string().as_str() {
                "upstream stream ended before DONE" => "incomplete_stream",
                "empty or unfinished completion" => "empty_or_unfinished_completion",
                "upstream stream error" => "stream_api_error",
                "unsupported finish reason" => "unsupported_finish_reason",
                "incomplete extraction response" => "incomplete_extraction",
                "incomplete image description" => "incomplete_image_description",
                "missing image description" | "empty image description" => {
                    "empty_image_description"
                }
                "invalid image description" => "invalid_image_description",
                "image description timeout" => "timeout",
                "QQ token API error" => "token_api_error",
                "QQ token missing" => "token_missing",
                "QQ token expiry invalid" => "token_expiry_invalid",
                "upstream body too large" => "response_too_large",
                _ => continue,
            }
        };
        return Failure {
            category,
            http_status: None,
        };
    }
    Failure {
        category: "internal",
        http_status: None,
    }
}

pub fn warn(stage: &'static str, error: &anyhow::Error) {
    let failure = describe(error);
    tracing::warn!(
        stage,
        category = failure.category,
        http_status = failure.http_status,
        "upstream operation failed"
    );
}
