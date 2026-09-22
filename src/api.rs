use crate::backend::{Backend, BackendConfig, GenerationEvent};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State, rejection::JsonRejection},
    http::StatusCode,
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::sync::{Semaphore, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Clone)]
pub struct AppState {
    pub api_key: String,
    pub backend: BackendConfig,
    pub timeout: Duration,
    pub slots: Arc<Semaphore>,
    pub shutdown: CancellationToken,
    pub tasks: TaskTracker,
    pub models: Value,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/responses", post(responses))
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "Unknown endpoint") })
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}

pub fn valid_bearer(headers: &axum::http::HeaderMap, key: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|candidate| bool::from(candidate.as_bytes().ct_eq(key.as_bytes())))
}

async fn authenticate(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    if !valid_bearer(request.headers(), &state.api_key) {
        return ApiError::new(StatusCode::UNAUTHORIZED, "Invalid or missing proxy API key")
            .into_response();
    }
    next.run(request).await
}

async fn models(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(state.models.clone())
}

pub struct ApiError {
    status: StatusCode,
    message: String,
}
impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let kind = match self.status {
            StatusCode::UNAUTHORIZED => "authentication_error",
            StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
            status if status.is_client_error() => "invalid_request_error",
            _ => "server_error",
        };
        (self.status, Json(json!({"error":{"message":self.message,"type":kind,"param":null,"code":self.status.as_u16().to_string()}}))).into_response()
    }
}

pub struct Prompt {
    pub model: String,
    pub text: String,
    pub instructions: String,
    pub effort: Option<String>,
}
struct Parsed {
    prompt: Prompt,
    stream: bool,
    include_usage: bool,
}

fn text_content(value: &Value) -> Result<String, ApiError> {
    if let Some(text) = value.as_str() {
        return Ok(text.to_owned());
    }
    let parts = value
        .as_array()
        .ok_or_else(|| ApiError::bad("content must be text or an array of text parts"))?;
    let mut result = String::new();
    for part in parts {
        match part["type"].as_str() {
            Some("text" | "input_text" | "output_text") => result.push_str(
                part["text"]
                    .as_str()
                    .ok_or_else(|| ApiError::bad("Text part requires text"))?,
            ),
            _ => {
                return Err(ApiError::bad(
                    "Only text content is supported; images, audio and tool items are not supported",
                ));
            }
        }
    }
    Ok(result)
}

