use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State, WebSocketUpgrade},
    http::{HeaderMap, Method, Response, StatusCode, Uri},
    response::IntoResponse,
    routing::{any, get, post, put},
};
use cel::{Context as CelContext, Program, Value as CelValue, to_value as cel_to_value};
use chrono::{DateTime, Utc};
use clap::{Parser, ValueEnum};
use futures::stream::StreamExt;
use regex::Regex;
use reqwest::header::HeaderName;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast, oneshot};
use tower_http::cors::CorsLayer;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "replayr")]
#[command(about = "API record/replay/simulation proxy", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    Proxy(ProxyArgs),
}

#[derive(Parser, Debug, Clone)]
struct ProxyArgs {
    #[arg(long)]
    upstream: String,
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,
    #[arg(long, default_value_t = 9090)]
    port: u16,
    #[arg(long)]
    ui: bool,
    #[arg(long, default_value_t = 9091)]
    admin_port: u16,
    #[arg(long, value_enum, default_value_t = LogLevel::Summary)]
    log: LogLevel,
    #[arg(long)]
    filter: Option<String>,
    #[arg(long, default_value_t = 1000)]
    ring_size: usize,
    #[arg(long)]
    record: bool,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    modify_header: Vec<String>,
    #[arg(long)]
    delete_header: Vec<String>,
    #[arg(long)]
    modify_body: Option<String>,
    #[arg(long)]
    intercept: Option<String>,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum LogLevel {
    None,
    Summary,
    Headers,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Interaction {
    id: String,
    recorded_at: DateTime<Utc>,
    request: StoredRequest,
    response: StoredResponse,
    metadata: Metadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredResponse {
    status: u16,
    headers: HashMap<String, String>,
    streaming: bool,
    chunks: Vec<Chunk>,
    body: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Chunk {
    delay_ms: u128,
    data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Metadata {
    provider: Option<String>,
    model: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    latency_ms: u128,
    latency_to_first_chunk_ms: Option<u128>,
}

#[derive(Debug)]
struct RecordState {
    enabled: bool,
    output: PathBuf,
    count: usize,
}

#[derive(Debug)]
struct InterceptEntry {
    request: StoredRequest,
    sender: Option<oneshot::Sender<InterceptAction>>,
}

#[derive(Debug)]
enum InterceptAction {
    Release {
        headers: Option<HashMap<String, String>>,
        body: Option<String>,
    },
    Drop,
}

#[derive(Debug)]
struct BodyModifier {
    regex: Regex,
    replacement: String,
}

#[derive(Clone)]
struct AppState {
    args: ProxyArgs,
    client: reqwest::Client,
    ring: Arc<Mutex<VecDeque<Interaction>>>,
    broadcaster: broadcast::Sender<Interaction>,
    record: Arc<Mutex<RecordState>>,
    intercept_pattern: Arc<Mutex<Option<String>>>,
    intercept_queue: Arc<Mutex<HashMap<String, InterceptEntry>>>,
    body_modifier: Option<Arc<BodyModifier>>,
    header_sets: Arc<HashMap<String, String>>,
    header_deletes: Arc<Vec<String>>,
}

#[derive(Deserialize)]
struct SaveRequest {
    path: String,
    ids: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RecordToggleRequest {
    enabled: bool,
    output: Option<String>,
}

#[derive(Deserialize)]
struct InterceptPatternRequest {
    pattern: Option<String>,
}

#[derive(Deserialize)]
struct ReleaseRequest {
    headers: Option<HashMap<String, String>>,
    body: Option<String>,
}

#[derive(Deserialize)]
struct RequestsQuery {
    filter: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Proxy(args) => run_proxy(args).await,
    }
}

async fn run_proxy(args: ProxyArgs) -> Result<()> {
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from("./session.json"));
    let body_modifier = if let Some(raw) = &args.modify_body {
        Some(Arc::new(parse_body_modifier(raw)?))
    } else {
        None
    };

    let (tx, _) = broadcast::channel(1024);
    let state = AppState {
        args: args.clone(),
        client: reqwest::Client::builder().build()?,
        ring: Arc::new(Mutex::new(VecDeque::with_capacity(args.ring_size))),
        broadcaster: tx,
        record: Arc::new(Mutex::new(RecordState {
            enabled: args.record,
            output,
            count: 0,
        })),
        intercept_pattern: Arc::new(Mutex::new(args.intercept.clone())),
        intercept_queue: Arc::new(Mutex::new(HashMap::new())),
        body_modifier,
        header_sets: Arc::new(parse_set_headers(&args.modify_header)),
        header_deletes: Arc::new(
            args.delete_header
                .iter()
                .map(|x| x.to_ascii_lowercase())
                .collect(),
        ),
    };

    let proxy_router = Router::new()
        .route("/", any(proxy_handler))
        .route("/*path", any(proxy_handler))
        .with_state(state.clone());

    let mut admin_router = Router::new()
        .route("/api/v1/health", get(health_handler))
        .route(
            "/api/v1/requests",
            get(list_requests_handler).delete(clear_requests_handler),
        )
        .route("/api/v1/requests/:id", get(get_request_handler))
        .route("/api/v1/requests/save", post(save_requests_handler))
        .route("/api/v1/requests/:id/replay", post(replay_request_handler))
        .route("/api/v1/requests/:id/curl", post(curl_request_handler))
        .route(
            "/api/v1/record",
            get(get_record_handler).put(toggle_record_handler),
        )
        .route("/api/v1/intercept", put(set_intercept_pattern_handler))
        .route("/api/v1/intercept/queue", get(intercept_queue_handler))
        .route(
            "/api/v1/intercept/:id/release",
            post(release_intercept_handler),
        )
        .route("/api/v1/intercept/:id/drop", post(drop_intercept_handler))
        .route("/api/v1/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    if args.ui {
        admin_router = admin_router
            .route("/", get(ui_index_handler))
            .route("/app.js", get(ui_js_handler))
            .route("/style.css", get(ui_css_handler));
    }

    let proxy_addr = format!("{}:{}", args.bind, args.port)
        .parse::<SocketAddr>()
        .context("invalid --bind or --port value")?;
    let admin_addr = format!("{}:{}", args.bind, args.admin_port)
        .parse::<SocketAddr>()
        .context("invalid --bind or --admin-port value")?;
    println!("proxy listening on http://{}", proxy_addr);
    println!("admin listening on http://{}", admin_addr);

    let proxy_listener = tokio::net::TcpListener::bind(proxy_addr).await?;
    let admin_listener = tokio::net::TcpListener::bind(admin_addr).await?;
    tokio::try_join!(
        axum::serve(proxy_listener, proxy_router),
        axum::serve(admin_listener, admin_router),
    )?;
    Ok(())
}

async fn proxy_handler(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    match proxy_handler_impl(state, method, uri, headers, body).await {
        Ok(resp) => resp,
        Err(err) => {
            let payload = json!({"error": err.to_string()});
            (StatusCode::BAD_GATEWAY, Json(payload)).into_response()
        }
    }
}

async fn proxy_handler_impl(
    state: AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>> {
    let start = Instant::now();
    let path_and_query = uri
        .path_and_query()
        .map(|v| v.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());

    let mut outgoing_headers = headers_to_map(&headers);
    for (k, v) in state.header_sets.iter() {
        outgoing_headers.insert(k.clone(), v.clone());
    }
    for name in state.header_deletes.iter() {
        outgoing_headers.remove(name);
    }

    let mut request_body = bytes_to_value(&body);
    if let Some(modifier) = &state.body_modifier
        && let Some(updated) = apply_modifier(&request_body, modifier)
    {
        request_body = updated;
    }

    let mut stored_req = StoredRequest {
        method: method.to_string(),
        path: uri.path().to_string(),
        headers: outgoing_headers.clone(),
        body: request_body.clone(),
    };

    if let Some(action) = maybe_intercept(&state, &stored_req).await {
        match action {
            InterceptAction::Drop => {
                return Ok((StatusCode::NO_CONTENT, Body::empty()).into_response());
            }
            InterceptAction::Release { headers, body } => {
                if let Some(h) = headers {
                    stored_req.headers = h;
                }
                if let Some(b) = body {
                    stored_req.body = text_to_json_or_string(&b);
                }
            }
        }
    }

    let upstream_url = format!(
        "{}{}",
        state.args.upstream.trim_end_matches('/'),
        path_and_query
    );
    let mut req = state.client.request(method.clone(), &upstream_url);

    for (k, v) in &stored_req.headers {
        if k == "host" || k == "content-length" {
            continue;
        }
        if let Ok(name) = HeaderName::from_bytes(k.as_bytes()) {
            req = req.header(name, v);
        }
    }

    let req_body_string = json_value_to_body_string(&stored_req.body);
    req = req.body(req_body_string.clone());

    let upstream_resp = req.send().await.context("failed to call upstream")?;
    let status = upstream_resp.status();
    let response_headers = headers_to_map(upstream_resp.headers());
    let mut response_headers_redacted = response_headers.clone();
    redact_headers(&mut response_headers_redacted);
    let streaming = response_headers
        .get("content-type")
        .map(|v| v.contains("text/event-stream"))
        .unwrap_or(false);

    let mut metadata = detect_provider(&stored_req.path, &stored_req.headers);
    metadata.model = extract_model(&stored_req.body);
    metadata.latency_ms = 0;

    let mut response_builder = Response::builder().status(status);
    for (k, v) in response_headers {
        response_builder = response_builder.header(k, v);
    }

    if streaming {
        let mut stream = upstream_resp.bytes_stream();
        let state_clone = state.clone();
        let request_for_log = stored_req.clone();
        let headers_for_log = response_headers_redacted.clone();
        let log_level = state.args.log;
        let filter = state.args.filter.clone();
        let body_modifier = state.body_modifier.clone();
        let start_inner = start;

        let output = async_stream::stream! {
            let mut chunks = Vec::new();
            let mut merged = String::new();
            let mut last_chunk = Instant::now();
            let mut first_chunk_latency = None;
            while let Some(item) = stream.next().await {
                if let Ok(bytes) = item {
                    let now = Instant::now();
                    let delay = now.duration_since(last_chunk).as_millis();
                    last_chunk = now;
                    let text = String::from_utf8_lossy(&bytes).to_string();
                    if first_chunk_latency.is_none() {
                        first_chunk_latency = Some(start_inner.elapsed().as_millis());
                    }
                    let mut out = text.clone();
                    if let Some(m) = &body_modifier {
                        out = m.regex.replace_all(&out, m.replacement.as_str()).to_string();
                    }
                    merged.push_str(&out);
                    chunks.push(Chunk { delay_ms: delay, data: out.clone() });
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(out));
                }
            }
            metadata.latency_ms = start_inner.elapsed().as_millis();
            metadata.latency_to_first_chunk_ms = first_chunk_latency;
            extract_usage_tokens(&mut metadata, &merged);
            let interaction = Interaction {
                id: Uuid::new_v4().to_string(),
                recorded_at: Utc::now(),
                request: request_for_log,
                response: StoredResponse {
                    status: status.as_u16(),
                    headers: headers_for_log,
                    streaming: true,
                    chunks,
                    body: None,
                },
                metadata,
            };
            store_interaction(state_clone, interaction, log_level, filter).await;
        };

        let body = Body::from_stream(output);
        return Ok(response_builder.body(body)?);
    }

    let resp_bytes = upstream_resp.bytes().await?;
    let mut body_text = String::from_utf8_lossy(&resp_bytes).to_string();
    if let Some(modifier) = &state.body_modifier {
        body_text = modifier
            .regex
            .replace_all(&body_text, modifier.replacement.as_str())
            .to_string();
    }

    metadata.latency_ms = start.elapsed().as_millis();
    extract_usage_tokens(&mut metadata, &body_text);

    let request_for_log = stored_req.clone();

    let interaction = Interaction {
        id: Uuid::new_v4().to_string(),
        recorded_at: Utc::now(),
        request: request_for_log,
        response: StoredResponse {
            status: status.as_u16(),
            headers: response_headers_redacted,
            streaming: false,
            chunks: Vec::new(),
            body: Some(text_to_json_or_string(&body_text)),
        },
        metadata,
    };
    let body_for_client = body_text.clone();
    store_interaction(
        state.clone(),
        interaction,
        state.args.log,
        state.args.filter.clone(),
    )
    .await;
    Ok(response_builder.body(Body::from(body_for_client))?)
}

async fn maybe_intercept(state: &AppState, req: &StoredRequest) -> Option<InterceptAction> {
    let pattern = state.intercept_pattern.lock().await.clone();
    if let Some(pattern) = pattern {
        let fake = Interaction {
            id: String::new(),
            recorded_at: Utc::now(),
            request: req.clone(),
            response: StoredResponse {
                status: 0,
                headers: HashMap::new(),
                streaming: false,
                chunks: Vec::new(),
                body: None,
            },
            metadata: Metadata::default(),
        };
        if evaluate_expression(&pattern, &fake) {
            let id = Uuid::new_v4().to_string();
            let (tx, rx) = oneshot::channel::<InterceptAction>();
            {
                let mut queue = state.intercept_queue.lock().await;
                queue.insert(
                    id,
                    InterceptEntry {
                        request: {
                            let mut request = req.clone();
                            redact_headers(&mut request.headers);
                            request
                        },
                        sender: Some(tx),
                    },
                );
            }
            return match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
                Ok(Ok(action)) => Some(action),
                _ => Some(InterceptAction::Drop),
            };
        }
    }
    None
}

async fn store_interaction(
    state: AppState,
    interaction: Interaction,
    log_level: LogLevel,
    filter: Option<String>,
) {
    {
        let mut ring = state.ring.lock().await;
        ring.push_front(interaction.clone());
        if ring.len() > state.args.ring_size {
            ring.pop_back();
        }
    }

    let _ = state.broadcaster.send(interaction.clone());

    if should_log(&interaction, &filter) {
        print_log(&interaction, log_level);
    }

    let record = state.record.lock().await;
    if record.enabled {
        let path = record.output.clone();
        drop(record);
        if write_cassette(&state, &path, None).await.is_ok() {
            let mut record = state.record.lock().await;
            record.count += 1;
        }
    }
}

fn print_log(interaction: &Interaction, level: LogLevel) {
    match level {
        LogLevel::None => {}
        LogLevel::Summary => {
            println!(
                "{} {} -> {} ({}ms{}{})",
                interaction.request.method,
                interaction.request.path,
                interaction.response.status,
                interaction.metadata.latency_ms,
                interaction
                    .metadata
                    .total_tokens
                    .map(|v| format!(", {} tokens", v))
                    .unwrap_or_default(),
                interaction
                    .metadata
                    .model
                    .as_ref()
                    .map(|m| format!(", {}", m))
                    .unwrap_or_default()
            );
        }
        LogLevel::Headers => {
            println!(
                "{} {} -> {}\nreq headers: {:?}\nresp headers: {:?}",
                interaction.request.method,
                interaction.request.path,
                interaction.response.status,
                interaction.request.headers,
                interaction.response.headers
            );
        }
        LogLevel::Full => {
            println!(
                "{} {} -> {}\nreq: {}\nresp: {}",
                interaction.request.method,
                interaction.request.path,
                interaction.response.status,
                interaction.request.body,
                interaction
                    .response
                    .body
                    .clone()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "<streaming>".to_string())
            );
        }
    }
}

fn should_log(interaction: &Interaction, filter: &Option<String>) -> bool {
    if let Some(f) = filter {
        return evaluate_expression(f, interaction);
    }
    true
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

async fn list_requests_handler(
    State(state): State<AppState>,
    Query(query): Query<RequestsQuery>,
) -> impl IntoResponse {
    let ring = state.ring.lock().await;
    let mut items: Vec<Interaction> = ring.iter().map(redact_interaction).collect();
    if let Some(filter) = query.filter {
        items.retain(|i| evaluate_expression(&filter, i));
    }
    Json(items)
}

async fn get_request_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let ring = state.ring.lock().await;
    if let Some(item) = ring.iter().find(|x| x.id == id) {
        return (StatusCode::OK, Json(redact_interaction(item))).into_response();
    }
    (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response()
}

async fn clear_requests_handler(State(state): State<AppState>) -> impl IntoResponse {
    let mut ring = state.ring.lock().await;
    ring.clear();
    Json(json!({"ok": true}))
}

async fn save_requests_handler(
    State(state): State<AppState>,
    Json(input): Json<SaveRequest>,
) -> impl IntoResponse {
    match write_cassette(&state, &PathBuf::from(input.path), input.ids).await {
        Ok(saved) => (StatusCode::OK, Json(json!({"saved": saved}))).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn replay_request_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let maybe = {
        let ring = state.ring.lock().await;
        ring.iter().find(|x| x.id == id).cloned()
    };

    let Some(item) = maybe else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    };

    let url = format!(
        "{}{}",
        state.args.upstream.trim_end_matches('/'),
        item.request.path
    );
    let mut req = state.client.request(
        item.request.method.parse::<Method>().unwrap_or(Method::GET),
        url,
    );
    for (k, v) in &item.request.headers {
        if k == "host" || k == "content-length" {
            continue;
        }
        req = req.header(k, v);
    }
    req = req.body(json_value_to_body_string(&item.request.body));

    match req.send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            (StatusCode::OK, Json(json!({"status": code}))).into_response()
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn curl_request_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let maybe = {
        let ring = state.ring.lock().await;
        ring.iter().find(|x| x.id == id).cloned()
    };
    let Some(item) = maybe else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    };

    let mut cmd = format!(
        "curl -X {} '{}{}'",
        item.request.method,
        state.args.upstream.trim_end_matches('/'),
        item.request.path
    );
    for (k, v) in &item.request.headers {
        cmd.push_str(&format!(" -H '{}: {}'", k, v));
    }
    let body = json_value_to_body_string(&item.request.body).replace('"', "\\\"");
    if !body.is_empty() {
        cmd.push_str(&format!(" --data \"{}\"", body));
    }
    (StatusCode::OK, Json(json!({"curl": cmd}))).into_response()
}

async fn toggle_record_handler(
    State(state): State<AppState>,
    Json(input): Json<RecordToggleRequest>,
) -> impl IntoResponse {
    let mut record = state.record.lock().await;
    record.enabled = input.enabled;
    if let Some(output) = input.output {
        record.output = PathBuf::from(output);
    }
    Json(json!({
        "enabled": record.enabled,
        "output": record.output,
        "count": record.count
    }))
}

async fn get_record_handler(State(state): State<AppState>) -> impl IntoResponse {
    let record = state.record.lock().await;
    Json(json!({
        "enabled": record.enabled,
        "output": record.output,
        "count": record.count
    }))
}

async fn set_intercept_pattern_handler(
    State(state): State<AppState>,
    Json(input): Json<InterceptPatternRequest>,
) -> impl IntoResponse {
    let mut pattern = state.intercept_pattern.lock().await;
    *pattern = input.pattern;
    Json(json!({"pattern": *pattern}))
}

async fn intercept_queue_handler(State(state): State<AppState>) -> impl IntoResponse {
    let queue = state.intercept_queue.lock().await;
    let items = queue
        .iter()
        .map(|(id, entry)| {
            json!({
                "id": id,
                "method": entry.request.method,
                "path": entry.request.path,
                "headers": entry.request.headers,
                "body": entry.request.body,
            })
        })
        .collect::<Vec<_>>();
    Json(items)
}

async fn release_intercept_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ReleaseRequest>,
) -> impl IntoResponse {
    let mut queue = state.intercept_queue.lock().await;
    let Some(mut entry) = queue.remove(&id) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    };
    if let Some(sender) = entry.sender.take() {
        let _ = sender.send(InterceptAction::Release {
            headers: input.headers,
            body: input.body,
        });
    }
    Json(json!({"released": id})).into_response()
}

