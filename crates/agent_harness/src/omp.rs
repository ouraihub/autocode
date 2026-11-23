//! OMP protocol-v2 adapter over `--mode rpc-ui`.

use std::{collections::VecDeque, path::PathBuf, time::Duration};

use async_trait::async_trait;
use futures::{StreamExt as _, stream::BoxStream};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::sync::mpsc;

use crate::framing::{Chunks, OMP_LINE_LIMIT};
use crate::process::{Child, ChildStdin, Stdio, command};
use crate::{AgentEvent, CatalogContext, DoneStatus, Harness, HarnessError, HarnessId, LaunchContext, LaunchSpec, Model, PermissionMode, ReasoningLevel, RunControls, RunRequest, SteeringMode, UserInputAnswer, UserInputQuestion};

#[derive(Debug, Clone, Default)]
pub struct OmpHarness { executable: Option<PathBuf> }

impl OmpHarness {
    pub fn new() -> Self { Self::default() }
    pub fn with_executable(mut self, executable: impl Into<PathBuf>) -> Self { self.executable = Some(executable.into()); self }
    fn executable(&self) -> PathBuf { self.executable.clone().or_else(|| std::env::var_os("OMP_EXECUTABLE").map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("omp")) }

    fn spawn(&self, spec: Option<&LaunchSpec>, cwd: &str, permission: PermissionMode, catalog: bool, resume: Option<&str>, ephemeral: bool, support: Option<&std::path::Path>) -> Result<(Child, Wire, crate::StderrTail), HarnessError> {
        let executable = spec.map(|spec| spec.executable.clone()).unwrap_or_else(|| self.executable());
        let mut command = command(&executable);
        crate::compose_child_path(&mut command, &executable);
        if let Some(spec) = spec {
            command.args(&spec.prefix_args);
            for (name, value) in &spec.env { command.env(name, value.expose()); }
        }
        command.args(["--mode", "rpc-ui"]);
        match permission {
            PermissionMode::FullAccess => { command.args(["--approval-mode", "yolo"]); }
            PermissionMode::AcceptEdits => { command.args(["--approval-mode", "write"]); }
            PermissionMode::Auto => {}
            _ => { command.args(["--approval-mode", "always-ask"]); }
        }
        if catalog { command.args(["--no-ui", "--no-session"]); }
        else if ephemeral { command.arg("--no-session"); }
        else if let Some(session) = resume { command.args(["--session", session]); }
        if let Some(support) = support { command.env("AUTOCODE_AGENT_SUPPORT_DIR", support); }
        let cwd = if cwd.is_empty() { std::env::current_dir().map_err(HarnessError::Io)? } else { PathBuf::from(cwd) };
        let mut child = command.current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn()
            .map_err(|error| if error.kind() == std::io::ErrorKind::NotFound { HarnessError::NotInstalled(executable.display().to_string()) } else { HarnessError::Io(error) })?;
        let stdin = child.stdin.take().ok_or_else(|| protocol("omp has no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| protocol("omp has no stdout"))?;
        let stderr = crate::StderrTail::default();
        if let Some(output) = child.stderr.take() {
            let stderr = stderr.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(output).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => stderr.push(&line),
                        Ok(None) => return,
                        Err(error) => { log::warn!("omp.stderr.read_failed: {error}"); return; }
                    }
                }
            });
        }
        Ok((child, Wire { stdin, lines: BufReader::new(stdout).lines(), chunks: Chunks::default(), next_id: 0 }, stderr))
    }
}

#[async_trait]
impl Harness for OmpHarness {
    fn id(&self) -> HarnessId { HarnessId::Omp }
    fn display_name(&self) -> &str { "OMP" }
    fn supports_steering(&self) -> bool { true }
    fn steering_mode(&self) -> SteeringMode { SteeringMode::TurnBoundary }
    fn reasoning_levels(&self) -> &[ReasoningLevel] { &[ReasoningLevel::Off, ReasoningLevel::Minimal, ReasoningLevel::Low, ReasoningLevel::Medium, ReasoningLevel::High, ReasoningLevel::XHigh, ReasoningLevel::Max] }

