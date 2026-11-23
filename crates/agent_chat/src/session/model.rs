use agent_harness::{PermissionMode, ReasoningLevel};
use model_provider::{ConfigSource, ModelSelection};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunSettings {
    #[serde(default, deserialize_with = "deserialize_model_selection")]
    pub model: Option<ModelSelection>,
    #[serde(default)]
    pub config_source: ConfigSource,
    #[serde(default)]
    pub reasoning: Option<ReasoningLevel>,
    #[serde(default)]
    pub options: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub permission: PermissionMode,
}

impl RunSettings {
    pub fn native_model_id(&self) -> Option<String> {
        match &self.model {
            Some(ModelSelection::Native { id }) => Some(id.clone()),
            Some(ModelSelection::Managed { .. }) | None => None,
        }
    }

    pub fn native_model(&self) -> Option<&str> {
        match &self.model {
            Some(ModelSelection::Native { id }) => Some(id),
            Some(ModelSelection::Managed { .. }) | None => None,
        }
    }
}

fn deserialize_model_selection<'de, D>(deserializer: D) -> std::result::Result<Option<ModelSelection>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(id)) => Ok(Some(ModelSelection::Native { id })),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub tokens: Option<u64>,
    pub window: Option<u64>,
}

impl ContextSnapshot {
    pub fn fraction(self) -> Option<f64> {
        Some(self.tokens? as f64 / self.window.filter(|window| *window > 0)? as f64)
    }
}
