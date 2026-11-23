//! Validated launch-spec utilities shared by Native/Managed drivers.
//!
//! No shell is involved. Windows command shims are rejected here rather than
//! assembled into `cmd /c`; callers must provide an explicit runtime + entry.

use std::path::Path;

use crate::{HarnessError, LaunchSpec};

const FORBIDDEN: [&str; 9] = [
    "--mode", "--session", "--resume", "--continue", "--fork", "--print", "--approval-mode",
    "--profile", "--config",
];

pub fn validate_launch_spec(spec: &LaunchSpec) -> Result<(), HarnessError> {
    if spec.executable.as_os_str().is_empty() {
        return Err(HarnessError::Protocol("launch executable is empty".into()));
    }
    for arg in &spec.prefix_args {
        let key = arg.split_once('=').map_or(arg.as_str(), |(key, _)| key);
        if FORBIDDEN.contains(&key) {
            return Err(HarnessError::Protocol(format!(
                "launch prefix argument {arg:?} is reserved"
            )));
        }
        if arg.contains(['\0', '\r', '\n']) {
            return Err(HarnessError::Protocol("launch argument contains a control character".into()));
        }
    }
    if spec.executable.extension().is_some_and(|ext| {
        matches!(ext.to_str(), Some("cmd" | "bat"))
    }) {
        return Err(HarnessError::Protocol(
            "Windows command shims require an explicit runtime and entry".into(),
        ));
    }
    Ok(())
}

pub fn validate_entry(executable: &Path, entry: &Path) -> Result<(), HarnessError> {
    if !executable.is_file() {
        return Err(HarnessError::NotInstalled(executable.display().to_string()));
    }
    if !entry.is_file() {
        return Err(HarnessError::NotInstalled(entry.display().to_string()));
    }
    Ok(())
}
pub fn managed_config_dir(support: &std::path::Path, chat_id: &str, generation: u64) -> std::path::PathBuf {
    support.join("managed").join(format!("{chat_id}-{generation}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(arg: &str) -> LaunchSpec {
        LaunchSpec {
            executable: Path::new("agent").into(),
            prefix_args: vec![arg.into()],
            config_dir: None,
            env_refs: Vec::new(),
            env: Vec::new(),
        }
    }

    #[test]
    fn rejects_reserved_prefix_args_in_equals_form() {
        let error = validate_launch_spec(&spec("--resume=old")).unwrap_err();
        assert!(error.to_string().contains("reserved"));
    }

    #[test]
    fn accepts_non_host_control_arguments() {
        validate_launch_spec(&spec("--extension=autocode")).unwrap();
    }
}
