//! Per-launch loopback proxy for managed model providers.

use std::{convert::Infallible, sync::{Arc, Mutex}};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use futures::StreamExt as _;
use hyper::{body::{Frame, Incoming}, header, server::conn::http1, service::service_fn, HeaderMap, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use model_provider::{AuthMode, Protocol, Secret};
use tokio::{net::TcpListener, task::JoinHandle};

use crate::HarnessError;

const MAX_REQUEST_BODY: usize = 10 * 1024 * 1024;
type BoxError = Box<dyn std::error::Error + Send + Sync>;
type GatewayBody = UnsyncBoxBody<Bytes, BoxError>;

pub struct GatewayServer {
    base_url: String,
    token: String,
    listener: Mutex<Option<JoinHandle<()>>>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

struct GatewayState {
    endpoint: reqwest::Url,
    protocol: Protocol,
    keys: KeyRoundRobin,
    headers: Vec<(String, Secret)>,
    auth: AuthMode,
    token: String,
    route: String,
    client: reqwest::Client,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl std::fmt::Debug for GatewayServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayServer").field("base_url", &self.base_url).field("token", &"[redacted]").finish_non_exhaustive()
    }
}

impl GatewayServer {
    pub async fn start(endpoint: String, protocol: Protocol, keys: Vec<Secret>, headers: Vec<(String, Secret)>, auth: AuthMode) -> Result<Arc<Self>, HarnessError> {
        let endpoint_url = reqwest::Url::parse(&endpoint).map_err(|_| HarnessError::Protocol("gateway endpoint is invalid".into()))?;
        if !matches!(endpoint_url.scheme(), "http" | "https") || endpoint_url.host_str().is_none() || !endpoint_url.username().is_empty() || endpoint_url.password().is_some() || endpoint_url.query().is_some() || endpoint_url.fragment().is_some() {
            return Err(HarnessError::Protocol("gateway endpoint is invalid".into()));
        }
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let route = format!("/{}/v1", uuid::Uuid::new_v4().simple());
        let token = uuid::Uuid::new_v4().to_string();
        let client = reqwest::Client::builder().redirect_policy(reqwest::redirect::Policy::none()).build().map_err(|_| HarnessError::Protocol("gateway client unavailable".into()))?;
        let connections = Arc::new(Mutex::new(Vec::new()));
        if auth == AuthMode::ApiKey && keys.is_empty() { return Err(HarnessError::Protocol("gateway has no enabled keys".into())); }
        let state = Arc::new(GatewayState { endpoint: endpoint_url, protocol, keys: KeyRoundRobin::new(keys.iter().map(|key| key.expose().to_owned())), headers, auth, token: token.clone(), route: route.clone(), client, connections: connections.clone() });
        let accept_state = state.clone();
        let listener_task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break; };
                let state = accept_state.clone();
                let task = tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |request| handle_request(state.clone(), request));
                    let _ = http1::Builder::new().serve_connection(io, service).await;
                });
                if let Ok(mut tasks) = accept_state.connections.lock() { tasks.retain(|task| !task.is_finished()); tasks.push(task); }
            }
        });
        Ok(Arc::new(Self { base_url: format!("http://{}{}", address, route), token, listener: Mutex::new(Some(listener_task)), connections }))
    }

    pub fn base_url(&self) -> String { self.base_url.clone() }
    pub fn token(&self) -> &str { &self.token }
}

impl Drop for GatewayServer {
    fn drop(&mut self) {
        if let Ok(mut listener) = self.listener.lock() { if let Some(task) = listener.take() { task.abort(); } }
        if let Ok(mut tasks) = self.connections.lock() { for task in tasks.drain(..) { task.abort(); } }
    }
}

