//! Public provider templates, matching auto-craft's LumaKit support catalog 276751df.
use crate::Protocol;

/// A public endpoint and key-registration page; never contains credentials or models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub protocol: Protocol,
    pub api_key_url: &'static str,
}

/// Templates are seeded disabled; existing user configuration is never replaced.
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset { id: "qiniu", name: "七牛云", base_url: "https://api.qnaigc.com", protocol: Protocol::OpenaiChat, api_key_url: "https://s.qiniu.com/BfyeU3" },
    ProviderPreset { id: "siliconflow", name: "硅基流动", base_url: "https://api.siliconflow.cn/v1", protocol: Protocol::OpenaiChat, api_key_url: "https://cloud.siliconflow.cn/i/ieP7r6Bx" },
    ProviderPreset { id: "fenno", name: "Fenno", base_url: "https://api.fenno.ai/v1", protocol: Protocol::OpenaiChat, api_key_url: "https://api.fenno.ai/s/vWbM" },
];

/// Looks up stable identity even after the user renames the provider.
pub fn provider_preset(id: &str) -> Option<&'static ProviderPreset> {
    PROVIDER_PRESETS.iter().find(|preset| preset.id == id)
}