    async fn models(&self, context: CatalogContext) -> Result<Vec<Model>, HarnessError> {
        let (_child, mut wire, _stderr) = self.spawn(context.launch.as_ref(), "", PermissionMode::Auto, true, None, true, None)?;
        tokio::time::timeout(Duration::from_secs(20), async {
            while wire.read().await?["type"] != "ready" {}
            wire.request("negotiate_protocol", json!({"protocolVersion":2})).await?;
            let data = wire.request("get_available_models", json!({})).await?;
            let models = data["models"].as_array().ok_or_else(|| protocol("OMP catalog has no models array"))?;
            models.iter().map(|model| {
                let (provider, id) = model_identity(model)?;
                Ok(Model {
                    id: format!("{provider}/{id}"),
                    label: model["name"].as_str().unwrap_or(id).to_owned(),
                    description: Some(provider.to_owned()),
                    reasoning_levels: if model["reasoning"].as_bool() == Some(true) { self.reasoning_levels().to_vec() } else { Vec::new() },
                    options: Vec::new(),
                })
            }).collect()
        }).await.map_err(|_| protocol("OMP model discovery timed out"))?
    }

    async fn run(&self, request: RunRequest, controls: RunControls, launch: LaunchContext) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        if let Some(level) = request.reasoning { level.thinking_level()?; }
        let (mut child, wire, stderr) = self.spawn(launch.launch.as_ref(), &request.cwd, request.permission, false, request.resume.as_deref(), request.ephemeral, launch.support.as_ref().map(|support| support.path()))?;
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let _launch = launch;
            if let Err(error) = run_process(&mut child, wire, request, controls, &tx, &stderr).await {
                if tx.send(Err(error)).await.is_err() { return; }
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|event| (event, rx)) }).boxed())
    }
}

struct Wire {
    stdin: ChildStdin,
    lines: Lines<BufReader<tokio::process::ChildStdout>>,
    chunks: Chunks,
    next_id: u64,
}

impl Wire {
    async fn read(&mut self) -> Result<Value, HarnessError> {
        loop {
            let Some(line) = self.lines.next_line().await.map_err(HarnessError::Io)? else {
                self.chunks.finish()?;
                return Err(protocol("omp stdout closed"));
            };
            if line.len() + 1 > OMP_LINE_LIMIT { return Err(protocol("omp physical line exceeds limit")); }
            let frame = serde_json::from_str(&line).map_err(|error| protocol(format!("omp frame is not JSON: {error}")))?;
            if let Some(frame) = self.chunks.accept(frame)? { return Ok(frame); }
        }
    }

    async fn write(&mut self, frame: &Value) -> Result<(), HarnessError> {
        let mut bytes = serde_json::to_vec(frame).map_err(|error| protocol(error.to_string()))?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await.map_err(HarnessError::Io)?;
        self.stdin.flush().await.map_err(HarnessError::Io)
    }

    async fn send(&mut self, kind: &str, mut fields: Value) -> Result<String, HarnessError> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        fields["type"] = json!(kind);
        fields["id"] = json!(id);
        self.write(&fields).await?;
        Ok(id)
    }

    async fn request(&mut self, kind: &str, fields: Value) -> Result<Value, HarnessError> {
        let id = self.send(kind, fields).await?;
        loop {
            let frame = self.read().await?;
            if frame["type"] == "response" {
                check_response(&frame)?;
                if frame["id"] == id { return Ok(frame["data"].clone()); }
            }
        }
    }
}