async fn handle_request(state: Arc<GatewayState>, request: Request<Incoming>) -> Result<Response<GatewayBody>, Infallible> {
    if !authorized(&request, &state.token) { return Ok(simple(StatusCode::UNAUTHORIZED, "unauthorized")); }
    let path = request.uri().path();
    let Some(suffix) = path.strip_prefix(&state.route).and_then(|value| value.strip_prefix('/')) else { return Ok(simple(StatusCode::NOT_FOUND, "not found")); };
    let method = request.method().clone();
    let valid = match suffix { "models" => method == hyper::Method::GET, "chat/completions" => state.protocol == Protocol::OpenaiChat && method == hyper::Method::POST, "responses" => state.protocol == Protocol::OpenaiResponses && method == hyper::Method::POST, "messages" => state.protocol == Protocol::AnthropicMessages && method == hyper::Method::POST, _ => false };
    if !valid { return Ok(simple(StatusCode::NOT_FOUND, "not found")); }
    let suffix = suffix.to_owned();
    let headers = request.headers().clone();
    let body = match read_body(request.into_body()).await { Ok(body) => body, Err(status) => return Ok(simple(status, if status == StatusCode::PAYLOAD_TOO_LARGE { "request body too large" } else { "bad request" })), };
    let mut upstream = state.endpoint.clone();
    let base_path = upstream.path().trim_end_matches('/');
    let path = if base_path.is_empty() { format!("/v1/{suffix}") } else { format!("{base_path}/{suffix}") };
    upstream.set_path(&path);
    let mut builder = state.client.request(reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST), upstream).body(body);
    for (name, value) in copy_headers(&headers) { builder = builder.header(name, value); }
    if state.auth == AuthMode::ApiKey {
        if let Some(key) = state.keys.next() {
            let (name, value) = if state.protocol == Protocol::AnthropicMessages { ("x-api-key", key) } else { ("authorization", format!("Bearer {key}")) };
            builder = builder.header(name, value);
        }
    }
    for (name, value) in &state.headers {
        if forbidden_header(name) { continue; }
        builder = builder.header(name, value.expose());
    }
    let upstream = match builder.send().await { Ok(response) => response, Err(_) => return Ok(simple(StatusCode::BAD_GATEWAY, "upstream unavailable")), };
    let status = StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if !status.is_success() { return Ok(simple(status, "model service rejected the request")); }
    let mut response = Response::builder().status(status);
    for (name, value) in copy_headers(upstream.headers()) { response = response.header(name, value); }
    let stream = upstream.bytes_stream().map(|chunk| chunk.map(Frame::data).map_err(|_| -> BoxError { Box::new(std::io::Error::other("upstream stream disconnected")) }));
    Ok(response.body(StreamBody::new(stream).boxed_unsync()).unwrap_or_else(|_| simple(StatusCode::BAD_GATEWAY, "gateway error")))
}

fn authorized(request: &Request<Incoming>, token: &str) -> bool {
    request.headers().get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()).and_then(|value| value.strip_prefix("Bearer ")).is_some_and(|value| value == token)
        || request.headers().get("x-api-key").and_then(|value| value.to_str().ok()).is_some_and(|value| value == token)
}

async fn read_body(mut body: Incoming) -> Result<Bytes, StatusCode> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| StatusCode::BAD_REQUEST)?;
        if let Some(data) = frame.data_ref() { if bytes.len().saturating_add(data.len()) > MAX_REQUEST_BODY { return Err(StatusCode::PAYLOAD_TOO_LARGE); } bytes.extend_from_slice(data); }
    }
    Ok(Bytes::from(bytes))
}

fn forbidden_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(name.as_str(), "authorization" | "x-api-key" | "host" | "content-length" | "connection" | "keep-alive" | "proxy-authenticate" | "proxy-authorization" | "te" | "trailer" | "transfer-encoding" | "upgrade")
}

fn copy_headers(headers: &HeaderMap) -> impl Iterator<Item = (&hyper::header::HeaderName, &hyper::header::HeaderValue)> {
    let listed: std::collections::HashSet<_> = headers.get_all(header::CONNECTION).iter().filter_map(|value| value.to_str().ok()).flat_map(|value| value.split(',')).map(|value| value.trim().to_ascii_lowercase()).collect();
    headers.iter().filter(move |(name, _)| !forbidden_header(name.as_str()) && !listed.contains(name.as_str()))
}

fn simple(status: StatusCode, text: &str) -> Response<GatewayBody> { Response::builder().status(status).body(Full::new(Bytes::from(text.to_owned())).map_err(|never| -> BoxError { match never {} }).boxed_unsync()).unwrap() }

#[derive(Debug, Clone)]
pub struct GatewayLease { pub token: String, pub base_url: String }

#[derive(Clone)]
pub struct KeyRoundRobin { keys: Arc<Mutex<Vec<String>>>, next: Arc<Mutex<usize>> }
impl KeyRoundRobin {
    pub fn new(keys: impl IntoIterator<Item = String>) -> Self { Self { keys: Arc::new(Mutex::new(keys.into_iter().filter(|key| !key.is_empty()).collect())), next: Arc::new(Mutex::new(0)) } }
    pub fn next(&self) -> Option<String> { let keys = self.keys.lock().ok()?; if keys.is_empty() { return None; } let mut next = self.next.lock().ok()?; let key = keys[*next % keys.len()].clone(); *next = (*next + 1) % keys.len(); Some(key) }
    pub fn replace(&self, keys: impl IntoIterator<Item = String>) { if let Ok(mut current) = self.keys.lock() { *current = keys.into_iter().filter(|key| !key.is_empty()).collect(); } if let Ok(mut next) = self.next.lock() { *next = 0; } }
}

impl std::fmt::Debug for KeyRoundRobin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("KeyRoundRobin").finish_non_exhaustive() }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLimits { pub max_bytes: usize, pub max_line_bytes: usize }
impl Default for StreamLimits { fn default() -> Self { Self { max_bytes: 64 * 1024 * 1024, max_line_bytes: 1024 * 1024 } } }
pub fn accept_stream_chunk(total: &mut usize, chunk: &[u8], limits: StreamLimits) -> Result<(), &'static str> { if chunk.len() > limits.max_line_bytes { return Err("stream chunk exceeds line limit"); } *total = total.checked_add(chunk.len()).ok_or("stream size overflow")?; if *total > limits.max_bytes { return Err("stream exceeds total limit"); } Ok(()) }

#[cfg(test)]
mod tests;
