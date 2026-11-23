//! Uninstall only fixed engine packages whose global package-manager ownership is proven.
use std::{ffi::OsString, io::Read as _, path::{Path, PathBuf}, time::Duration};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use model_provider::BackendTarget;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use util::command::{Command, new_command};

use crate::{dsh_install::{self, package}, engine_launch::{find_program, is_shell_shim}};
use util::paths::SanitizedPath;

fn command_path(path: &Path) -> &Path { SanitizedPath::new(path).as_path() }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Manager { Npm, Bun, Pnpm }

impl Manager {
    fn name(self) -> &'static str {
        match self { Self::Npm => "npm", Self::Bun => "bun", Self::Pnpm => "pnpm" }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Runner {
    program: PathBuf,
    arguments: Vec<OsString>,
    directory: PathBuf,
}

impl Runner {
    fn command(&self) -> Command {
        let mut command = new_command(command_path(&self.program));
        command.args(self.arguments.iter().map(|argument| command_path(Path::new(argument))))
            .current_dir(command_path(&self.directory)).kill_on_drop(true);
        command
    }
}

/// A confirmation snapshot, not permission to remove an executable directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UninstallPlan {
    target: BackendTarget,
    executable: PathBuf,
    manager: Manager,
    runner: Runner,
    root: PathBuf,
    bin: PathBuf,
    prefix: PathBuf,
    entry: PathBuf,
    manifest_digest: Vec<u8>,
    executable_digest: Vec<u8>,
    entry_digest: Vec<u8>,
    companion_digest: Option<Vec<u8>>,
}

impl UninstallPlan {
    /// Describes the exact package-manager operation shown before confirmation.
    pub(crate) fn description(&self) -> String {
        format!("{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {}",
            i18n::t!("Package manager"), self.manager.name(),
            i18n::t!("Package"), package(self.target).0,
            i18n::t!("Installation directory"), self.root.display(),
            i18n::t!("Executable"), self.executable.display(),
            i18n::t!("Command"), command_display(&self.uninstall_command()))
    }

    fn uninstall_command(&self) -> Command {
        let mut command = self.runner.command();
        match self.manager {
            Manager::Npm => { command.args(["--global", "--prefix"]).arg(command_path(&self.prefix))
                .args(["uninstall", "--ignore-scripts", package(self.target).0]); }
            Manager::Bun => { command.env("BUN_INSTALL_GLOBAL_DIR", command_path(&self.prefix))
                .env("BUN_INSTALL_BIN", command_path(&self.bin))
                .args(["remove", "-g", "--ignore-scripts", package(self.target).0]); }
            Manager::Pnpm => { command.args(["--global-dir"]).arg(command_path(&self.prefix))
                .arg("--global-bin-dir").arg(command_path(&self.bin))
                .args(["remove", "--global", "--ignore-scripts", package(self.target).0]); }
        }
        command
    }

    /// Revalidates ownership and the confirmation snapshot before invoking the manager.
    ///
    /// # Errors
    /// Rejects changed installations, manager failures, timeouts, and incomplete removal.
    pub(crate) async fn execute(&self) -> Result<()> {
        self.revalidate_files()?;
        let current = detect(self.target, self.executable.clone()).await
            .context("Installation changed or ownership can no longer be verified; refresh and confirm again")?;
        ensure!(&current == self, "Installation changed; refresh and confirm again before uninstalling");
        self.verify_destination().await?;
        self.revalidate_files()?;
        run(&mut self.uninstall_command(), "uninstall engine", Duration::from_secs(120)).await?;
        ensure!(!self.root.join(package(self.target).0).try_exists()?,
            "{} reported success but the package is still installed; inspect it with {} before retrying",
            self.manager.name(), self.manager.name());
        Ok(())
    }