async fn run_process(child: &mut Child, mut wire: Wire, request: RunRequest, mut controls: RunControls, tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>, stderr: &crate::StderrTail) -> Result<(), HarnessError> {
    let mut startup = VecDeque::new();
    let mut pending = None::<(String, &'static str)>;
    let mut prompt_id = None::<String>;
    let mut session_id = None::<String>;
    let mut session_started = false;
    let mut expected_model = None::<(String, String)>;
    let mut outcome = None::<(DoneStatus, Option<String>)>;
    let (ui_tx, mut ui_rx) = mpsc::channel::<Value>(16);
    let mut ui_tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = tx.closed() => return Ok(()),
            _ = controls.interrupt.cancelled() => {
                wire.send("abort", json!({})).await?;
                emit(tx, done(DoneStatus::Interrupted, None, session_id)).await?;
                return Ok(());
            }
            Some(steer) = controls.steering.recv(), if prompt_id.is_some() => {
                wire.send("steer", json!({"message":steer.prompt})).await?;
            }
            Some(response) = ui_rx.recv() => {
                wire.write(&response).await?;
                if let Some(id) = response["id"].as_str() { emit(tx, AgentEvent::InputResolved { request_id: id.to_owned() }).await?; }
            }
            frame = wire.read() => {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(error) => {
                        if let Some(status) = child.try_wait().map_err(HarnessError::Io)? {
                            return Err(protocol(crate::crash_message("OMP", Some(status), stderr)));
                        }
                        return Err(error);
                    }
                };
                match frame["type"].as_str().unwrap_or_default() {
                    "ready" => {
                        emit(tx, AgentEvent::CapabilitiesReported { capabilities: crate::HarnessCapabilities { resume: true, streaming: true, questions: true, tool_permissions: true, model_switch: true, steer: true, ..Default::default() } }).await?;
                        startup.extend([("negotiate_protocol", json!({"protocolVersion":2})), ("set_event_filter", json!({"events":null,"messageUpdates":"delta"})), ("set_ask_dialog", json!({"enabled":true}))]);
                        if request.model.is_some() { startup.push_back(("get_available_models", json!({}))); }
                        if let Some(level) = request.reasoning { startup.push_back(("set_thinking_level", json!({"level":level.thinking_level()?}))); }
                        startup.push_back(("get_state", json!({})));
                    }
                    "response" => {
                        check_response(&frame)?;
                        if pending.as_ref().is_some_and(|(id, _)| frame["id"] == *id) {
                            let (_, command) = pending.take().ok_or_else(|| protocol("OMP response has no pending command"))?;
                            if command == "get_available_models" {
                                let selected = select_model(&frame["data"]["models"], request.model.as_deref().ok_or_else(|| protocol("OMP model selection is missing"))?)?;
                                startup.push_front(("set_model", json!({"provider":selected.0,"modelId":selected.1})));
                                expected_model = Some(selected);
                            } else if command == "get_state" {
                                let state = &frame["data"];
                                session_id = state["sessionFile"].as_str().map(str::to_owned).or(session_id);
                                if !session_started {
                                    if let Some(level) = request.reasoning {
                                        if state["thinkingLevel"] != level.thinking_level()? { return Err(protocol(format!("OMP did not apply requested thinking level: {}", state["thinkingLevel"]))); }
                                    }
                                    if let Some((provider, model)) = &expected_model {
                                        if state["model"]["provider"] != *provider || state["model"]["id"] != *model { return Err(protocol("OMP did not apply requested model")); }
                                    }
                                    emit(tx, AgentEvent::SessionStarted { harness: HarnessId::Omp, model: state["model"]["id"].as_str().unwrap_or_default().to_owned(), tools: Vec::new(), cwd: request.cwd.clone(), session_id: session_id.clone().or_else(|| state["sessionId"].as_str().map(str::to_owned)).unwrap_or_default(), assistant_message_id: format!("omp-{}", uuid::Uuid::new_v4()) }).await?;
                                    session_started = true;
                                }
                                if let Some(event) = model_event(state) { emit(tx, event).await?; }
                            }
                        }
                        if prompt_id.as_ref().is_some_and(|id| frame["id"] == *id) && frame["data"]["agentInvoked"] == false {
                            emit(tx, done(DoneStatus::Completed, None, session_id)).await?;
                            return Ok(());
                        }
                    }
                    "model_changed" | "thinking_level_changed" if prompt_id.is_some() && pending.is_none() => {
                        pending = Some((wire.send("get_state", json!({})).await?, "get_state"));
                    }
                    "message_update" => { if let Some(event) = delta_event(&frame) { emit(tx, event).await?; } }
                    "tool_execution_start" => {
                        emit(tx, AgentEvent::ToolCall { id: frame["toolCallId"].as_str().unwrap_or_default().to_owned(), call: crate::ToolCall::Unknown { name: frame["toolName"].as_str().unwrap_or_default().to_owned(), input: frame.get("args").cloned() } }).await?;
                    }
                    "tool_execution_end" => {
                        emit(tx, AgentEvent::ToolResult { id: frame["toolCallId"].as_str().unwrap_or_default().to_owned(), is_error: frame["isError"].as_bool().unwrap_or(false), output: content_text(&frame["result"]["content"]), diff: None }).await?;
                    }
                    "message_end" if frame["message"]["role"] == "assistant" => {
                        emit(tx, AgentEvent::Usage { input_tokens: frame["message"]["usage"]["input"].as_u64().unwrap_or(0), output_tokens: frame["message"]["usage"]["output"].as_u64().unwrap_or(0) }).await?;
                    }
                    "command_output" => { if let Some(text) = frame["text"].as_str() { emit(tx, AgentEvent::TextDelta { text: text.to_owned() }).await?; } }
                    "extension_ui_request" => {
                        if let Some(questions) = ui_questions(&frame)? {
                            let id = frame["id"].as_str().ok_or_else(|| protocol("OMP dialog has no id"))?.to_owned();
                            emit(tx, AgentEvent::InputRequested { request_id: id, questions: questions.clone() }).await?;
                            let answers = (controls.request_input)(questions.clone());
                            let sender = ui_tx.clone();
                            ui_tasks.spawn(async move {
                                let response = ui_response(&frame, &questions, answers.await.ok().as_deref());
                                if sender.send(response).await.is_err() { return; }
                            });
                        } else if frame["method"] == "setTitle" {
                            if let Some(title) = frame["title"].as_str() { emit(tx, AgentEvent::TitleChanged { title: title.to_owned() }).await?; }
                        } else if frame["method"] == "cancel" {
                            if let Some(id) = frame["targetId"].as_str() { emit(tx, AgentEvent::InputResolved { request_id: id.to_owned() }).await?; }
                        }
                    }
                    "rpc_frame_error" => return Err(protocol("OMP response exceeded transport limit")),
                    "prompt_result" if prompt_id.as_ref().is_some_and(|id| frame["id"] == *id) => {
                        let result = prompt_outcome(&frame)?;
                        if result.0 != DoneStatus::Completed || frame["sessionSettled"] == true || frame["agentInvoked"] == false {
                            emit(tx, done(result.0, result.1, session_id)).await?;
                            return Ok(());
                        }
                        outcome = Some(result);
                    }
                    "session_settled" if outcome.is_some() => {
                        let (status, error) = outcome.take().ok_or_else(|| protocol("OMP settled without prompt outcome"))?;
                        emit(tx, done(status, error, session_id)).await?;
                        return Ok(());
                    }
                    _ => {}
                }
                if pending.is_none() {
                    if let Some((kind, fields)) = startup.pop_front() { pending = Some((wire.send(kind, fields).await?, kind)); }
                    else if session_started && prompt_id.is_none() { prompt_id = Some(wire.send("prompt", json!({"message":request.prompt})).await?); }
                }
            }
        }
    }
}

