use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::config::ProxyConfig;
use crate::proxy::server::build_client;
use crate::warp::WarpResolver;

pub struct ProxyState {
    pub config: ProxyConfig,
    pub client: Client,
    pub warp_resolver: WarpResolver,
}

fn strip_encrypted_content_from_input(body: &mut Value) {
    let Some(items) = body.get_mut("input").and_then(|v| v.as_array_mut()) else {
        return;
    };
    let mut drop_idx = Vec::new();
    for (i, item) in items.iter_mut().enumerate() {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        if obj.get("type").and_then(|v| v.as_str()) != Some("reasoning") {
            continue;
        }
        obj.remove("encrypted_content");
        obj.remove("id");
        let has_text = obj
            .get("summary")
            .and_then(|v| v.as_array())
            .map(|s| {
                s.iter().any(|e| {
                    e.get("text")
                        .and_then(|t| t.as_str())
                        .map(|t| !t.trim().is_empty())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if !has_text {
            drop_idx.push(i);
        }
    }
    for i in drop_idx.into_iter().rev() {
        items.remove(i);
    }
}

fn is_summary_verification_error(body: &str) -> bool {
    let lower = body.to_lowercase();
    lower.contains("must be verified") && lower.contains("summar")
}

fn strip_reasoning_summary(body: &mut Value) {
    if let Some(reasoning) = body.get_mut("reasoning").and_then(|v| v.as_object_mut()) {
        reasoning.remove("summary");
        if reasoning.is_empty() {
            body.as_object_mut().map(|o| o.remove("reasoning"));
        }
    }
}
fn has_encrypted_content(body: &Value) -> bool {
    body.get("input")
        .and_then(|v| v.as_array())
        .map(|items| {
            items.iter().any(|item| {
                item.as_object()
                    .map(|obj| {
                        obj.get("type").and_then(|v| v.as_str()) == Some("reasoning")
                            && obj
                                .get("encrypted_content")
                                .and_then(|v| v.as_str())
                                .map(|s| !s.is_empty())
                                .unwrap_or(false)
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn is_encrypted_content_error(body: &str) -> bool {
    let lower = body.to_lowercase();
    lower.contains("encrypted_content") && lower.contains("was not issued to this caller")
}

fn inject_responses_fix(body: &mut Value) {
    if let Some(obj) = body.as_object_mut() {
        obj.insert("store".to_string(), Value::Bool(false));
        if let Some(include) = obj.get_mut("include").and_then(|v| v.as_array_mut()) {
            include.retain(|v| v.as_str() != Some("reasoning.encrypted_content"));
            if include.is_empty() {
                obj.remove("include");
            }
        }
        match obj.get_mut("reasoning") {
            Some(Value::Object(reasoning)) => {
                if !reasoning.contains_key("summary") {
                    reasoning.insert(
                        "summary".to_string(),
                        Value::String("auto".to_string()),
                    );
                }
            }
            Some(_) => {
                let mut reasoning = serde_json::Map::new();
                reasoning.insert(
                    "summary".to_string(),
                    Value::String("auto".to_string()),
                );
                obj.insert("reasoning".to_string(), Value::Object(reasoning));
            }
            None => {
                let mut reasoning = serde_json::Map::new();
                reasoning.insert(
                    "summary".to_string(),
                    Value::String("auto".to_string()),
                );
                obj.insert("reasoning".to_string(), Value::Object(reasoning));
            }
        }
    }
}

fn normalize_auth_header(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("bearer ") {
        trimmed.to_string()
    } else {
        format!("Bearer {}", trimmed)
    }
}

fn resolve_auth_header(caller_auth: Option<String>, fallback_key: Option<String>) -> Option<String> {
    if let Some(h) = caller_auth {
        if h.trim().is_empty() {
            fallback_key.map(|k| normalize_auth_header(&k))
        } else {
            Some(normalize_auth_header(&h))
        }
    } else {
        fallback_key.map(|k| normalize_auth_header(&k))
    }
}

pub async fn handle_responses(
    State(state): State<Arc<Mutex<ProxyState>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut request_body = match serde_json::from_slice::<Value>(&body) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to parse request body: {}", e);
            return error_response(&format!("Invalid JSON: {}", e));
        }
    };

    if has_encrypted_content(&request_body) {
        inject_responses_fix(&mut request_body);
        strip_encrypted_content_from_input(&mut request_body);
    }

    let is_stream = request_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let model = request_body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let caller_auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let state_guard = state.lock().await;
    let base_url = state_guard.config.opencode_base_url.clone();
    let proxy_api_key = state_guard.config.opencode_api_key.clone();
    let max_retries = state_guard.config.max_retries;
    let reset_delay = state_guard.config.warp_reset_delay_ms;
    let mut client = state_guard.client.clone();
    drop(state_guard);

    let auth = resolve_auth_header(caller_auth, proxy_api_key);

    let mut retry_count = 0;
    let mut encrypted_retried = false;
    let mut summary_retried = false;
    let mut body_for_retry = request_body.clone();

    loop {
        let result = if is_stream {
            forward_responses_streaming(&base_url, &auth, &body_for_retry, &model, &client).await
        } else {
            forward_responses_non_streaming(&base_url, &auth, &body_for_retry, &model, &client).await
        };

        match result {
            Ok(response) => return response,
            Err(ProxyError::RateLimited) => {
                retry_count += 1;
                if retry_count > max_retries {
                    return error_response("Rate limit exceeded after WARP resets");
                }

                info!(
                    "429 received, attempting WARP reset (attempt {}/{})",
                    retry_count, max_retries
                );

                let resolver = WarpResolver::new(max_retries, reset_delay);
                if !resolver.handle_429(retry_count - 1).await {
                    return error_response("WARP reset failed, rate limit still active");
                }
                if let Ok(fresh) = build_client() {
                    state.lock().await.client = fresh.clone();
                    client = fresh;
                    info!("Rebuilt HTTP client pool after WARP reset");
                }
            }
            Err(ProxyError::EncryptedContentError) => {
                if encrypted_retried {
                    return error_response("Upstream encrypted_content error persisted after retry");
                }
                encrypted_retried = true;
                strip_encrypted_content_from_input(&mut body_for_retry);
                info!("Stripped encrypted_content from reasoning items, retrying...");
                continue;
            }
            Err(ProxyError::SummaryVerificationError) => {
                if summary_retried {
                    return error_response("Upstream reasoning-summary verification error persisted after retry");
                }
                summary_retried = true;
                strip_reasoning_summary(&mut body_for_retry);
                info!("Stripped reasoning.summary after verification error, retrying...");
                continue;
            }
            Err(ProxyError::RequestFailed(msg)) => {
                error!("Upstream request failed: {}", msg);
                return error_response(&format!("Upstream error: {}", msg));
            }
        }
    }
}

async fn forward_responses_streaming(
    base_url: &str,
    api_key: &Option<String>,
    body: &Value,
    _model: &str,
    client: &Client,
) -> Result<Response, ProxyError> {
    let url = format!("{}/v1/responses", base_url.trim_end_matches('/'));

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .json(body);

    if let Some(header) = api_key {
        request = request.header("Authorization", header.clone());
    }

    let response = request
        .send()
        .await
        .map_err(|e| ProxyError::RequestFailed(e.to_string()))?;

    if response.status() == 429 {
        return Err(ProxyError::RateLimited);
    }

    if response.status() == 400 {
        let status = response.status();
        let body_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        if is_encrypted_content_error(&body_text) {
            return Err(ProxyError::EncryptedContentError);
        }
        if is_summary_verification_error(&body_text) {
            return Err(ProxyError::SummaryVerificationError);
        }
        return Err(ProxyError::RequestFailed(format!(
            "Upstream {} : {}",
            status, body_text
        )));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(ProxyError::RequestFailed(format!(
            "Upstream {} : {}",
            status, body_text
        )));
    }

    let stream = response.bytes_stream().map(move |chunk| {
        chunk.map_err(std::io::Error::other)
    });

    let body = Body::from_stream(stream);

    let mut response_builder = Response::new(body);
    *response_builder.status_mut() = StatusCode::OK;
    response_builder.headers_mut().insert(
        "Content-Type",
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response_builder
        .headers_mut()
        .insert("Cache-Control", HeaderValue::from_static("no-cache"));
    response_builder
        .headers_mut()
        .insert("Connection", HeaderValue::from_static("keep-alive"));
    response_builder
        .headers_mut()
        .insert("X-Accel-Buffering", HeaderValue::from_static("no"));

    Ok(response_builder)
}

async fn forward_responses_non_streaming(
    base_url: &str,
    api_key: &Option<String>,
    body: &Value,
    _model: &str,
    client: &Client,
) -> Result<Response, ProxyError> {
    let url = format!("{}/v1/responses", base_url.trim_end_matches('/'));

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(body);

    if let Some(header) = api_key {
        request = request.header("Authorization", header.clone());
    }

    let response = request
        .send()
        .await
        .map_err(|e| ProxyError::RequestFailed(e.to_string()))?;

    if response.status() == 429 {
        return Err(ProxyError::RateLimited);
    }

    if response.status() == 400 {
        let status = response.status();
        let body_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        if is_encrypted_content_error(&body_text) {
            return Err(ProxyError::EncryptedContentError);
        }
        if is_summary_verification_error(&body_text) {
            return Err(ProxyError::SummaryVerificationError);
        }
        return Err(ProxyError::RequestFailed(format!(
            "Upstream {} : {}",
            status, body_text
        )));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(ProxyError::RequestFailed(format!(
            "Upstream {} : {}",
            status, body_text
        )));
    }

    let response_bytes = response
        .bytes()
        .await
        .map_err(|e| ProxyError::RequestFailed(e.to_string()))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        "Content-Type",
        HeaderValue::from_static("application/json"),
    );

    Ok((StatusCode::OK, headers, Body::from(response_bytes)).into_response())
}

pub async fn handle_models(
    State(state): State<Arc<Mutex<ProxyState>>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let caller_auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let state_guard = state.lock().await;
    let base_url = state_guard.config.opencode_base_url.clone();
    let proxy_api_key = state_guard.config.opencode_api_key.clone();
    let client = state_guard.client.clone();
    drop(state_guard);

    let auth = resolve_auth_header(caller_auth, proxy_api_key);

    let models_url = format!("{}/v1/models", base_url.trim_end_matches('/'));

    let mut request = client
        .get(&models_url)
        .header("Accept", "application/json");

    if let Some(header) = &auth {
        request = request.header("Authorization", header.clone());
    }

    match request.send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
            Ok(upstream_models) => (StatusCode::OK, Json(upstream_models)).into_response(),
            Err(e) => {
                error!("Failed to parse models response: {}", e);
                StatusCode::BAD_GATEWAY.into_response()
            }
        },
        Ok(resp) => {
            warn!("Upstream /v1/models returned status: {}", resp.status());
            StatusCode::BAD_GATEWAY.into_response()
        }
        Err(e) => {
            warn!("Failed to fetch models from upstream: {}", e);
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

pub async fn handle_model_detail(
    State(state): State<Arc<Mutex<ProxyState>>>,
    axum::extract::Path(model_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let caller_auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let state_guard = state.lock().await;
    let base_url = state_guard.config.opencode_base_url.clone();
    let proxy_api_key = state_guard.config.opencode_api_key.clone();
    let client = state_guard.client.clone();
    drop(state_guard);

    let auth = resolve_auth_header(caller_auth, proxy_api_key);

    let model_url = format!("{}/v1/models/{}", base_url.trim_end_matches('/'), model_id);

    let mut request = client
        .get(&model_url)
        .header("Accept", "application/json");

    if let Some(header) = &auth {
        request = request.header("Authorization", header.clone());
    }

    match request.send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
            Ok(model) => (StatusCode::OK, Json(model)).into_response(),
            Err(e) => {
                error!("Failed to parse model detail: {}", e);
                StatusCode::NOT_FOUND.into_response()
            }
        },
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn error_response(message: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "api_error",
            "code": "internal_error"
        }
    });
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(body),
    )
        .into_response()
}

#[derive(Debug)]
enum ProxyError {
    RateLimited,
    RequestFailed(String),
    EncryptedContentError,
    SummaryVerificationError,
}