    async fn verify_destination(&self) -> Result<()> {
        let mut command = self.runner.command();
        match self.manager {
            Manager::Npm => { command.args(["--global", "--prefix"]).arg(command_path(&self.prefix)).arg("root"); }
            Manager::Bun => { command.env("BUN_INSTALL_GLOBAL_DIR", command_path(&self.prefix))
                .env("BUN_INSTALL_BIN", command_path(&self.bin)).args(["pm", "ls", "-g"]); }
            Manager::Pnpm => { command.arg("--global-dir").arg(command_path(&self.prefix))
                .arg("--global-bin-dir").arg(command_path(&self.bin)).args(["root", "-g"]); }
        }
        let output = run(&mut command, "verify uninstall destination", Duration::from_secs(15)).await?;
        let root = if self.manager == Manager::Bun { bun_root(&output)? } else { output_path(&output)? };
        ensure!(root == self.root, "Package-manager destination changed; uninstall manually using the original global directory");
        Ok(())
    }

    fn revalidate_files(&self) -> Result<()> {
        let current = prove(self.target, &self.executable, self.manager, self.runner.clone(),
            self.root.clone(), self.bin.clone(), self.prefix.clone())
            .context("Installation changed; refresh and confirm again before uninstalling")?;
        ensure!(current.as_ref() == Some(self), "Installation changed; refresh and confirm again before uninstalling");
        Ok(())
    }
}

fn command_display(command: &Command) -> String {
    std::iter::once(command.get_program()).chain(command.get_args())
        .map(|argument| format!("{:?}", argument)).collect::<Vec<_>>().join(" ")
}

async fn run(command: &mut Command, action: &str, timeout: Duration) -> Result<String> {
    // Do not surface manager output: config diagnostics can contain registry credentials.
    let output = smol::future::race(async {
        command.output().await.map_err(|_| anyhow!("Could not start {action}; check the package manager and its runtime"))
    }, async {
        smol::Timer::after(timeout).await;
        bail!("Timed out trying to {action}; inspect the package manager before retrying")
    }).await?;
    ensure!(output.status.success(), "Could not {action} ({}); inspect the package manager's configuration and permissions", output.status);
    String::from_utf8(output.stdout).map_err(|_| anyhow!("Invalid text returned while trying to {action}"))
}

fn digest(path: &Path) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().to_vec())
}

fn canonical(path: &Path) -> Result<PathBuf> {
    path.canonicalize().context("Could not resolve an installation path")
}

fn manager_runner(manager: Manager, directory: &Path) -> Result<Option<Runner>> {
    let Some(program) = find_program(Path::new(manager.name())) else { return Ok(None); };
    let program = canonical(&program)?;
    let parent = program.parent().ok_or_else(|| anyhow!("Package manager has no directory"))?;
    let script = match manager {
        Manager::Npm => {
            if program.file_name().is_some_and(|name| name == "npm-cli.js") { Some(program.clone()) }
            else { Some(parent.join("node_modules/npm/bin/npm-cli.js")) }
        }
        Manager::Pnpm if is_shell_shim(&program) || program.extension().is_none()
            || program.extension().is_some_and(|extension| extension == "cjs" || extension == "js") => {
            if program.extension().is_some_and(|extension| extension == "cjs" || extension == "js") { Some(program.clone()) }
            else {
                let cli = parent.join("node_modules/pnpm/bin/pnpm.cjs");
                let corepack = parent.join("node_modules/corepack/dist/pnpm.js");
                if cli.is_file() { Some(cli) }
                else if corepack.is_file() && shim_references(&program, &canonical(&corepack)?)? { Some(corepack) }
                else { bail!("pnpm has no verified JavaScript CLI entry; reinstall pnpm or uninstall manually"); }
            }
        }
        Manager::Pnpm | Manager::Bun => None,
    };
    if let Some(script) = script {
        ensure!(script.is_file(), "{} needs its JavaScript CLI entry; reinstall the package manager or uninstall manually", manager.name());
        let node = find_program(&parent.join("node")).or_else(|| find_program(Path::new("node")))
            .filter(|path| !is_shell_shim(path))
            .ok_or_else(|| anyhow!("Node.js is required to invoke {} without a command shell", manager.name()))?;
        Ok(Some(Runner { program: canonical(&node)?, arguments: vec![canonical(&script)?.into_os_string()], directory: directory.to_path_buf() }))
    } else {
        ensure!(!is_shell_shim(&program), "{} is a command-shell shim; use its native runtime to uninstall manually", manager.name());
        Ok(Some(Runner { program, arguments: Vec::new(), directory: directory.to_path_buf() }))
    }
}