fn protocol(message: impl Into<String>) -> HarnessError { HarnessError::Protocol(message.into()) }
async fn emit(tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>, event: AgentEvent) -> Result<(), HarnessError> { tx.send(Ok(event)).await.map_err(|_| protocol("OMP event consumer closed")) }
fn done(status: DoneStatus, error: Option<String>, session_id: Option<String>) -> AgentEvent { AgentEvent::Done { status, result: None, error, session_id } }
fn check_response(frame: &Value) -> Result<(), HarnessError> {
    if frame["success"] == true { Ok(()) } else { Err(protocol(frame["error"].as_str().unwrap_or("OMP command failed"))) }
}
fn model_identity(model: &Value) -> Result<(&str, &str), HarnessError> {
    Ok((model["provider"].as_str().filter(|value| !value.is_empty()).ok_or_else(|| protocol("OMP model has no provider"))?, model["id"].as_str().filter(|value| !value.is_empty()).ok_or_else(|| protocol("OMP model has no id"))?))
}
fn select_model(models: &Value, selected: &str) -> Result<(String, String), HarnessError> {
    let models = models.as_array().ok_or_else(|| protocol("OMP catalog has no models array"))?;
    let mut found = None;
    for model in models {
        let (provider, id) = model_identity(model)?;
        if selected == format!("{provider}/{id}") || selected == id {
            if found.is_some() { return Err(protocol(format!("OMP model {selected} is ambiguous; choose provider/model"))); }
            found = Some((provider.to_owned(), id.to_owned()));
        }
    }
    found.ok_or_else(|| protocol(format!("OMP model not available: {selected}")))
}
fn model_event(state: &Value) -> Option<AgentEvent> {
    let (provider, id) = model_identity(&state["model"]).ok()?;
    Some(AgentEvent::ModelReported { model_id: id.to_owned(), provider_id: Some(provider.to_owned()), reasoning: serde_json::from_value(state["thinkingLevel"].clone()).ok() })
}
fn delta_event(frame: &Value) -> Option<AgentEvent> {
    let event = &frame["assistantMessageEvent"];
    let text = event["delta"].as_str()?.to_owned();
    match event["type"].as_str()? {
        "text_delta" => Some(AgentEvent::TextDelta { text }),
        "thinking_delta" => Some(AgentEvent::ReasoningDelta { text }),
        _ => None,
    }
}
fn prompt_outcome(frame: &Value) -> Result<(DoneStatus, Option<String>), HarnessError> {
    match frame["status"].as_str() {
        Some("completed") => Ok((DoneStatus::Completed, None)),
        Some("aborted") => Ok((DoneStatus::Interrupted, None)),
        Some("error") => Ok((DoneStatus::Errored, Some(frame["error"]["message"].as_str().ok_or_else(|| protocol("OMP prompt error has no message"))?.to_owned()))),
        _ => Err(protocol("OMP prompt_result has invalid status")),
    }
}
fn content_text(content: &Value) -> Option<String> {
    let text = content.as_array()?.iter().filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("\n");
    if text.is_empty() { None } else { Some(text) }
}