fn parse(value: Value, responses: bool) -> Result<Parsed, ApiError> {
    let object = value
        .as_object()
        .ok_or_else(|| ApiError::bad("Request must be a JSON object"))?;
    let allowed = if responses {
        &[
            "model",
            "input",
            "instructions",
            "stream",
            "reasoning",
            "store",
            "metadata",
            "user",
        ][..]
    } else {
        &[
            "model",
            "messages",
            "stream",
            "stream_options",
            "reasoning_effort",
            "n",
            "user",
        ][..]
    };
    for (key, item) in object {
        if !allowed.contains(&key.as_str()) && !item.is_null() {
            return Err(ApiError::bad(format!(
                "Unsupported parameter: {key}. gprox supports text generation only; see README."
            )));
        }
    }
    let model = value["model"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::bad("model is required; use GET /v1/models"))?
        .to_owned();
    let stream = match value.get("stream") {
        None => false,
        Some(v) => v
            .as_bool()
            .ok_or_else(|| ApiError::bad("stream must be boolean"))?,
    };
    if let Some(n) = value.get("n")
        && n != 1
    {
        return Err(ApiError::bad("Only n=1 is supported"));
    }
    if responses && value.get("store").is_some_and(|v| v != false) {
        return Err(ApiError::bad(
            "Only store=false is supported; responses are stateless",
        ));
    }
    let mut instructions = String::new();
    if let Some(v) = value.get("instructions") {
        instructions.push_str(
            v.as_str()
                .ok_or_else(|| ApiError::bad("instructions must be a string"))?,
        );
    }
    let input = if responses {
        &value["input"]
    } else {
        &value["messages"]
    };
    let mut history = Vec::new();
    if responses && input.is_string() {
        history.push(json!({"role":"user","content":input}));
    } else {
        let messages = input
            .as_array()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| ApiError::bad("A nonempty messages/input array is required"))?;
        for message in messages {
            if message.get("tool_calls").is_some() || message.get("function_call").is_some() {
                return Err(ApiError::bad("Tool calls are not supported"));
            }
            if message.get("type").is_some_and(|t| t != "message") {
                return Err(ApiError::bad("Only message input items are supported"));
            }
            let role = message["role"]
                .as_str()
                .ok_or_else(|| ApiError::bad("Each message requires a role"))?;
            let text = text_content(&message["content"])?;
            match role {
                "system" | "developer" => {
                    instructions.push('\n');
                    instructions.push_str(&text);
                }
                "user" | "assistant" => history.push(json!({"role":role,"content":text})),
                _ => return Err(ApiError::bad(format!("Unsupported message role: {role}"))),
            }
        }
    }
    if history.is_empty() {
        return Err(ApiError::bad(
            "At least one user or assistant message is required",
        ));
    }
    let effort = if responses {
        if let Some(reasoning) = value.get("reasoning") {
            let object = reasoning
                .as_object()
                .ok_or_else(|| ApiError::bad("reasoning must be an object"))?;
            if object.keys().any(|key| key != "effort") {
                return Err(ApiError::bad("Only reasoning.effort is supported"));
            }
        }
        value["reasoning"].get("effort")
    } else {
        value.get("reasoning_effort")
    };
    let effort = effort
        .map(|v| {
            v.as_str()
                .filter(|s| {
                    [
                        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
                    ]
                    .contains(s)
                })
                .map(str::to_owned)
                .ok_or_else(|| ApiError::bad("Invalid reasoning effort"))
        })
        .transpose()?;
    let mut include_usage = false;
    if let Some(options) = value.get("stream_options") {
        let obj = options
            .as_object()
            .ok_or_else(|| ApiError::bad("stream_options must be an object"))?;
        if obj.keys().any(|k| k != "include_usage") {
            return Err(ApiError::bad(
                "Only stream_options.include_usage is supported",
            ));
        }
        if let Some(v) = obj.get("include_usage") {
            include_usage = v
                .as_bool()
                .ok_or_else(|| ApiError::bad("include_usage must be boolean"))?;
        }
    }
    Ok(Parsed {
        prompt: Prompt {
            model,
            text: serde_json::to_string(&history).unwrap(),
            instructions,
            effort,
        },
        stream,
        include_usage,
    })
}

async fn chat(
    State(state): State<Arc<AppState>>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    generate(
        state,
        parse(body.map_err(|e| ApiError::bad(e.body_text()))?.0, false)?,
        false,
    )
    .await
}
async fn responses(
    State(state): State<Arc<AppState>>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    generate(
        state,
        parse(body.map_err(|e| ApiError::bad(e.body_text()))?.0, true)?,
        true,
    )
    .await
}

