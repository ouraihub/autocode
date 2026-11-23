//! DSH ACP adapter over JSON-RPC 2.0 on stdio.

use std::path::PathBuf;
use async_trait::async_trait;
use futures::{StreamExt as _, stream::BoxStream};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::process::{Child, ChildStdin, Stdio, command};
use crate::{AgentEvent, CatalogContext, DoneStatus, Harness, HarnessError, HarnessId, LaunchContext, Model, ReasoningLevel, RunControls, RunRequest, SteeringMode};

#[derive(Debug, Clone, Default)]
pub struct DshHarness { executable: Option<PathBuf> }

impl DshHarness {
    pub fn new() -> Self { Self::default() }
    pub fn with_executable(mut self, executable: impl Into<PathBuf>) -> Self { self.executable = Some(executable.into()); self }
    pub(crate) fn executable_path(&self) -> PathBuf { self.executable.clone().or_else(|| std::env::var_os("DSH_EXECUTABLE").map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("dsh")) }
    fn executable(&self) -> PathBuf { self.executable_path() }
}

#[async_trait]
impl Harness for DshHarness {
    fn id(&self) -> HarnessId { HarnessId::Dsh }
    fn display_name(&self) -> &str { "DSH" }
    fn supports_steering(&self) -> bool { false }
    fn steering_mode(&self) -> SteeringMode { SteeringMode::TurnBoundary }
    fn reasoning_levels(&self) -> &[ReasoningLevel] { &[ReasoningLevel::Off, ReasoningLevel::Minimal, ReasoningLevel::Low, ReasoningLevel::Medium, ReasoningLevel::High, ReasoningLevel::XHigh, ReasoningLevel::Max] }
    fn installed(&self) -> bool { self.executable_path().is_file() || crate::executable::find_on_paths("dsh", Vec::new()).is_some() }
    async fn models(&self, _context: CatalogContext) -> Result<Vec<Model>, HarnessError> { Ok(Vec::new()) }
    async fn run(&self, request: RunRequest, controls: RunControls, launch: LaunchContext) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        if let Some(level) = request.reasoning { level.thinking_level()?; }
        let (executable, prefix_args) = launch.launch.as_ref().map(|spec| (spec.executable.clone(), spec.prefix_args.clone())).unwrap_or_else(|| (self.executable(), Vec::new()));
        let mut command = command(&executable);
        if let Some(spec) = &launch.launch {
            command.args(&prefix_args);
            for (name, value) in &spec.env { command.env(name, value.expose()); }
        }
        command.args(["--profile", "acp"]);
        if let Some(support) = launch.support.as_ref() { command.env("AUTOCODE_AGENT_SUPPORT_DIR", support.path()); }
        let cwd = if request.cwd.is_empty() { std::env::current_dir().map_err(HarnessError::Io)? } else { PathBuf::from(&request.cwd) };
        let mut child = command.current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn()
            .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { HarnessError::NotInstalled(executable.display().to_string()) } else { HarnessError::Io(e) })?;
        let stdin = child.stdin.take().ok_or_else(|| HarnessError::Protocol("dsh has no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| HarnessError::Protocol("dsh has no stdout".into()))?;
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move { let _launch = launch; run_process(child, stdout, stdin, request, controls, tx).await; });
        Ok(futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|v| (v, rx)) }).boxed())
    }
}