fn ui_questions(frame: &Value) -> Result<Option<Vec<UserInputQuestion>>, HarnessError> {
    let method = frame["method"].as_str().unwrap_or_default();
    let id = frame["id"].as_str().unwrap_or_default();
    if method == "ask" {
        let questions = frame["questions"].as_array().ok_or_else(|| protocol("OMP ask has no questions"))?;
        return questions.iter().map(|question| Ok(UserInputQuestion {
            id: question["id"].as_str().ok_or_else(|| protocol("OMP question has no id"))?.to_owned(),
            header: question["header"].as_str().unwrap_or_default().to_owned(),
            question: question["question"].as_str().unwrap_or_default().to_owned(),
            options: question["options"].as_array().ok_or_else(|| protocol("OMP question has no options"))?.iter().filter_map(|option| option["label"].as_str().map(str::to_owned)).collect(),
            multi_select: question["multi"].as_bool().unwrap_or(false), prefill: None, multiline: false, accepts_typed_answer: true,
        })).collect::<Result<Vec<_>, _>>().map(Some);
    }
    if !matches!(method, "select" | "confirm" | "input" | "editor") { return Ok(None); }
    Ok(Some(vec![UserInputQuestion {
        id: id.to_owned(), header: frame["title"].as_str().unwrap_or_default().to_owned(),
        question: frame["message"].as_str().or_else(|| frame["title"].as_str()).unwrap_or_default().to_owned(),
        options: if method == "confirm" { vec!["Yes".into(), "No".into()] } else { frame["options"].as_array().map(|options| options.iter().filter_map(|option| option.as_str().map(str::to_owned)).collect()).unwrap_or_default() },
        multi_select: false, prefill: frame["prefill"].as_str().map(str::to_owned), multiline: method == "editor", accepts_typed_answer: matches!(method, "input" | "editor"),
    }]))
}