async fn generate(
    state: Arc<AppState>,
    parsed: Parsed,
    responses: bool,
) -> Result<Response, ApiError> {
    let permit =
        state.slots.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "Proxy is busy; retry later")
        })?;
    if state.shutdown.is_cancelled() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Proxy is stopping",
        ));
    }
    let model = parsed.prompt.model.clone();
    let (tx, mut rx) = mpsc::channel(32);
    let worker = state.clone();
    state.tasks.spawn(async move {
        let _permit = permit;
        let operation = async {
            let mut backend = Backend::connect(&worker.backend).await?;
            backend
                .generate(parsed.prompt, &worker.backend.workspace, &tx)
                .await
        };
        let outcome = tokio::select! {
            _ = tx.closed() => return,
            _ = worker.shutdown.cancelled() => Err(anyhow::anyhow!("Proxy is stopping")),
            result = tokio::time::timeout(worker.timeout, operation) => match result {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("Codex request timed out")),
            },
        };
        if let Err(error) = outcome {
            let _ = tx.send(GenerationEvent::Error(error.to_string())).await;
        }
    });
    let id = format!(
        "{}{}",
        if responses { "resp_" } else { "chatcmpl-" },
        uuid::Uuid::new_v4().simple()
    );
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if !parsed.stream {
        let mut text = String::new();
        while let Some(event) = rx.recv().await {
            match event {
                GenerationEvent::Delta(delta) => text.push_str(&delta),
                GenerationEvent::Done(usage) => {
                    let response = if responses {
                        response_object(&id, created, &model, &text, "completed", &usage)
                    } else {
                        json!({
                            "id": id,
                            "object": "chat.completion",
                            "created": created,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": text},
                                "finish_reason": "stop",
                            }],
                            "usage": chat_usage(&usage),
                        })
                    };
                    return Ok(Json(response).into_response());
                }
                GenerationEvent::Error(error) => {
                    let status = if error.contains("timed out") {
                        StatusCode::GATEWAY_TIMEOUT
                    } else {
                        StatusCode::BAD_GATEWAY
                    };
                    return Err(ApiError::new(status, error));
                }
            }
        }
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "Codex closed without completing the response",
        ));
    }
    let (events, receiver) = mpsc::channel::<Result<Event, Infallible>>(32);
    state.tasks.spawn(async move {
        let mut sequence = 0;
        let mut accumulated = String::new();
        let mut initial = vec![];
        if responses {
            initial.push(json!({"type":"response.created","response":response_object(&id,created,&model,"","in_progress",&Value::Null)}));
            initial.push(json!({"type":"response.in_progress","response":response_object(&id,created,&model,"","in_progress",&Value::Null)}));
            initial.push(json!({"type":"response.output_item.added","output_index":0,"item":{"id":format!("msg_{id}"),"type":"message","role":"assistant","status":"in_progress","content":[]}}));
            initial.push(json!({"type":"response.content_part.added","item_id":format!("msg_{id}"),"output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}));
        } else { initial.push(chunk(&id,created,&model,json!({"role":"assistant","content":""}),Value::Null)); }
        for value in initial { if emit(&events,value,responses,&mut sequence).await.is_err() { return; } }
        loop {
            let event = tokio::select! { _ = events.closed() => return, event = rx.recv() => event };
            let Some(event) = event else { return; };
            match event {
                GenerationEvent::Delta(delta) => {
                    if responses {
                        accumulated.push_str(&delta);
                    }
                    let value = if responses { json!({"type":"response.output_text.delta","item_id":format!("msg_{id}"),"output_index":0,"content_index":0,"delta":delta,"logprobs":[]}) }
                        else { chunk(&id,created,&model,json!({"content":delta}),Value::Null) };
                    if emit(&events,value,responses,&mut sequence).await.is_err() { return; }
                }
                GenerationEvent::Done(usage) => {
                    if responses {
                        let response = response_object(&id,created,&model,&accumulated,"completed",&usage);
                        let part = response["output"][0]["content"][0].clone();
                        for value in [
                            json!({"type":"response.output_text.done","item_id":format!("msg_{id}"),"output_index":0,"content_index":0,"text":accumulated,"logprobs":[]}),
                            json!({"type":"response.content_part.done","item_id":format!("msg_{id}"),"output_index":0,"content_index":0,"part":part}),
                            json!({"type":"response.output_item.done","output_index":0,"item":response["output"][0]}),
                            json!({"type":"response.completed","response":response}),
                        ] { if emit(&events,value,true,&mut sequence).await.is_err() { return; } }
                    } else {
                        if emit(&events,chunk(&id,created,&model,json!({}),json!("stop")),false,&mut sequence).await.is_err() { return; }
                        if parsed.include_usage {
                            let value = json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[],"usage":chat_usage(&usage)});
                            if emit(&events,value,false,&mut sequence).await.is_err() { return; }
                        }
                        let _ = events.send(Ok(Event::default().data("[DONE]"))).await;
                    }
                    return;
                }
                GenerationEvent::Error(message) => {
                    let value = if responses {
                        let mut response = response_object(&id,created,&model,&accumulated,"failed",&Value::Null);
                        response["error"] = json!({"code":"server_error","message":message});
                        json!({"type":"response.failed","response":response})
                    } else { json!({"error":{"type":"server_error","code":"backend_error","message":message}}) };
                    let _ = emit(&events,value,responses,&mut sequence).await;
                    return;
                }
            }
        }
    });
    Ok(Sse::new(ReceiverStream::new(receiver))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(10)))
        .into_response())
}

