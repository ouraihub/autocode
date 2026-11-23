//! Pinned engine packages installed without lifecycle scripts or a command shell.
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use futures::AsyncReadExt as _;
use model_provider::BackendTarget;
use node_runtime::NodeRuntime;
use paths::data_dir;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use util::command::{Command, new_command};

pub(crate) fn name(target: BackendTarget) -> &'static str {
    match target {
        BackendTarget::Pi => "pi",
        BackendTarget::Omp => "omp",
        BackendTarget::Dsh => "dsh",
    }
}

pub(crate) fn package(target: BackendTarget) -> (&'static str, &'static str) {
    match target {
        BackendTarget::Pi => ("@earendil-works/pi-coding-agent", "1.4.0"),
        BackendTarget::Omp => ("@oh-my-pi/pi-coding-agent", "18.14.2"),
        BackendTarget::Dsh => ("@deepseek-ai/dsh", "0.2.1-rc.5"),
    }
}

pub(crate) fn root(target: BackendTarget) -> PathBuf {
    data_dir().join("agent_engines").join(name(target))
}

pub(crate) fn manifest(directory: &Path, target: BackendTarget) -> PathBuf {
    directory.join("node_modules").join(package(target).0).join("package.json")
}

pub(crate) fn entry_from_manifest(path: &Path, target: BackendTarget) -> Result<PathBuf> {
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let bin = match &value["bin"] {
        Value::String(value) => value.as_str(),
        Value::Object(values) => values.get(name(target)).and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{} package has no matching bin", name(target)))?,
        _ => bail!("{} package has no bin entry", name(target)),
    };
    // Validate both separators even when inspecting a Windows manifest on another OS.
    ensure!(!bin.is_empty() && !bin.contains(':') && !bin.starts_with(['/', '\\'])
        && !bin.split(['/', '\\']).any(|part| part == ".."), "invalid package bin entry");
    let relative = Path::new(bin);
    ensure!(!relative.components().any(|part| matches!(part,
        Component::ParentDir | Component::RootDir | Component::Prefix(_))), "invalid package bin entry");
    let directory = path.parent().ok_or_else(|| anyhow!("package has no parent"))?.canonicalize()?;
    let entry = directory.join(relative).canonicalize().context("locate package bin entry")?;
    ensure!(entry.starts_with(&directory) && entry.is_file(), "package bin escapes its directory or is not a file");
    Ok(entry)
}

pub(crate) fn entry(target: BackendTarget) -> Option<PathBuf> {
    entry_from_manifest(&manifest(&root(target), target), target).ok()
}

async fn run(command: &mut Command, action: &str) -> Result<()> {
    let output = command.output().await.with_context(|| format!("start {action}"))?;
    if !output.status.success() {
        let text = String::from_utf8_lossy(&output.stderr).into_owned()
            + &String::from_utf8_lossy(&output.stdout);
        let lines: Vec<_> = text.lines().filter(|line| !line.trim().is_empty()).collect();
        bail!("{action} failed ({}):\n{}", output.status,
            lines[lines.len().saturating_sub(12)..].join("\n"));
    }
    Ok(())
}

const BUN_VERSION: &str = "bun-v1.4.4";
const BUN_URL: &str = "https://github.com/oven-sh/bun/releases/download/bun-v1.4.4/bun-windows-x64-baseline.zip";
const BUN_SHA256: &str = "0f20ce6674d4f2dbcc8c5d59fdf99f7ba45f8c3b367e0f5781cbfa30a1c78d99";
const BUN_EXE: &str = "bun-windows-x64-baseline/bun.exe";

async fn remove_directory(path: &Path) -> Result<()> {
    match smol::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

fn supported_bun_version(version: &str) -> bool {
    let version = version.trim().trim_start_matches('v');
    let Some(core) = version.split('-').next() else { return false; };
    let mut parts = core.split('.').map(str::parse::<u64>);
    matches!((parts.next(), parts.next(), parts.next()),
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch))) if (major, minor, patch) >= (1, 4, 4))
}

