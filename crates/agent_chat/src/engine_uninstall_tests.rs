use super::*;

struct Installation {
    directory: tempfile::TempDir,
    root: PathBuf,
    bin: PathBuf,
    executable: PathBuf,
    entry: PathBuf,
    manifest: PathBuf,
}

impl Installation {
    fn new(target: BackendTarget) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("node_modules");
        let bin = directory.path().join("bin");
        let package_directory = root.join(package(target).0);
        std::fs::create_dir_all(package_directory.join("dist")).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let entry = package_directory.join("dist/cli.js");
        std::fs::write(&entry, "console.log('fixture');\n").unwrap();
        let manifest = package_directory.join("package.json");
        std::fs::write(&manifest, serde_json::json!({
            "name": package(target).0, "version": "1.0.0",
            "bin": { (dsh_install::name(target)): "dist/cli.js" }
        }).to_string()).unwrap();
        let executable = bin.join(format!("{}.cmd", dsh_install::name(target)));
        std::fs::write(&executable, format!("@node \"%~dp0/../node_modules/{}/dist/cli.js\" %*\n", package(target).0)).unwrap();
        Self { directory, root, bin, executable, entry, manifest }
    }

    fn plan(&self, target: BackendTarget, manager: Manager) -> Result<Option<UninstallPlan>> {
        let runner = Runner { program: self.directory.path().join("manager-not-executable"),
            arguments: Vec::new(), directory: self.directory.path().to_path_buf() };
        prove(target, &self.executable, manager, runner, canonical(&self.root)?,
            canonical(&self.bin)?, canonical(self.directory.path())?)
    }
}

#[test]
fn fixed_scoped_packages_and_global_shims_are_proven() {
    for target in BackendTarget::ALL {
        let installation = Installation::new(target);
        for manager in [Manager::Npm, Manager::Pnpm] {
            let plan = installation.plan(target, manager).unwrap().unwrap();
            assert_eq!(plan.entry, canonical(&installation.entry).unwrap());
            assert!(plan.description().contains(package(target).0));
            assert!(plan.description().contains("--ignore-scripts"));
            assert!(plan.revalidate_files().is_ok());
        }
    }
}

#[test]
fn bun_companion_proves_home_global_layout_without_package_manager_marker() {
    let mut installation = Installation::new(BackendTarget::Omp);
    let bin = installation.directory.path().join(".bun/bin");
    std::fs::create_dir_all(&bin).unwrap();
    installation.bin = bin;
    installation.executable = installation.bin.join("omp.exe");
    std::fs::write(&installation.executable, b"isolated executable fixture").unwrap();
    let relative = "..\\node_modules\\@oh-my-pi\\pi-coding-agent\\dist\\cli.js\"\0bun\0";
    let bytes: Vec<_> = relative.encode_utf16().flat_map(u16::to_le_bytes).collect();
    std::fs::write(installation.executable.with_extension("bunx"), bytes).unwrap();
    std::fs::write(installation.directory.path().join("package.json"),
        r#"{"packageManager":"pnpm@11.20.0"}"#).unwrap();
    let plan = installation.plan(BackendTarget::Omp, Manager::Bun).unwrap().unwrap();
    assert_eq!(plan.entry, canonical(&installation.entry).unwrap());
    assert!(installation.plan(BackendTarget::Omp, Manager::Npm).unwrap().is_none());
    assert_eq!(bun_root(&format!("{} node_modules (281)\n", installation.directory.path().display())).unwrap(), canonical(&installation.root).unwrap());
    std::fs::write(installation.executable.with_extension("bunx"), b"unrelated").unwrap();
    assert!(plan.revalidate_files().is_err());
}

#[test]
fn unknown_executable_and_wrong_package_are_not_owned() {
    let installation = Installation::new(BackendTarget::Dsh);
    std::fs::write(&installation.executable, "@echo arbitrary executable\n").unwrap();
    assert!(installation.plan(BackendTarget::Dsh, Manager::Npm).unwrap().is_none());
    assert!(choose(Vec::new(), &[]).unwrap_err().to_string().contains("original npm, Bun, or pnpm"));
    std::fs::write(&installation.manifest,
        r#"{"name":"unrelated","bin":{"dsh":"dist/cli.js"}}"#).unwrap();
    assert!(installation.plan(BackendTarget::Dsh, Manager::Npm).is_err());
}

#[test]
fn changed_entry_rejects_execute_before_any_manager_process() {
    let installation = Installation::new(BackendTarget::Pi);
    let plan = installation.plan(BackendTarget::Pi, Manager::Npm).unwrap().unwrap();
    std::fs::write(&installation.entry, "installation replaced after confirmation\n").unwrap();
    let error = smol::block_on(plan.execute()).unwrap_err();
    assert!(error.to_string().contains("Installation changed"));
    assert!(installation.manifest.is_file());
    assert!(installation.executable.is_file());
}

#[test]
fn changed_manifest_and_shim_reject_confirmation() {
    let installation = Installation::new(BackendTarget::Dsh);
    let plan = installation.plan(BackendTarget::Dsh, Manager::Npm).unwrap().unwrap();
    let manifest = std::fs::read(&installation.manifest).unwrap();
    std::fs::write(&installation.manifest, serde_json::json!({
        "name": package(BackendTarget::Dsh).0, "version": "2.0.0", "bin": {"dsh": "dist/cli.js"}
    }).to_string()).unwrap();
    assert!(plan.revalidate_files().is_err());
    std::fs::write(&installation.manifest, manifest).unwrap();
    std::fs::write(&installation.executable, "@echo replaced\n").unwrap();
    assert!(plan.revalidate_files().is_err());
}

#[test]
fn overlapping_manager_claims_are_rejected() {
    let installation = Installation::new(BackendTarget::Pi);
    let npm = installation.plan(BackendTarget::Pi, Manager::Npm).unwrap().unwrap();
    let pnpm = installation.plan(BackendTarget::Pi, Manager::Pnpm).unwrap().unwrap();
    let error = choose(vec![npm, pnpm], &[]).unwrap_err();
    assert!(error.to_string().contains("Multiple package managers"));
}

#[test]
fn manifest_cannot_escape_fixed_package_directory() {
    let installation = Installation::new(BackendTarget::Pi);
    std::fs::write(&installation.manifest, serde_json::json!({
        "name": package(BackendTarget::Pi).0, "bin": {"pi": "../outside.js"}
    }).to_string()).unwrap();
    assert!(installation.plan(BackendTarget::Pi, Manager::Npm).is_err());
}