async fn query(runner: &Runner, arguments: &[&str]) -> Result<String> {
    run(runner.command().args(arguments), "query global package ownership", Duration::from_secs(15)).await
}

fn output_path(output: &str) -> Result<PathBuf> {
    let lines: Vec<_> = output.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    ensure!(lines.len() == 1, "Package manager returned an ambiguous global directory");
    let path = PathBuf::from(lines[0]);
    ensure!(path.is_absolute(), "Package manager did not return an absolute global directory");
    canonical(&path)
}

fn bun_root(output: &str) -> Result<PathBuf> {
    let first = output.lines().find(|line| !line.trim().is_empty())
        .ok_or_else(|| anyhow!("Bun returned no global directory"))?;
    let (directory, count) = first.trim().rsplit_once(" node_modules (")
        .ok_or_else(|| anyhow!("Bun returned an unrecognized global directory; inspect bun pm ls -g"))?;
    ensure!(count.ends_with(')') && count.trim_end_matches(')').parse::<usize>().is_ok(), "Invalid Bun global package count");
    let directory = PathBuf::from(directory);
    ensure!(directory.is_absolute(), "Bun global directory is not absolute");
    canonical(&directory.join("node_modules"))
}

fn npm_prefix(root: &Path) -> Result<PathBuf> {
    ensure!(root.file_name().is_some_and(|name| name == "node_modules"), "Unexpected npm global root");
    let parent = root.parent().ok_or_else(|| anyhow!("npm global root has no prefix"))?;
    Ok(if parent.file_name().is_some_and(|name| name == "lib") {
        parent.parent().ok_or_else(|| anyhow!("npm global root has no prefix"))?.to_path_buf()
    } else { parent.to_path_buf() })
}

fn shim_references(executable: &Path, entry: &Path) -> Result<bool> {
    let contents = std::fs::read_to_string(executable)?;
    let directory = executable.parent().ok_or_else(|| anyhow!("Executable has no directory"))?;
    for token in contents.split(['"', '\'', '\r', '\n']) {
        for marker in ["%dp0%", "%~dp0", "%basedir%", "$basedir", "${basedir}"] {
            if let Some(relative) = token.strip_prefix(marker) {
                let relative = relative.trim_start_matches(['/', '\\']);
                let path = directory.join(relative.replace('\\', "/"));
                if path.canonicalize().is_ok_and(|path| path == entry) { return Ok(true); }
            }
        }
    }
    Ok(false)
}

fn bun_references(executable: &Path, entry: &Path) -> Result<bool> {
    let bytes = std::fs::read(executable.with_extension("bunx"))?;
    let units: Vec<_> = bytes.chunks_exact(2).map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .take_while(|unit| *unit != 0).collect();
    let relative = String::from_utf16(&units).context("Invalid Bun executable companion")?;
    let relative = relative.trim_matches('"').replace('\\', "/");
    let directory = executable.parent().ok_or_else(|| anyhow!("Executable has no directory"))?;
    Ok([Some(directory), directory.parent()].into_iter().flatten()
        .any(|base| base.join(&relative).canonicalize().is_ok_and(|path| path == entry)))
}