pub(crate) async fn bun() -> Result<PathBuf> {
    if let Some(executable) = crate::engine_launch::find_program(Path::new("bun")) {
        if !crate::engine_launch::is_shell_shim(&executable) {
            let output = new_command(&executable).arg("--version").output().await.context("check Bun runtime version")?;
            if output.status.success() && supported_bun_version(&String::from_utf8_lossy(&output.stdout)) {
                return Ok(executable);
            }
        }
    }
    ensure!(cfg!(all(windows, target_arch = "x86_64")),
        "Automatic Bun installation is supported on Windows x64; install Bun on PATH on this platform");
    let runtimes = data_dir().join("agent_engines").join("runtimes");
    let target = runtimes.join(BUN_VERSION);
    let executable = target.join(BUN_EXE);
    if executable.is_file() { return Ok(executable); }
    smol::fs::create_dir_all(&runtimes).await?;
    let archive = runtimes.join(format!("{BUN_VERSION}.zip"));
    run(new_command("curl.exe").args(["-fsSL", "--retry", "2", "--connect-timeout", "30", "-o"])
        .arg(&archive).arg(BUN_URL), "download Bun").await?;
    let mut file = smol::fs::File::open(&archive).await?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).await?;
        if count == 0 { break; }
        digest.update(&buffer[..count]);
    }
    drop(file);
    if format!("{:x}", digest.finalize()) != BUN_SHA256 {
        smol::fs::remove_file(&archive).await.context("remove rejected Bun archive")?;
        bail!("Bun archive SHA-256 does not match the pinned release");
    }
    let staging = runtimes.join(format!("{BUN_VERSION}.part"));
    remove_directory(&staging).await?;
    smol::fs::create_dir_all(&staging).await?;
    run(new_command("tar.exe").arg("-xf").arg(&archive).arg("-C").arg(&staging), "extract Bun").await?;
    ensure!(staging.join(BUN_EXE).is_file(), "Bun archive has no runtime executable");
    remove_directory(&target).await?;
    smol::fs::rename(&staging, &target).await?;
    if let Err(error) = smol::fs::remove_file(&archive).await {
        log::warn!("Could not remove Bun runtime archive: {error}");
    }
    Ok(executable)
}

pub(crate) async fn runtime(target: BackendTarget, node: Option<NodeRuntime>) -> Result<PathBuf> {
    if target == BackendTarget::Omp { return bun().await; }
    match node {
        Some(node) => node.binary_path().await.context("locate Node.js runtime"),
        None => crate::engine_launch::find_program(Path::new("node"))
            .filter(|path| !crate::engine_launch::is_shell_shim(path))
            .ok_or_else(|| anyhow!("Node.js runtime is unavailable")),
    }
}

pub(crate) async fn install(target: BackendTarget, node: Option<NodeRuntime>) -> Result<()> {
    let directory = root(target);
    smol::fs::create_dir_all(&directory).await.context("create engine install directory")?;
    let (package, version) = package(target);
    let spec = format!("{package}@{version}");
    if target == BackendTarget::Omp {
        let executable = bun().await?;
        let manifest = directory.join("package.json");
        if !manifest.is_file() { smol::fs::write(&manifest, "{\"private\":true}\n").await?; }
        let path = std::env::join_paths(executable.parent().into_iter().map(Path::to_path_buf)
            .chain(std::env::var_os("PATH").iter().flat_map(std::env::split_paths)))?;
        run(new_command(&executable).current_dir(&directory).env("PATH", path)
            .args(["add", "--exact", "--ignore-scripts", &spec]), "install OMP").await?;
    } else {
        let node = node.ok_or_else(|| anyhow!("Node.js runtime is unavailable"))?;
        let npm = node.npm_command(Some(&directory), "install", &[
            "--no-fund", "--no-audit", "--ignore-scripts", "--omit=dev", "--save-exact", &spec,
        ]).await?;
        let mut command = if crate::engine_launch::is_shell_shim(&npm.path) {
            // System NodeRuntime may return npm.cmd. Invoke npm's JS entry directly, never cmd.exe.
            let cli = npm.path.parent().ok_or_else(|| anyhow!("npm has no parent directory"))?
                .join("node_modules/npm/bin/npm-cli.js");
            ensure!(cli.is_file(), "npm command shim has no npm-cli.js entry");
            let mut command = new_command(node.binary_path().await?);
            command.arg(cli);
            command
        } else {
            new_command(&npm.path)
        };
        command.args(&npm.args).envs(&npm.env).current_dir(&directory);
        run(&mut command, &format!("install {}", name(target))).await?;
    }
    entry(target).ok_or_else(|| anyhow!("{} installed but its package entry point was not found", name(target)))?;
    Ok(())
}

pub(crate) async fn uninstall(target: BackendTarget) -> Result<()> {
    remove_directory(&root(target)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_bun_does_not_satisfy_omp_runtime_requirement() {
        assert!(!supported_bun_version("1.4.0\n"));
        assert!(!supported_bun_version("not a version"));
        assert!(supported_bun_version("1.4.4\n"));
        assert!(supported_bun_version("v1.5.0"));
    }

    #[test]
    fn manifest_bin_rejects_traversal_and_accepts_an_existing_entry() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("package.json");
        let write_bin = |bin: &str| std::fs::write(&manifest,
            serde_json::to_vec(&serde_json::json!({"bin": {"dsh": bin}})).unwrap()).unwrap();
        std::fs::write(directory.path().join("cli.js"), "").unwrap();
        write_bin("./cli.js");
        assert_eq!(entry_from_manifest(&manifest, BackendTarget::Dsh).unwrap(),
            directory.path().join("cli.js").canonicalize().unwrap());
        for bin in ["../cli.js", "dist/../../cli.js", "..\\cli.js", "/cli.js", "C:\\cli.js", ""] {
            write_bin(bin);
            assert!(entry_from_manifest(&manifest, BackendTarget::Dsh).is_err(), "accepted {bin}");
        }
    }
}