async fn emit(
    sender: &mpsc::Sender<Result<Event, Infallible>>,
    mut value: Value,
    named: bool,
    sequence: &mut u64,
) -> Result<(), ()> {
    let mut event = Event::default();
    if named {
        value["sequence_number"] = json!(*sequence);
        *sequence += 1;
        event = event.event(value["type"].as_str().unwrap_or("error"));
    }
    sender
        .send(Ok(event.data(value.to_string())))
        .await
        .map_err(|_| ())
}

fn chunk(id: &str, created: u64, model: &str, delta: Value, finish: Value) -> Value {
    json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
}
fn chat_usage(usage: &Value) -> Value {
    if usage.is_null() {
        return Value::Null;
    }
    json!({"prompt_tokens":usage["inputTokens"],"completion_tokens":usage["outputTokens"],"total_tokens":usage["totalTokens"],
        "prompt_tokens_details":{"cached_tokens":usage["cachedInputTokens"]},"completion_tokens_details":{"reasoning_tokens":usage["reasoningOutputTokens"]}})
}
fn response_object(
    id: &str,
    created: u64,
    model: &str,
    text: &str,
    status: &str,
    usage: &Value,
) -> Value {
    let output = if status == "in_progress" {
        json!([])
    } else {
        let item_status = if status == "completed" {
            "completed"
        } else {
            "incomplete"
        };
        json!([{
            "id": format!("msg_{id}"),
            "type": "message",
            "role": "assistant",
            "status": item_status,
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": [],
                "logprobs": [],
            }],
        }])
    };
    let usage = if usage.is_null() {
        Value::Null
    } else {
        json!({
            "input_tokens": usage["inputTokens"],
            "output_tokens": usage["outputTokens"],
            "total_tokens": usage["totalTokens"],
            "input_tokens_details": {"cached_tokens": usage["cachedInputTokens"]},
            "output_tokens_details": {"reasoning_tokens": usage["reasoningOutputTokens"]},
        })
    };
    json!({
        "id": id,
        "object": "response",
        "created_at": created,
        "model": model,
        "status": status,
        "error": null,
        "incomplete_details": null,
        "output": output,
        "parallel_tool_calls": false,
        "tools": [],
        "tool_choice": "none",
        "store": false,
        "usage": usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    #[test]
    fn preserves_history_and_separates_instructions() {
        let parsed = parse(json!({"model":"test","messages":[{"role":"system","content":"Be brief"},{"role":"user","content":"Hello"},{"role":"assistant","content":"Hi"},{"role":"user","content":[{"type":"text","text":"Again"}]}]}),false).ok().unwrap();
        assert!(parsed.prompt.instructions.contains("Be brief"));
        let history: Value = serde_json::from_str(&parsed.prompt.text).unwrap();
        assert_eq!(history.as_array().unwrap().len(), 3);
        assert_eq!(history[1]["role"], "assistant");
    }
    #[test]
    fn rejects_unsupported_options_instead_of_silently_ignoring_them() {
        for extra in [
            json!({"tools":[]}),
            json!({"temperature":0.2}),
            json!({"max_tokens":12}),
            json!({"n":2}),
            json!({"stream":"true"}),
        ] {
            let mut value = json!({"model":"test","messages":[{"role":"user","content":"hi"}]});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            assert!(parse(value, false).is_err());
        }
        assert!(parse(json!({"model":"test","input":"hi","store":true}), true).is_err());
        assert!(
            parse(
                json!({"model":"test","input":"hi","previous_response_id":"resp_1"}),
                true
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn endpoints_require_key_and_models_are_openai_shaped() {
        let state = Arc::new(AppState {
            api_key: "test-key".into(),
            backend: BackendConfig {
                executable: "missing".into(),
                workspace: ".".into(),
            },
            timeout: Duration::from_secs(1),
            slots: Arc::new(Semaphore::new(1)),
            shutdown: CancellationToken::new(),
            tasks: TaskTracker::new(),
            models: json!({"object":"list","data":[]}),
        });
        let app = router(state);
        for endpoint in ["/v1/models", "/health", "/missing"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header("authorization", "Bearer test-key")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
