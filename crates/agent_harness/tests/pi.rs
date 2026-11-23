
use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use agent_harness::{
    AgentEvent, CancellationToken, Harness, LaunchContext, PermissionMode, PiHarness, RunControls,
    RunRequest, SteerMessage, UserInputAnswer,
};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-pi.sh")
}

fn request() -> RunRequest {
    RunRequest {
        prompt: "hello".into(),
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: String::new(),
        permission: PermissionMode::FullAccess,
        resume: None,
        fork: None,
        attachments: Vec::new(),
        skills: Vec::new(),
        auto_compact_tokens: None,
        chrome: false,
        drop_prompt: None,
        ephemeral: false,
    }
}

fn controls() -> RunControls {
    let (_steer_tx, steer_rx) = mpsc::channel::<SteerMessage>(1);
    RunControls {
        request_input: Box::new(|questions| {
            let (tx, rx) = oneshot::channel();
            let answers = questions
                .into_iter()
                .map(|question| UserInputAnswer {
                    question_id: question.id,
                    labels: Vec::new(),
                })
                .collect();
            tx.send(answers).expect("receiver is still held");
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        permission: tokio::sync::watch::channel(PermissionMode::FullAccess).1,
    }
}

#[tokio::test]
async fn pi_transmits_session_started_from_state_response() {
    let harness = PiHarness::new().with_executable(fixture_path());
    let stream = harness
        .run(request(), controls(), LaunchContext::default())
        .await
        .expect("Pi run starts");
    let events = tokio::time::timeout(
        Duration::from_secs(5),
        stream.map(|event| event.expect("Pi event is valid")).collect::<Vec<_>>(),
    )
    .await
    .expect("Pi fake CLI finishes");

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "/tmp/pi-session"
    )));
}

async fn protocol_events(scenario: &str) -> Vec<AgentEvent> {
    let launch = LaunchContext {
        launch: Some(agent_harness::LaunchSpec {
            executable: "node".into(),
            prefix_args: vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi-peer.cjs").to_string_lossy().into_owned(), scenario.into()],
            config_dir: None, env_refs: vec![], env: vec![],
        }),
        ..Default::default()
    };
    let stream = PiHarness::new().run(request(), controls(), launch).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), stream.map(|event| event.unwrap()).collect()).await.unwrap()
}

#[tokio::test]
async fn pi_streams_real_deltas_and_completes_tools_without_duplicate_text() {
    let events = protocol_events("success").await;
    let text: String = events.iter().filter_map(|event| match event { AgentEvent::TextDelta { text } => Some(text.as_str()), _ => None }).collect();
    assert_eq!(text, "Hello Pi");
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ReasoningDelta { text } if text == "Plan")));
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolCall { id, call: agent_harness::ToolCall::Unknown { input: Some(input), .. } } if id == "bash-1" && input["command"] == "echo fixture")));
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolResult { id, is_error: false, output: Some(output), .. } if id == "bash-1" && output == "fixture output")));
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ModelReported { model_id, provider_id: Some(provider), .. } if model_id == "fixture-chat" && provider == "fixture")));
    assert!(matches!(events.last(), Some(AgentEvent::Done { status: agent_harness::DoneStatus::Completed, session_id: Some(session), .. }) if session == "fixture-session"));
}

#[tokio::test]
async fn pi_provider_errors_and_aborts_are_not_successful_turns() {
    let errors = protocol_events("error").await;
    assert!(matches!(errors.last(), Some(AgentEvent::Done { status: agent_harness::DoneStatus::Errored, error: Some(error), .. }) if error == "Controlled provider failure"));
    let aborted = protocol_events("aborted").await;
    assert!(matches!(aborted.last(), Some(AgentEvent::Done { status: agent_harness::DoneStatus::Interrupted, .. })));
}
