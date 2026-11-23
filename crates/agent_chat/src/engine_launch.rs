//! Shared native and managed launch resolution for every coding partner.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use agent_harness::{LaunchContext, LaunchSpec};
use model_provider::BackendTarget;
use node_runtime::NodeRuntime;

use crate::dsh_install::{self, name};

pub(crate) fn is_shell_shim(path: &Path) -> bool {
    path.extension().is_some_and(|extension|
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat"))
}

fn candidate(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    if path.extension().is_none() {
        for extension in ["exe", "cmd", "bat"] {
            let candidate = path.with_extension(extension);
            if candidate.is_file() { return Some(candidate); }
        }
    }
    path.is_file().then(|| path.to_path_buf())
}

pub(crate) fn find_program(program: &Path) -> Option<PathBuf> {
    if program.is_absolute() || program.components().count() > 1 {
        return candidate(program);
    }
    std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .find_map(|directory| candidate(&directory.join(program))))
}

pub(crate) fn discover(target: BackendTarget) -> Option<PathBuf> {
    let key = format!("{}_EXECUTABLE", name(target).to_ascii_uppercase());
    if let Some(program) = std::env::var_os(key) {
        return find_program(Path::new(&program));
    }
    find_program(Path::new(name(target)))
}

pub(crate) fn managed_entry(target: BackendTarget) -> Option<PathBuf> {
    dsh_install::entry(target)
}

pub(crate) async fn install(target: BackendTarget, node: Option<NodeRuntime>) -> Result<()> {
    dsh_install::install(target, node).await
}

pub(crate) async fn uninstall(target: BackendTarget) -> Result<()> {
    dsh_install::uninstall(target).await
}

fn shim_entry(shim: &Path, target: BackendTarget) -> Result<Option<PathBuf>> {
    let Some(directory) = shim.parent() else { return Ok(None); };
    if shim.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("exe")) {
        if !shim.with_extension("bunx").is_file() { return Ok(None); }
        for root in directory.ancestors() {
            let manifest = dsh_install::manifest(root, target);
            if manifest.is_file() {
                return dsh_install::entry_from_manifest(&manifest, target).map(Some)
                    .with_context(|| format!("read {} Bun command manifest", name(target)));
            }
        }
        bail!("{} Bun command shim has no package manifest", name(target));
    }
    let manifest = dsh_install::manifest(directory, target);
    if manifest.is_file() {
        return dsh_install::entry_from_manifest(&manifest, target).map(Some)
            .with_context(|| format!("read {} command manifest", name(target)));
    }
    // Local npm/bun shims live in node_modules/.bin, not beside node_modules.
    if directory.file_name().is_some_and(|name| name == ".bin") {
        if let Some(root) = directory.parent().and_then(Path::parent) {
            let manifest = dsh_install::manifest(root, target);
            if manifest.is_file() {
                return dsh_install::entry_from_manifest(&manifest, target).map(Some)
                    .with_context(|| format!("read {} command manifest", name(target)));
            }
        }
    }
    Ok(None)
}

fn spec(executable: PathBuf) -> LaunchSpec {
    LaunchSpec { executable, prefix_args: Vec::new(), config_dir: None,
        env_refs: Vec::new(), env: Vec::new() }
}

pub(crate) async fn apply(target: BackendTarget, node: Option<NodeRuntime>, launch: &mut LaunchContext) -> Result<()> {
    let default = launch.launch.as_ref().is_none_or(|spec| spec.executable == Path::new(name(target)));
    let managed = if default { managed_entry(target) } else { None };
    let is_managed = managed.is_some();
    let executable = if let Some(entry) = managed {
        entry
    } else if default {
        discover(target).ok_or_else(|| anyhow!("{} is not installed", name(target)))?
    } else {
        let requested = &launch.launch.as_ref()
            .ok_or_else(|| anyhow!("{} launch has no executable", name(target)))?.executable;
        find_program(requested).ok_or_else(|| anyhow!("{} executable was not found: {}", name(target), requested.display()))?
    };
    let script = is_managed || executable.extension().is_some_and(|ext|
        ["js", "mjs", "cjs", "ts"].iter().any(|value| ext.eq_ignore_ascii_case(value)));
    let entry = if script {
        None
    } else {
        let entry = shim_entry(&executable, target)?;
        if entry.is_none() && (is_shell_shim(&executable)
            || executable.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("ps1"))) {
            bail!("{} command shim has no package manifest entry: {}", name(target), executable.display());
        }
        entry
    };
    let (runtime, entry) = if script || entry.is_some() {
        (dsh_install::runtime(target, node).await?, Some(entry.unwrap_or(executable)))
    } else {
        (executable, None)
    };
    let spec = launch.launch.get_or_insert_with(|| spec(PathBuf::new()));
    spec.executable = runtime;
    if let Some(entry) = entry {
        spec.prefix_args.insert(0, entry.to_string_lossy().into_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bun_executable_shim_uses_package_entry_instead_of_old_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join(".bun/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let shim = bin.join("omp.exe");
        std::fs::write(&shim, b"binary shim").unwrap();
        std::fs::write(shim.with_extension("bunx"), b"shim metadata").unwrap();
        let manifest = dsh_install::manifest(directory.path(), BackendTarget::Omp);
        let package = manifest.parent().unwrap();
        std::fs::create_dir_all(package.join("dist")).unwrap();
        std::fs::write(package.join("dist/cli.js"), "").unwrap();
        std::fs::write(&manifest, r#"{"bin":{"omp":"dist/cli.js"}}"#).unwrap();
        assert_eq!(shim_entry(&shim, BackendTarget::Omp).unwrap(), Some(package.join("dist/cli.js").canonicalize().unwrap()));
        std::fs::remove_file(shim.with_extension("bunx")).unwrap();
        assert_eq!(shim_entry(&shim, BackendTarget::Omp).unwrap(), None);
    }
}