fn prove(target: BackendTarget, executable: &Path, manager: Manager, runner: Runner,
    root: PathBuf, bin: PathBuf, prefix: PathBuf) -> Result<Option<UninstallPlan>> {
    let manifest = root.join(package(target).0).join("package.json");
    if !manifest.is_file() { return Ok(None); }
    let value: Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
    ensure!(value["name"].as_str() == Some(package(target).0), "Global package manifest has an unexpected name");
    let entry = dsh_install::entry_from_manifest(&manifest, target)?;
    let resolved = canonical(executable)?;
    let in_bin = executable.parent().is_some_and(|parent| parent.canonicalize().is_ok_and(|parent| parent == bin));
    let companion = executable.with_extension("bunx");
    let has_companion = companion.is_file();
    let matches = match manager {
        Manager::Bun if has_companion => in_bin && bun_references(executable, &entry)?,
        Manager::Npm | Manager::Pnpm if has_companion => false,
        Manager::Npm | Manager::Pnpm | Manager::Bun => resolved == entry
            || (in_bin && (is_shell_shim(executable) || executable.extension().is_none())
                && shim_references(executable, &entry)?),
    };
    if !matches { return Ok(None); }
    Ok(Some(UninstallPlan { target, executable: executable.to_path_buf(), manager, runner,
        root, bin, prefix, entry_digest: digest(&entry)?, entry, manifest_digest: digest(&manifest)?,
        executable_digest: digest(executable)?,
        companion_digest: if has_companion { Some(digest(&companion)?) } else { None } }))
}

fn choose(mut plans: Vec<UninstallPlan>, diagnostics: &[String]) -> Result<UninstallPlan> {
    match plans.len() {
        1 => Ok(plans.remove(0)),
        0 => bail!("Cannot safely identify the global package manager for this engine. Use the original npm, Bun, or pnpm installation to uninstall it manually. {}", diagnostics.join("; ")),
        _ => bail!("Multiple package managers claim this installation; resolve the conflicting global directories and refresh before uninstalling"),
    }
}

/// Finds the original global package manager using its reported directories and package bin.
///
/// # Errors
/// Rejects unknown, local, ambiguous, or unverified installations without removing files.
pub(crate) async fn detect(target: BackendTarget, executable: PathBuf) -> Result<UninstallPlan> {
    ensure!(executable.is_absolute() && executable.is_file(), "Engine executable is missing; refresh engine discovery");
    let directory = std::env::current_dir().context("Could not resolve the package-manager working directory")?;
    let mut plans = Vec::new();
    let mut diagnostics = Vec::new();
    for manager in [Manager::Npm, Manager::Bun, Manager::Pnpm] {
        let result: Result<Option<UninstallPlan>> = async {
            let Some(runner) = manager_runner(manager, &directory)? else { return Ok(None); };
            let (root, bin, prefix) = match manager {
                Manager::Npm => {
                    let root = output_path(&query(&runner, &["root", "-g"]).await?)?;
                    let prefix = npm_prefix(&root)?;
                    let bin = if cfg!(windows) { prefix.clone() } else { prefix.join("bin") };
                    (root, canonical(&bin)?, prefix)
                }
                Manager::Bun => {
                    let root = bun_root(&query(&runner, &["pm", "ls", "-g"]).await?)?;
                    let bin = output_path(&query(&runner, &["pm", "bin", "-g"]).await?)?;
                    let prefix = root.parent().ok_or_else(|| anyhow!("Bun global root has no parent"))?.to_path_buf();
                    (root, bin, prefix)
                }
                Manager::Pnpm => {
                    let root = output_path(&query(&runner, &["root", "-g"]).await?)?;
                    let bin = output_path(&query(&runner, &["bin", "-g"]).await?)?;
                    let directory = root.parent().ok_or_else(|| anyhow!("pnpm global root has no parent"))?;
                    let prefix = if directory.file_name().is_some_and(|name| name.to_string_lossy().parse::<u64>().is_ok()) {
                        directory.parent().ok_or_else(|| anyhow!("pnpm global directory has no parent"))?.to_path_buf()
                    } else { directory.to_path_buf() };
                    (root, bin, prefix)
                }
            };
            prove(target, &executable, manager, runner, root, bin, prefix)
        }.await;
        match result {
            Ok(Some(plan)) => plans.push(plan),
            Ok(None) => {},
            Err(error) => diagnostics.push(format!("{}: {error}", manager.name())),
        }
    }
    choose(plans, &diagnostics)
}

#[cfg(test)]
#[path = "engine_uninstall_tests.rs"]
mod tests;
