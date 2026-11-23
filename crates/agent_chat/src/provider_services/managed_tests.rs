use super::managed::*;
use model_provider::{AuthMode, BackendTarget, EndpointSnapshot, Protocol, ProviderModel, ProviderSnapshot, RoutingMode, Secret};

#[tokio::test]
async fn gateway_launch_injects_only_token_and_releases_support() {
    let directory = tempfile::tempdir().unwrap();
    let support_path = directory.path().join("run");
    let mut launch = agent_harness::LaunchContext { support: Some(agent_harness::RunSupportLease::create(support_path.clone()).unwrap()), ..Default::default() };
    let provider = ProviderSnapshot {
        id: "p1".into(), preset_id: None, revision: 1, name: "Local".into(), enabled: true, routing: RoutingMode::Gateway,
        auth: AuthMode::ApiKey, default_protocol: Protocol::OpenaiChat,
        endpoints: vec![EndpointSnapshot { protocol: Protocol::OpenaiChat, base_url: "http://127.0.0.1:9/v1".into() }],
        models: vec![ProviderModel { model_id: "m1".into(), model_type: model_provider::ModelType::Chat, name: "Model".into(), protocols: vec![Protocol::OpenaiChat], context_window: Some(32000), max_output_tokens: Some(4096), supports_vision: false, supports_tools: true, supports_reasoning: false, efforts: vec![], enabled: true }],
        keys: vec![], headers: vec![], sort: 0, created_at: 0, updated_at: 0,
    };
    let managed = ResolvedManaged { provider, model_id: "m1".into(), protocol: Protocol::OpenaiChat,
        keys: vec![Secret::new("never-write-this-key")], headers: vec![], index: 0, executable: "fake-pi".into() };
    let model = managed.apply(BackendTarget::Pi, &mut launch).await.unwrap();
    assert_eq!(model, "autocode-p1/m1");
    let spec = launch.launch.as_ref().unwrap();
    assert_eq!(spec.env[0].1.expose(), launch.gateway.as_ref().unwrap().token());
    assert_ne!(spec.env[0].1.expose(), "never-write-this-key");
    let extension = std::fs::read_to_string(support_path.join("managed.js")).unwrap();
    assert!(!extension.contains("never-write-this-key"));
    let address = launch.gateway.as_ref().unwrap().base_url().split('/').nth(2).unwrap().to_owned();
    drop(launch);
    tokio::task::yield_now().await;
    assert!(!support_path.exists());
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}