async fn drop_intercept_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let mut queue = state.intercept_queue.lock().await;
    let Some(mut entry) = queue.remove(&id) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    };
    if let Some(sender) = entry.sender.take() {
        let _ = sender.send(InterceptAction::Drop);
    }
    Json(json!({"dropped": id})).into_response()
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(socket, state))
}

async fn ws_session(mut socket: axum::extract::ws::WebSocket, state: AppState) {
    let mut rx = state.broadcaster.subscribe();
    loop {
        let msg = rx.recv().await;
        match msg {
            Ok(interaction) => {
                let payload = serde_json::to_string(&redact_interaction(&interaction))
                    .unwrap_or_else(|_| "{}".to_string());
                if socket
                    .send(axum::extract::ws::Message::Text(payload.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

async fn ui_index_handler() -> impl IntoResponse {
    Response::builder()
        .header("content-type", "text/html; charset=utf-8")
        .body(Body::from(INDEX_HTML))
        .unwrap()
}

async fn ui_js_handler() -> impl IntoResponse {
    Response::builder()
        .header("content-type", "application/javascript; charset=utf-8")
        .body(Body::from(APP_JS))
        .unwrap()
}

async fn ui_css_handler() -> impl IntoResponse {
    Response::builder()
        .header("content-type", "text/css; charset=utf-8")
        .body(Body::from(STYLE_CSS))
        .unwrap()
}

async fn write_cassette(
    state: &AppState,
    path: &PathBuf,
    ids: Option<Vec<String>>,
) -> Result<usize> {
    let ring = state.ring.lock().await;
    let mut interactions: Vec<Interaction> = ring.iter().map(redact_interaction).collect();
    if let Some(ids) = ids {
        interactions.retain(|i| ids.contains(&i.id));
    }
    let payload = json!({
        "replayr_version": "1",
        "cassette": {
            "id": Uuid::new_v4().to_string(),
            "name": "session",
            "created_at": Utc::now(),
            "upstream": state.args.upstream,
        },
        "interactions": interactions,
    });
    let text = serde_json::to_string_pretty(&payload)?;
    tokio::fs::write(path, text).await?;
    Ok(payload["interactions"]
        .as_array()
        .map(|x| x.len())
        .unwrap_or(0))
}

fn parse_set_headers(items: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for item in items {
        if let Some((name, value)) = item.split_once(':') {
            out.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    out
}

fn parse_body_modifier(raw: &str) -> Result<BodyModifier> {
    let mut chars = raw.chars();
    let sep = chars.next().context("empty modify-body expression")?;
    let parts = raw[1..].split(sep).collect::<Vec<_>>();
    if parts.len() < 2 {
        anyhow::bail!("invalid modify-body expression");
    }
    Ok(BodyModifier {
        regex: Regex::new(parts[0]).context("invalid regex")?,
        replacement: parts[1].to_string(),
    })
}

fn apply_modifier(value: &Value, modifier: &BodyModifier) -> Option<Value> {
    let raw = json_value_to_body_string(value);
    let updated = modifier
        .regex
        .replace_all(&raw, modifier.replacement.as_str())
        .to_string();
    if updated == raw {
        None
    } else {
        Some(text_to_json_or_string(&updated))
    }
}

fn json_value_to_body_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        _ => value.to_string(),
    }
}

fn bytes_to_value(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }
    let text = String::from_utf8_lossy(bytes).to_string();
    text_to_json_or_string(&text)
}

fn text_to_json_or_string(text: &str) -> Value {
    serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

fn headers_to_map(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_ascii_lowercase(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn redact_headers(headers: &mut HashMap<String, String>) {
    for key in ["authorization", "x-api-key", "api-key"] {
        if headers.contains_key(key) {
            headers.insert(key.to_string(), "REDACTED".to_string());
        }
    }
}

fn redact_interaction(interaction: &Interaction) -> Interaction {
    let mut out = interaction.clone();
    redact_headers(&mut out.request.headers);
    redact_headers(&mut out.response.headers);
    out
}

fn detect_provider(path: &str, headers: &HashMap<String, String>) -> Metadata {
    let provider = if path.contains("/v1/messages") && headers.contains_key("x-api-key") {
        Some("anthropic".to_string())
    } else if path.contains("/v1/chat/completions")
        && headers
            .get("authorization")
            .map(|v| v.to_ascii_lowercase().starts_with("bearer "))
            .unwrap_or(false)
    {
        Some("openai".to_string())
    } else {
        None
    };
    Metadata {
        provider,
        ..Metadata::default()
    }
}

fn extract_model(body: &Value) -> Option<String> {
    body.as_object()
        .and_then(|obj| obj.get("model"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
}

fn extract_usage_tokens(metadata: &mut Metadata, body: &str) {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if let Some(usage) = value.get("usage") {
            let input = usage
                .get("input_tokens")
                .or_else(|| usage.get("prompt_tokens"))
                .and_then(|v| v.as_u64());
            let output = usage
                .get("output_tokens")
                .or_else(|| usage.get("completion_tokens"))
                .and_then(|v| v.as_u64());
            metadata.input_tokens = input;
            metadata.output_tokens = output;
            metadata.total_tokens = match (input, output) {
                (Some(i), Some(o)) => Some(i + o),
                _ => None,
            };
        }
        return;
    }
    if let Some((i, o)) = extract_tokens_from_sse(body) {
        metadata.input_tokens = Some(i);
        metadata.output_tokens = Some(o);
        metadata.total_tokens = Some(i + o);
    }
}

fn extract_tokens_from_sse(body: &str) -> Option<(u64, u64)> {
    let input_re = Regex::new(r#"\"input_tokens\"\s*:\s*(\d+)"#).ok()?;
    let output_re = Regex::new(r#"\"output_tokens\"\s*:\s*(\d+)"#).ok()?;
    let input = input_re
        .captures_iter(body)
        .last()
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u64>().ok());
    let output = output_re
        .captures_iter(body)
        .last()
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u64>().ok());
    match (input, output) {
        (Some(i), Some(o)) => Some((i, o)),
        _ => None,
    }
}

fn evaluate_expression(expr: &str, interaction: &Interaction) -> bool {
    let Ok(program) = Program::compile(expr) else {
        return false;
    };

    let mut context = CelContext::default();
    let request = json!({
        "method": &interaction.request.method,
        "path": &interaction.request.path,
        "headers": &interaction.request.headers,
        "body": &interaction.request.body,
    });
    let response = json!({
        "status": interaction.response.status,
        "headers": &interaction.response.headers,
        "body": &interaction.response.body,
        "streaming": interaction.response.streaming,
    });
    let metadata = json!({
        "provider": &interaction.metadata.provider,
        "model": &interaction.metadata.model,
        "input_tokens": interaction.metadata.input_tokens,
        "output_tokens": interaction.metadata.output_tokens,
        "total_tokens": interaction.metadata.total_tokens,
        "latency_ms": interaction.metadata.latency_ms,
        "latency_to_first_chunk_ms": interaction.metadata.latency_to_first_chunk_ms,
    });

    let Ok(request_value) = cel_to_value(request) else {
        return false;
    };
    let Ok(response_value) = cel_to_value(response) else {
        return false;
    };
    let Ok(metadata_value) = cel_to_value(metadata) else {
        return false;
    };

    context.add_variable_from_value("request", request_value);
    context.add_variable_from_value("response", response_value);
    context.add_variable_from_value("metadata", metadata_value);

    match program.execute(&context) {
        Ok(CelValue::Bool(value)) => value,
        _ => false,
    }
}

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_JS: &str = include_str!("../ui/app.js");
const STYLE_CSS: &str = include_str!("../ui/style.css");

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::to_bytes, routing::get, routing::post};
    use tempfile::tempdir;

    async fn spawn_upstream() -> SocketAddr {
        let app = Router::new()
            .route(
                "/v1/messages",
                post(|| async {
                    Json(json!({
                        "ok": true,
                        "usage": {
                            "input_tokens": 2,
                            "output_tokens": 3
                        }
                    }))
                }),
            )
            .route(
                "/v1/messages/stream",
                get(|| async {
                    let stream = async_stream::stream! {
                        yield Ok::<_, std::io::Error>(bytes::Bytes::from("event: content_block_delta\ndata: {\"delta\":\"hello\"}\n\n"));
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        yield Ok::<_, std::io::Error>(bytes::Bytes::from("event: message_stop\ndata: {\"usage\":{\"input_tokens\":4,\"output_tokens\":6}}\n\n"));
                    };
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from_stream(stream))
                        .unwrap()
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        addr
    }

    async fn test_state(upstream: &str, output: PathBuf) -> AppState {
        let (tx, _) = broadcast::channel(256);
        AppState {
            args: ProxyArgs {
                upstream: upstream.to_string(),
                bind: "127.0.0.1".to_string(),
                port: 0,
                ui: false,
                admin_port: 0,
                log: LogLevel::None,
                filter: None,
                ring_size: 100,
                record: false,
                output: Some(output.clone()),
                modify_header: Vec::new(),
                delete_header: Vec::new(),
                modify_body: None,
                intercept: None,
            },
            client: reqwest::Client::builder().build().unwrap(),
            ring: Arc::new(Mutex::new(VecDeque::new())),
            broadcaster: tx,
            record: Arc::new(Mutex::new(RecordState {
                enabled: false,
                output,
                count: 0,
            })),
            intercept_pattern: Arc::new(Mutex::new(None)),
            intercept_queue: Arc::new(Mutex::new(HashMap::new())),
            body_modifier: None,
            header_sets: Arc::new(HashMap::new()),
            header_deletes: Arc::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn forwards_json_and_applies_cel_filter() {
        let addr = spawn_upstream().await;
        let tmp = tempdir().unwrap();
        let state = test_state(&format!("http://{}", addr), tmp.path().join("session.json")).await;

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "secret".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());

        let resp = proxy_handler_impl(
            state.clone(),
            Method::POST,
            "/v1/messages".parse::<Uri>().unwrap(),
            headers,
            bytes::Bytes::from(r#"{"model":"claude-sonnet","messages":[]}"#),
        )
        .await
        .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("\"ok\":true"));

        let ring = state.ring.lock().await;
        assert_eq!(ring.len(), 1);
        let interaction = ring.front().unwrap();
        assert_eq!(interaction.metadata.total_tokens, Some(5));
        assert!(evaluate_expression(
            "response.status >= 200 && request.body.model.startsWith('claude') && metadata.provider == 'anthropic'",
            interaction
        ));
        assert!(!evaluate_expression("response.status >= 400", interaction));
    }

    #[tokio::test]
    async fn streams_sse_passthrough_and_captures_chunks() {
        let addr = spawn_upstream().await;
        let tmp = tempdir().unwrap();
        let state = test_state(
            &format!("http://{}", addr),
            tmp.path().join("stream-session.json"),
        )
        .await;

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "secret".parse().unwrap());

        let resp = proxy_handler_impl(
            state.clone(),
            Method::GET,
            "/v1/messages/stream".parse::<Uri>().unwrap(),
            headers,
            bytes::Bytes::new(),
        )
        .await
        .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("event: content_block_delta"));
        assert!(text.contains("event: message_stop"));

        let ring = state.ring.lock().await;
        let interaction = ring.front().unwrap();
        assert!(interaction.response.streaming);
        assert!(!interaction.response.chunks.is_empty());
        assert_eq!(interaction.metadata.total_tokens, Some(10));
    }

    #[tokio::test]
    async fn admin_save_replay_and_curl_smoke() {
        let addr = spawn_upstream().await;
        let tmp = tempdir().unwrap();
        let output = tmp.path().join("saved.json");
        let state = test_state(&format!("http://{}", addr), output.clone()).await;

        let interaction = Interaction {
            id: "abc-123".to_string(),
            recorded_at: Utc::now(),
            request: StoredRequest {
                method: "POST".to_string(),
                path: "/v1/messages".to_string(),
                headers: HashMap::from([(
                    "content-type".to_string(),
                    "application/json".to_string(),
                )]),
                body: json!({"model": "claude-sonnet"}),
            },
            response: StoredResponse {
                status: 200,
                headers: HashMap::new(),
                streaming: false,
                chunks: Vec::new(),
                body: Some(json!({"ok": true})),
            },
            metadata: Metadata::default(),
        };

        {
            let mut ring = state.ring.lock().await;
            ring.push_front(interaction);
        }

        let saved = write_cassette(&state, &output, Some(vec!["abc-123".to_string()]))
            .await
            .unwrap();
        assert_eq!(saved, 1);
        assert!(output.exists());

        let curl_resp = curl_request_handler(State(state.clone()), Path("abc-123".to_string()))
            .await
            .into_response();
        assert_eq!(curl_resp.status(), StatusCode::OK);

        let replay_resp = replay_request_handler(State(state.clone()), Path("abc-123".to_string()))
            .await
            .into_response();
        assert_eq!(replay_resp.status(), StatusCode::OK);
    }
}
