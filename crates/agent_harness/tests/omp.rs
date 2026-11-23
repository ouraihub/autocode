use std::{path::PathBuf, time::Duration};

use agent_harness::{AgentEvent, CancellationToken, CatalogContext, DoneStatus, Harness, HarnessError, LaunchContext, LaunchSpec, OmpHarness, PermissionMode, ReasoningLevel, RunControls, RunRequest, UserInputAnswer};
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

fn launch(scenario: &str) -> LaunchSpec {
    LaunchSpec {
        executable: PathBuf::from("node"),
        prefix_args: vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omp-peer.cjs").to_string_lossy().into_owned(), scenario.into()],
        config_dir: None, env_refs: vec![], env: vec![],
    }
}

fn request() -> RunRequest {
    RunRequest {
        prompt: "Answer the question".into(), model: None, reasoning: None,
        model_options: serde_json::Map::new(), cwd: String::new(), permission: PermissionMode::FullAccess,
        resume: None, fork: None, attachments: vec![], skills: vec![], auto_compact_tokens: None,
        chrome: false, drop_prompt: None, ephemeral: false,
    }
}

fn controls() -> RunControls {
    RunControls {
        request_input: Box::new(|questions| {
            let (sender, receiver) = oneshot::channel();
            let answers = questions.iter().map(|question| UserInputAnswer { question_id: question.id.clone(), labels: vec![question.option_id(0)] }).collect();
            sender.send(answers).unwrap();
            receiver
        }),
        steering: mpsc::channel(1).1,
        interrupt: CancellationToken::new(),
        permission: tokio::sync::watch::channel(PermissionMode::FullAccess).1,
    }
}

async fn run(scenario: &str, request: RunRequest) -> Vec<Result<AgentEvent, HarnessError>> {
    let stream = OmpHarness::new().run(request, controls(), LaunchContext { launch: Some(launch(scenario)), ..Default::default() }).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), stream.collect()).await.unwrap()
}

#[tokio::test]
async fn real_delta_frames_stream_once_and_wait_for_background_settlement() {
    let events = run("stream", request()).await.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    let text: String = events.iter().filter_map(|event| match event { AgentEvent::TextDelta { text } => Some(text.as_str()), _ => None }).collect();
    assert_eq!(text, "Hello from background");
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ReasoningDelta { text } if text == "Plan")));
    assert!(events.iter().any(|event| matches!(event, AgentEvent::Usage { input_tokens: 12, output_tokens: 3 })));
    assert!(matches!(events.last(), Some(AgentEvent::Done { status: DoneStatus::Completed, session_id: Some(id), .. }) if id == "saved-session.jsonl"));
    assert_eq!(events.iter().filter(|event| matches!(event, AgentEvent::Done { .. })).count(), 1);
}

#[tokio::test]
async fn selected_model_reasoning_and_resume_are_applied_before_prompt() {
    let mut request = request();
    request.model = Some("native/answer/model".into());
    request.reasoning = Some(ReasoningLevel::High);
    request.resume = Some("resume-session.jsonl".into());
    let events = run("selected", request).await.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ModelReported { model_id, provider_id: Some(provider), reasoning: Some(ReasoningLevel::High) } if model_id == "answer/model" && provider == "native")));
    assert!(events.iter().any(|event| matches!(event, AgentEvent::Done { status: DoneStatus::Completed, .. })));
}

#[tokio::test]
async fn catalog_uses_native_provider_model_identities() {
    let models = OmpHarness::new().models(CatalogContext { launch: Some(launch("catalog")), ..Default::default() }).await.unwrap();
    assert_eq!(models.iter().map(|model| (model.id.as_str(), model.label.as_str())).collect::<Vec<_>>(), vec![("native/first", "First model"), ("native/answer/model", "Answer model")]);
    assert!(models[0].reasoning_levels.is_empty());
    assert!(models[1].reasoning_levels.contains(&ReasoningLevel::High));
}

#[tokio::test]
async fn provider_errors_and_aborts_are_not_reported_as_success() {
    let events = run("provider-error", request()).await.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    assert!(matches!(events.last(), Some(AgentEvent::Done { status: DoneStatus::Errored, error: Some(message), .. }) if message == "Provider rejected credentials"));
    let events = run("aborted", request()).await.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    assert!(matches!(events.last(), Some(AgentEvent::Done { status: DoneStatus::Interrupted, .. })));
    let events = run("command-error", request()).await;
    assert!(matches!(events.last(), Some(Err(HarnessError::Protocol(message))) if message == "Prompt rejected"));
}

#[tokio::test]
async fn local_commands_complete_without_prompt_result() {
    let events = run("local", request()).await.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    assert!(events.iter().any(|event| matches!(event, AgentEvent::TextDelta { text } if text == "Local command output")));
    assert!(matches!(events.last(), Some(AgentEvent::Done { status: DoneStatus::Completed, .. })));
}

#[tokio::test]
async fn rpc_ui_dialog_replies_unblock_the_response() {
    let events = run("dialog", request()).await.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    assert!(events.iter().any(|event| matches!(event, AgentEvent::InputRequested { request_id, .. } if request_id == "approval-1")));
    assert!(events.iter().any(|event| matches!(event, AgentEvent::InputResolved { request_id } if request_id == "approval-1")));
    assert!(events.iter().any(|event| matches!(event, AgentEvent::TextDelta { text } if text == "Approved response")));
    assert!(matches!(events.last(), Some(AgentEvent::Done { status: DoneStatus::Completed, .. })));
}

#[tokio::test]
async fn missing_selected_model_fails_instead_of_using_an_unrelated_default() {
    let mut request = request();
    request.model = Some("native/missing".into());
    let events = run("stream", request).await;
    assert!(matches!(events.last(), Some(Err(HarnessError::Protocol(message))) if message == "OMP model not available: native/missing"));
    assert!(!events.iter().any(|event| matches!(event, Ok(AgentEvent::TextDelta { .. }))));
}

#[tokio::test]
async fn catalog_startup_exit_reports_stderr_after_stdout_eof() {
    let error = OmpHarness::new().models(CatalogContext { launch: Some(launch("startup-exit")), ..Default::default() }).await.unwrap_err();
    assert!(matches!(error, HarnessError::Protocol(message) if message.contains("Bun runtime must be >= 1.4.4 (found v1.4.0)")));
}

#[tokio::test]
async fn run_startup_exit_reports_one_error_without_completion() {
    let events = run("startup-exit", request()).await;
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], Err(HarnessError::Protocol(message)) if message.contains("Bun runtime must be >= 1.4.4 (found v1.4.0)")));
}


#[tokio::test]
async fn invalid_frames_keep_the_original_protocol_error() {
    let events = run("invalid-frame-exit", request()).await;
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], Err(HarnessError::Protocol(message)) if message.starts_with("omp frame is not JSON:")));
    let error = OmpHarness::new().models(CatalogContext { launch: Some(launch("invalid-frame-exit")), ..Default::default() }).await.unwrap_err();
    assert!(matches!(error, HarnessError::Protocol(message) if message.starts_with("omp frame is not JSON:")));
}