async fn run_process(mut child: Child, stdout: tokio::process::ChildStdout, mut stdin: ChildStdin, request: RunRequest, controls: RunControls, tx: mpsc::Sender<Result<AgentEvent, HarnessError>>) {
    let mut lines = BufReader::new(stdout).lines();
    let mut next_id = 1u64;
    let initialize = next_id;
    next_id += 1;
    if let Err(e) = send(&mut stdin, "initialize", initialize, json!({"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"autocode","version":env!("CARGO_PKG_VERSION")}})).await { let _ = tx.send(Err(e)).await; return; }
    let mut session_id = None::<String>;
    let mut prompted = false;
    let mut effort_id = None;
    let mut prompt_id = None;
    let requested_effort = match request.reasoning.map(ReasoningLevel::thinking_level).transpose() {
        Ok(level) => level,
        Err(error) => { let _ = tx.send(Err(error)).await; return; }
    };
    loop {
        tokio::select! {
            _ = controls.interrupt.cancelled() => { let _ = send(&mut stdin, "session/cancel", next_id, json!({"sessionId":session_id})).await; let _ = child.kill().await; let _ = tx.send(Ok(done(DoneStatus::Interrupted, session_id))).await; return; }
            line = lines.next_line() => {
                let line = match line { Ok(Some(v)) => v, Ok(None) => { let _ = tx.send(Err(HarnessError::Protocol("dsh stdout closed".into()))).await; return; }, Err(e) => { let _ = tx.send(Err(HarnessError::Io(e))).await; return; } };
                let frame: Value = match serde_json::from_str(&line) { Ok(v) => v, Err(e) => { let _ = tx.send(Err(HarnessError::Protocol(format!("dsh frame is not JSON: {e}")))).await; return; } };
                if frame["error"].is_object() { let _ = tx.send(Err(HarnessError::Protocol(frame["error"]["message"].as_str().unwrap_or("dsh request failed").to_owned()))).await; return; }
                if prompt_id.is_some_and(|id| frame["id"] == id) {
                    let status = match frame["result"]["stopReason"].as_str() {
                        Some("cancelled") => DoneStatus::Interrupted,
                        Some("end_turn" | "max_tokens" | "max_turn_requests" | "refusal") => DoneStatus::Completed,
                        _ => { let _ = tx.send(Err(HarnessError::Protocol("DSH prompt reply has no supported stop reason".into()))).await; return; }
                    };
                    let _ = tx.send(Ok(done(status, session_id))).await;
                    return;
                }
                if frame["id"] == initialize { let id = next_id; next_id += 1; if let Err(e) = send(&mut stdin, "session/new", id, json!({"cwd":request.cwd,"mcpServers":{}})).await { let _ = tx.send(Err(e)).await; return; } continue; }
                if session_id.is_none() && frame["result"]["sessionId"].is_string() {
                    session_id = frame["result"]["sessionId"].as_str().map(str::to_owned);
                    let _ = tx.send(Ok(AgentEvent::CapabilitiesReported { capabilities: crate::HarnessCapabilities { model_switch: true, ..Default::default() } })).await;
                    let _ = tx.send(Ok(AgentEvent::SessionPersistenceReported { resumable: false })).await;
                    let _ = tx.send(Ok(AgentEvent::SessionStarted { harness: HarnessId::Dsh, model: String::new(), tools: Vec::new(), cwd: request.cwd.clone(), session_id: session_id.clone().unwrap_or_default(), assistant_message_id: format!("dsh-{next_id}") })).await;
                    if let Some(level) = requested_effort {
                        let params = match effort_option(session_id.as_deref().unwrap_or_default(), &frame["result"]["configOptions"], level) {
                            Ok(params) => params,
                            Err(error) => { let _ = tx.send(Err(error)).await; return; }
                        };
                        effort_id = Some(next_id);
                        if let Err(error) = send(&mut stdin, "session/set_config_option", next_id, params).await { let _ = tx.send(Err(error)).await; return; }
                        next_id += 1;
                        continue;
                    }
                }
                if let Some(level) = requested_effort {
                    if !effort_id.is_some_and(|id| frame["id"] == id) { continue; }
                    let actual = thought_option(&frame["result"]["configOptions"]).and_then(|option| option["currentValue"].as_str());
                    if actual != Some(level) {
                        let _ = tx.send(Err(HarnessError::Protocol(format!("DSH applied thinking level {actual:?} instead of {level}")))).await;
                        return;
                    }
                }
                if !prompted && session_id.is_some() { let id = next_id; next_id += 1; prompt_id = Some(id); if let Err(e) = send(&mut stdin, "session/prompt", id, json!({"sessionId":session_id,"prompt":[{"type":"text","text":request.prompt}]})).await { let _ = tx.send(Err(e)).await; return; } prompted = true; }
            }
        }
    }
}

async fn send(stdin: &mut ChildStdin, method: &str, id: u64, params: Value) -> Result<(), HarnessError> { let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}); let mut bytes = serde_json::to_vec(&request).map_err(|e| HarnessError::Protocol(e.to_string()))?; bytes.push(b'\n'); stdin.write_all(&bytes).await.map_err(HarnessError::Io)?; stdin.flush().await.map_err(HarnessError::Io) }
fn done(status: DoneStatus, session_id: Option<String>) -> AgentEvent { AgentEvent::Done { status, result: None, error: None, session_id } }

fn thought_option(options: &Value) -> Option<&Value> {
    options.as_array()?.iter().find(|option| option["category"] == "thought_level")
}

fn effort_option(session: &str, options: &Value, level: &str) -> Result<Value, HarnessError> {
    let option = thought_option(options).ok_or_else(|| HarnessError::Protocol("DSH model does not offer a reasoning setting".into()))?;
    let id = option["id"].as_str().ok_or_else(|| HarnessError::Protocol("DSH reasoning setting has no id".into()))?;
    if !option["options"].as_array().is_some_and(|choices| choices.iter().any(|choice| choice["value"] == level)) {
        return Err(HarnessError::Protocol(format!("DSH model does not offer thinking level {level}")));
    }
    Ok(json!({"sessionId":session,"configId":id,"value":level}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_uses_advertised_config_id_and_never_substitutes_provider_default() {
        let options = json!([{"id":"provider_effort","category":"thought_level","currentValue":"",
            "options":[{"value":"off"},{"value":"high"}]}]);
        assert_eq!(effort_option("session", &options, "off").unwrap(), json!({"sessionId":"session","configId":"provider_effort","value":"off"}));
        assert!(effort_option("session", &options, "low").is_err());
        assert!(effort_option("session", &json!([]), "off").is_err());
    }
}