fn ui_response(frame: &Value, questions: &[UserInputQuestion], answers: Option<&[UserInputAnswer]>) -> Value {
    let mut response = json!({"type":"extension_ui_response","id":frame["id"]});
    let Some(answers) = answers.filter(|answers| !answers.is_empty()) else { response["cancelled"] = json!(true); return response; };
    let values = |question: &UserInputQuestion| -> Vec<String> {
        answers.iter().filter(|answer| answer.question_id == question.id).flat_map(|answer| &answer.labels).map(|label| question.option_label(label).unwrap_or(label).to_owned()).collect()
    };
    if frame["method"] == "ask" {
        response["answers"] = Value::Array(questions.iter().map(|question| {
            let (selected, custom): (Vec<_>, Vec<_>) = values(question).into_iter().partition(|label| question.options.contains(label));
            let mut answer = json!({"id":question.id,"selectedOptions":selected});
            if !custom.is_empty() { answer["customInput"] = json!(custom.join("\n")); }
            answer
        }).collect());
    } else if let Some(question) = questions.first() {
        if let Some(value) = values(question).first() {
            if frame["method"] == "confirm" { response["confirmed"] = json!(value == "Yes"); }
            else { response["value"] = json!(value); }
        } else { response["cancelled"] = json!(true); }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_answers_preserve_selection_and_free_text() {
        let frame = json!({"id":"ask-1","method":"ask","questions":[{"id":"q1","question":"Choose","multi":true,"options":[{"label":"One"},{"label":"Two"}]}]});
        let questions = ui_questions(&frame).unwrap().unwrap();
        let answers = vec![UserInputAnswer { question_id: "q1".into(), labels: vec!["q1:1".into(), "Custom answer".into()] }];
        assert_eq!(ui_response(&frame, &questions, Some(&answers)), json!({"type":"extension_ui_response","id":"ask-1","answers":[{"id":"q1","selectedOptions":["Two"],"customInput":"Custom answer"}]}));
        assert_eq!(ui_response(&frame, &questions, None), json!({"type":"extension_ui_response","id":"ask-1","cancelled":true}));
    }

    #[test]
    fn model_selection_requires_unambiguous_native_identity() {
        let models = json!([{"provider":"first","id":"model"},{"provider":"second","id":"model"}]);
        assert!(select_model(&models, "model").is_err());
        assert_eq!(select_model(&models, "second/model").unwrap(), ("second".into(), "model".into()));
    }

    #[test]
    fn terminal_text_snapshots_are_not_duplicate_deltas() {
        assert_eq!(delta_event(&json!({"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"answer"}})), Some(AgentEvent::TextDelta { text: "answer".into() }));
        assert_eq!(delta_event(&json!({"assistantMessageEvent":{"type":"text_end","contentIndex":0,"content":"answer"}})), None);
        assert!(prompt_outcome(&json!({"status":"unknown"})).is_err());
        assert!(prompt_outcome(&json!({"status":"error","error":{}})).is_err());
    }
}
