use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use agent_harness::{LaunchContext, LaunchSpec, HarnessError};
use model_provider::{AuthMode, BackendTarget, CredentialStore, ModelSelection, Protocol, ProviderSnapshot, RoutingMode, Secret};
type Service = model_provider::ProviderService<crate::provider_store::ProviderDb, super::GpuiCredentialBroker>;

#[derive(Clone)]
pub struct ManagedRequest {
    pub service: Arc<Service>,
    pub counters: Arc<Mutex<HashMap<String, usize>>>,
    pub target: BackendTarget,
    pub selection: Option<ModelSelection>,
}

pub struct ResolvedManaged {
    pub provider: ProviderSnapshot,
    pub model_id: String,
    pub protocol: Protocol,
    pub keys: Vec<Secret>,
    pub headers: Vec<(String, Secret)>,
    pub index: usize,
    pub executable: std::path::PathBuf,
}

impl ManagedRequest {
    /// Runs on the GPUI foreground so credential broker requests can be serviced.
    pub async fn resolve(self) -> anyhow::Result<Option<ResolvedManaged>> {
        let preferences = self.service.load_preferences(self.target).await?;
        let selection = self.selection.or(preferences.default_model);
        let Some(ModelSelection::Managed { provider_id, model_id }) = selection else { return Ok(None); };
        let provider = self.service.get_provider(&provider_id).await?.ok_or_else(|| anyhow::anyhow!("Configured model service was deleted"))?;
        anyhow::ensure!(provider.enabled, "Configured model service is disabled");
        let model = provider.models.iter().find(|model| model.enabled && model.model_id == model_id)
            .ok_or_else(|| anyhow::anyhow!("Configured model is unavailable"))?;
        let protocol = if model.protocols.contains(&provider.default_protocol) { provider.default_protocol }
            else { *model.protocols.first().ok_or_else(|| anyhow::anyhow!("Model has no protocol"))? };
        anyhow::ensure!(provider.endpoints.iter().any(|endpoint| endpoint.protocol == protocol), "Model endpoint is unavailable");
        let mut keys = Vec::new();
        if provider.auth == AuthMode::ApiKey {
            for key in provider.keys.iter().filter(|key| key.enabled) {
                keys.push(self.service.credentials().get(&key.credential_ref).await?.ok_or_else(|| anyhow::anyhow!("An enabled API key is unavailable"))?);
            }
            anyhow::ensure!(!keys.is_empty(), "No enabled API key is configured");
        }
        let mut headers = Vec::new();
        for header in provider.headers.iter().filter(|header| header.enabled) {
            headers.push((header.name.clone(), self.service.credentials().get(&header.credential_ref).await?.ok_or_else(|| anyhow::anyhow!("A provider header is unavailable"))?));
        }
        let index = {
            let mut counters = self.counters.lock().map_err(|_| anyhow::anyhow!("Provider routing is unavailable"))?;
            let index = counters.entry(provider.id.clone()).or_default();
            let selected = *index; *index = index.wrapping_add(1); selected
        };
        let executable = preferences.launch_executable.unwrap_or_else(|| match self.target {
            BackendTarget::Pi => "pi".into(), BackendTarget::Omp => "omp".into(), BackendTarget::Dsh => "dsh".into(),
        });
        Ok(Some(ResolvedManaged { provider, model_id, protocol, keys, headers, index, executable }))
    }
}

impl ResolvedManaged {
    /// Runs inside Tokio, owns the gateway and support files until the child stream ends.
    pub async fn apply(mut self, target: BackendTarget, launch: &mut LaunchContext) -> Result<String, HarnessError> {
        let support = launch.support.as_ref().ok_or_else(|| HarnessError::Protocol("Managed launch has no support directory".into()))?;
        let endpoint = self.provider.endpoints.iter().find(|endpoint| endpoint.protocol == self.protocol).ok_or_else(|| HarnessError::Protocol("Managed endpoint is unavailable".into()))?.base_url.clone();
        let mut env = Vec::new();
        let use_gateway = self.provider.routing == RoutingMode::Gateway || self.provider.auth == AuthMode::None || (target == BackendTarget::Dsh && !self.headers.is_empty());
        let mut config_headers = Vec::new();
        if use_gateway {
            let gateway = crate::provider_services::managed::start_gateway(endpoint, self.protocol, self.keys, self.headers, self.provider.auth).await?;
            let url = gateway.base_url();
            for endpoint in &mut self.provider.endpoints { if endpoint.protocol == self.protocol { endpoint.base_url = url.clone(); } }
            env.push(("AUTOCODE_MANAGED_API_KEY".into(), Secret::new(gateway.token())));
            self.provider.auth = AuthMode::ApiKey;
            launch.gateway = Some(gateway);
        } else {
            if !self.keys.is_empty() {
                let index = self.index % self.keys.len();
                let key = self.keys.swap_remove(index);
                env.push(("AUTOCODE_MANAGED_API_KEY".into(), key));
            }
            for (index, (name, value)) in self.headers.into_iter().enumerate() {
                let variable = format!("AUTOCODE_MANAGED_HEADER_{index}");
                env.push((variable.clone(), value)); config_headers.push((name, variable));
            }
        }
        self.provider.default_protocol = self.protocol;
        let extension = agent_harness::projection::engine_config(target, &self.provider, &self.model_id, &config_headers).map_err(HarnessError::Protocol)?;
        let suffix = match target { BackendTarget::Pi => "js", BackendTarget::Omp => "ts", BackendTarget::Dsh => "yml" };
        let path = support.path().join(format!("managed.{suffix}"));
        std::fs::write(&path, extension).map_err(HarnessError::Io)?;
        let route = format!("autocode-{}", self.provider.id);
        let model = format!("{route}/{}", self.model_id);
        let mut args = match target {
            BackendTarget::Pi => vec!["--extension".into(), path.to_string_lossy().into_owned(), "--model".into(), model.clone()],
            BackendTarget::Omp => vec!["-e".into(), path.to_string_lossy().into_owned(), "--model".into(), model.clone()],
            BackendTarget::Dsh => vec!["--patch".into(), path.to_string_lossy().into_owned()],
        };
        if cfg!(windows) && target == BackendTarget::Pi && (self.executable == std::path::Path::new("pi") || self.executable.extension().is_some_and(|extension| extension == "cmd")) {
            let paths = std::env::var_os("PATH").unwrap_or_default();
            let shim = if self.executable.is_file() { Some(self.executable.clone()) } else { std::env::split_paths(&paths).map(|dir| dir.join("pi.cmd")).find(|path| path.is_file()) };
            if let Some(shim) = shim {
                let entry = shim.parent().unwrap_or(std::path::Path::new(".")).join("node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js");
                if !entry.is_file() { return Err(HarnessError::Protocol("Pi command shim requires an executable runtime entry".into())); }
                args.insert(0, entry.to_string_lossy().into_owned());
                self.executable = "node".into();
            }
        }
        launch.launch = Some(LaunchSpec { executable: self.executable, prefix_args: args, config_dir: None, env_refs: env.iter().map(|(name, _)| name.clone()).collect(), env });
        Ok(model)
    }
}

async fn start_gateway(endpoint: String, protocol: Protocol, keys: Vec<Secret>, headers: Vec<(String, Secret)>, auth: AuthMode) -> Result<Arc<agent_harness::gateway::GatewayServer>, HarnessError> {
    agent_harness::gateway::GatewayServer::start(endpoint, protocol, keys, headers, auth).await.map_err(|_| HarnessError::Protocol("Could not start the model gateway".into()))
}
