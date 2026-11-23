//! Pi Native over `--mode rpc`.
//!
//! This adapter keeps the transport deliberately small: newline-delimited Pi
//! frames are correlated by `id`, text/thinking deltas are converted to the
//! common event stream, and `agent_settled` is the only normal turn boundary.

use std::path::PathBuf;

use async_trait::async_trait;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use crate::process::{Child, ChildStdin, Stdio, command};
use crate::{
    AgentEvent, CatalogContext, DoneStatus, Harness, HarnessError, HarnessId, LaunchContext, Model,
    ReasoningLevel, RunControls, RunRequest, SteeringMode, ToolCall,
};


#[derive(Debug, Clone, Default)]
pub struct PiHarness {
    executable: Option<PathBuf>,
}

impl PiHarness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.executable = Some(executable.into());
        self
    }

    fn executable(&self) -> PathBuf {
        self.executable
            .clone()
            .or_else(|| std::env::var_os("PI_EXECUTABLE").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("pi"))
    }
}

#[async_trait]
impl Harness for PiHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }

    fn display_name(&self) -> &str {
        "Pi"
    }

    fn supports_steering(&self) -> bool {
        true
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[
            ReasoningLevel::Off,
            ReasoningLevel::Minimal,
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::XHigh,
            ReasoningLevel::Max,
        ]
    }

    fn fallback_models(&self) -> Vec<Model> {
        Vec::new()
    }

    async fn models(&self, _context: CatalogContext) -> Result<Vec<Model>, HarnessError> {
        Ok(self.fallback_models())
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
        launch: LaunchContext,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        if let Some(level) = request.reasoning { level.thinking_level()?; }
        let executable = launch.launch.as_ref().map(|spec| spec.executable.clone()).unwrap_or_else(|| self.executable());
        let mut command = if cfg!(windows)
            && executable.extension().and_then(|extension| extension.to_str()) == Some("sh")
        {
            let mut command = command("bash");
            command.arg(&executable);
            command
        } else {
            command(&executable)
        };
        crate::compose_child_path(&mut command, &executable);
        if let Some(spec) = &launch.launch {
            command.args(&spec.prefix_args);
            for (name, value) in &spec.env {
                command.env(name, value.expose());
            }
        }
        command.args(["--mode", "rpc"]);
        if let Some(support) = launch.support.as_ref() { command.env("AUTOCODE_AGENT_SUPPORT_DIR", support.path()); }
        let mut child = command
            .current_dir(if request.cwd.is_empty() { std::env::current_dir().map_err(HarnessError::Io)? } else { PathBuf::from(&request.cwd) })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| if error.kind() == std::io::ErrorKind::NotFound { HarnessError::NotInstalled(executable.display().to_string()) } else { HarnessError::Io(error) })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Protocol("pi has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("pi has no stdout".into()))?;
        let (tx, rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(64);
        tokio::spawn(async move { let _launch = launch; run_process(child, stdout, stdin, request, controls, tx).await; });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

async fn run_process(
    mut child: Child,
    stdout: tokio::process::ChildStdout,
    mut stdin: ChildStdin,
    request: RunRequest,
    mut controls: RunControls,
    tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
) {
    let mut lines = BufReader::new(stdout).lines();
    let mut next_id = 1u64;
    let mut session_started = false;
    let mut prompt_sent = false;
    let mut state_id = next_id;
    next_id += 1;
    let mut effort_id = None;
    let mut outcome = DoneStatus::Completed;
    let mut outcome_error = None;
    let mut session_file = None;
    let requested_effort = match request.reasoning.map(ReasoningLevel::thinking_level).transpose() {
        Ok(level) => level,
        Err(error) => { let _ = tx.send(Err(error)).await; return; }
    };
    if let Err(error) = send_command(&mut stdin, "get_state", state_id, json!({})).await {
        let _ = tx.send(Err(error)).await;
        return;
    }
    loop {
        tokio::select! {
            _ = controls.interrupt.cancelled() => {
                let _ = send_command(&mut stdin, "abort", next_id, json!({})).await;
                let _ = child.kill().await;
                let _ = tx.send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: None,
                })).await;
                return;
            }
            Some(steer) = controls.steering.recv() => {
                if let Err(error) = send_command(&mut stdin, "steer", next_id, json!({"message": steer.prompt})).await {
                    let _ = tx.send(Err(error)).await;
                    let _ = child.kill().await;
                    return;
                }
                next_id += 1;
            }
            line = lines.next_line() => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        let _ = tx.send(Err(HarnessError::Protocol("pi stdout closed".into()))).await;
                        return;
                    }
                    Err(error) => {
                        let _ = tx.send(Err(HarnessError::Io(error))).await;
                        return;
                    }
                };
                let frame: Value = match serde_json::from_str(&line) {
                    Ok(frame) => frame,
                    Err(error) => {
                        let _ = tx.send(Err(HarnessError::Protocol(format!("pi frame is not JSON: {error}")))).await;
                        return;
                    }
                };
                if frame["type"] == "response" && frame["id"] == state_id && frame["success"] == true {
                    session_file = frame["data"]["sessionFile"].as_str().map(str::to_owned);
                    if let Some(model_id) = frame["data"]["model"]["id"].as_str() {
                        if tx.send(Ok(AgentEvent::ModelReported {
                            model_id: model_id.to_owned(),
                            provider_id: frame["data"]["model"]["provider"].as_str().map(str::to_owned),
                            reasoning: serde_json::from_value(frame["data"]["thinkingLevel"].clone()).ok(),
                        })).await.is_err() { return; }
                    }
                }
                if let Some(event) = state_event(&frame, &mut session_started) {
                    if tx.send(Ok(event)).await.is_err() { return; }
                    if tx.send(Ok(AgentEvent::CapabilitiesReported { capabilities: crate::HarnessCapabilities { resume: true, history_replay: true, streaming: true, questions: true, model_switch: true, compact: true, steer: true, images: true, ..Default::default() } })).await.is_err() { return; }
                }
                if frame["type"] == "response" && frame["success"] == false {
                    let _ = tx.send(Err(HarnessError::Protocol(frame["error"].as_str().unwrap_or("pi command failed").to_owned()))).await;
                    return;
                }
                if frame["type"] == "response" && effort_id.is_some_and(|id| frame["id"] == id) {
                    state_id = next_id;
                    next_id += 1;
                    if let Err(error) = send_command(&mut stdin, "get_state", state_id, json!({})).await {
                        let _ = tx.send(Err(error)).await;
                        return;
                    }
                    continue;
                }
                if !prompt_sent && frame["type"] == "response" && frame["id"] == state_id {
                    if let Some(level) = requested_effort {
                        if effort_id.is_none() {
                            let id = next_id;
                            effort_id = Some(id);
                            next_id += 1;
                            if let Err(error) = send_command(&mut stdin, "set_thinking_level", id, json!({"level":level})).await {
                                let _ = tx.send(Err(error)).await;
                                return;
                            }
                            continue;
                        }
                        if frame["data"]["thinkingLevel"] != level {
                            let _ = tx.send(Err(HarnessError::Protocol(format!("Pi applied thinking level {} instead of {level}", frame["data"]["thinkingLevel"])))).await;
                            return;
                        }
                    }
                    let prompt_id = next_id;
                    next_id += 1;
                    let mut params = json!({"message": request.prompt});
                    if let Some(attachments) = request.attachments.first() {
                        params["images"] = json!([attachments]);
                    }
                    if let Err(error) = send_command(&mut stdin, "prompt", prompt_id, params).await {
                        let _ = tx.send(Err(error)).await;
                        return;
                    }
                    prompt_sent = true;
                }
                match frame["type"].as_str() {
                    Some("agent_settled") => {
                        if frame["aborted"] == true { outcome = DoneStatus::Interrupted; }
                        let _ = tx.send(Ok(AgentEvent::Done {
                            status: outcome,
                            result: None,
                            error: outcome_error,
                            session_id: frame["sessionFile"].as_str().map(str::to_owned).or(session_file),
                        })).await;
                        let _ = child.kill().await;
                        return;
                    }
                    Some("message_update") => {
                        if let Some(event) = text_event(&frame) {
                            if tx.send(Ok(event)).await.is_err() { return; }
                        }
                    }
                    Some("tool_execution_start") => {
                        let event = AgentEvent::ToolCall {
                            id: frame["toolCallId"].as_str().unwrap_or("pi-tool").to_owned(),
                            call: ToolCall::Unknown {
                                name: frame["toolName"].as_str().unwrap_or("tool").to_owned(),
                                input: frame.get("args").cloned(),
                            },
                        };
                        if tx.send(Ok(event)).await.is_err() { return; }
                    }
                    Some("tool_execution_end") => {
                        let event = AgentEvent::ToolResult {
                            id: frame["toolCallId"].as_str().unwrap_or("pi-tool").to_owned(),
                            is_error: frame["isError"].as_bool().unwrap_or(false),
                            output: frame["result"]["content"].as_array().map(|content| content.iter()
                                .filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("\n")),
                            diff: None,
                        };
                        if tx.send(Ok(event)).await.is_err() { return; }
                    }
                    Some("message_end") if frame["message"]["role"] == "assistant" => {
                        let message = &frame["message"];
                        match message["stopReason"].as_str() {
                            Some("error") => {
                                outcome = DoneStatus::Errored;
                                outcome_error = Some(message["errorMessage"].as_str().unwrap_or("Pi stopped with an error").to_owned());
                            }
                            Some("aborted") => outcome = DoneStatus::Interrupted,
                            _ => {}
                        }
                        if tx.send(Ok(AgentEvent::AssistantMessageCompleted { assistant_message_id: frame["message"]["id"].as_str().unwrap_or_default().to_owned() })).await.is_err() { return; }
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn send_command(
    stdin: &mut tokio::process::ChildStdin,
    kind: &str,
    id: u64,
    mut params: Value,
) -> Result<(), HarnessError> {
    params["type"] = Value::String(kind.to_owned());
    params["id"] = Value::from(id);
    let mut line = serde_json::to_vec(&params)
        .map_err(|error| HarnessError::Protocol(error.to_string()))?;
    line.push(b'\n');
    stdin.write_all(&line).await.map_err(HarnessError::Io)?;
    stdin.flush().await.map_err(HarnessError::Io)
}

fn state_event(frame: &Value, started: &mut bool) -> Option<AgentEvent> {
    let session_file = frame["data"]["sessionFile"].as_str().filter(|session_file| !session_file.is_empty())?;
    if *started || frame["type"] != "response" {
        return None;
    }
    *started = true;
    Some(AgentEvent::SessionStarted {
        harness: HarnessId::Pi,
        model: frame["data"]["model"]["id"].as_str().unwrap_or_default().to_owned(),
        tools: Vec::new(),
        cwd: String::new(),
        session_id: session_file.to_owned(),
        assistant_message_id: format!("pi-{}", uuid::Uuid::new_v4()),
    })
}

fn text_event(frame: &Value) -> Option<AgentEvent> {
    let event = frame["assistantMessageEvent"]["type"].as_str()?;
    let text = frame["assistantMessageEvent"]["delta"].as_str()?.to_owned();
    match event {
        "text_delta" => Some(AgentEvent::TextDelta { text }),
        "thinking_delta" => Some(AgentEvent::ReasoningDelta { text }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_deltas_stream_without_replaying_terminal_snapshots() {
        let text = json!({"assistantMessageEvent":{"type":"text_delta","delta":"hello"}});
        assert_eq!(text_event(&text), Some(AgentEvent::TextDelta { text: "hello".into() }));
        let thinking = json!({"assistantMessageEvent":{"type":"thinking_delta","delta":"plan"}});
        assert_eq!(text_event(&thinking), Some(AgentEvent::ReasoningDelta { text: "plan".into() }));
        for kind in ["text_end", "thinking_end"] {
            assert_eq!(text_event(&json!({"assistantMessageEvent":{"type":kind,"content":"hello"}})), None);
        }
    }

    #[test]
    fn pi_state_requires_a_non_empty_string_session_file() {
        for session_file in [Value::Null, Value::String(String::new()), Value::from(7)] {
            let frame = json!({
                "type": "response",
                "data": {"model": "test-model", "sessionFile": session_file},
            });
            let mut started = false;
            assert!(state_event(&frame, &mut started).is_none());
            assert!(!started);
        }

        let frame = json!({
            "type": "response",
            "data": {"model": "test-model", "sessionFile": "/tmp/pi-session"},
        });
        let mut started = false;
        assert!(matches!(
            state_event(&frame, &mut started),
            Some(AgentEvent::SessionStarted { session_id, .. }) if session_id == "/tmp/pi-session"
        ));
        assert!(started);
    }
}
