use std::{path::PathBuf, time::Duration};

use agent_harness::{AgentEvent, CancellationToken, DoneStatus, DshHarness, Harness, LaunchContext, LaunchSpec, OmpHarness, PermissionMode, PiHarness, ReasoningLevel, RunControls, RunRequest};
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

fn controls() -> RunControls {
    RunControls {
        request_input: Box::new(|_| { let (_sender, receiver) = oneshot::channel(); receiver }),
        steering: mpsc::channel(1).1,
        interrupt: CancellationToken::new(),
        permission: tokio::sync::watch::channel(PermissionMode::FullAccess).1,
    }
}

async fn run(mode: &str, level: ReasoningLevel) -> Vec<AgentEvent> {
    let harness: Box<dyn Harness> = match mode {
        "pi" => Box::new(PiHarness::new()),
        "omp" => Box::new(OmpHarness::new()),
        "dsh" => Box::new(DshHarness::new()),
        _ => unreachable!(),
    };
    let request = RunRequest {
        prompt: "Report selected effort".into(), model: None, reasoning: Some(level),
        model_options: serde_json::Map::new(), cwd: String::new(), permission: PermissionMode::FullAccess,
        resume: None, fork: None, attachments: vec![], skills: vec![], auto_compact_tokens: None,
        chrome: false, drop_prompt: None, ephemeral: false,
    };
    let launch = LaunchContext {
        launch: Some(LaunchSpec {
            executable: PathBuf::from("node"),
            prefix_args: vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reasoning-peer.cjs").to_string_lossy().into_owned(), mode.into()],
            config_dir: None, env_refs: vec![], env: vec![],
        }),
        ..Default::default()
    };
    let stream = harness.run(request, controls(), launch).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), stream.map(|event| event.unwrap()).collect()).await.unwrap()
}

#[tokio::test]
async fn requested_off_and_high_are_applied_before_prompt() {
    for (level, expected) in [(ReasoningLevel::Off, "off"), (ReasoningLevel::High, "high")] {
        for mode in ["pi", "omp", "dsh"] {
            let events = run(mode, level).await;
            assert!(events.iter().any(|event| matches!(event, AgentEvent::Done { status: DoneStatus::Completed, .. })), "{mode}");
            if mode != "dsh" {
                assert!(events.iter().any(|event| matches!(event, AgentEvent::TextDelta { text } if text == expected)), "{mode}");
            }
        }
    }
}
