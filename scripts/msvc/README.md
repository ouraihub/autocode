# Portable MSVC scripts

Windows local-build helpers. Rust's `x86_64-pc-windows-msvc` target needs
Microsoft's linker and the Windows SDK import libraries. These scripts avoid a
full Visual Studio install by fetching the MSVC compiler and Windows SDK into a
self-contained folder and injecting it into the current `cmd.exe` session.

## Origin and licence

The scripts are vendored from
[`ouraihub/thinkrail-native`](https://github.com/ouraihub/thinkrail-native)
`scripts/msvc/` (MIT, see `LICENSE` in this directory). `autocode` itself is
GPL-3.0-or-later; MIT content is compatible under those terms.

Files in this directory and their purpose:

| File | Role |
|---|---|
| `setup-msvc.bat` | One-time download: `~455 MB` MSVC 14.44 + Windows SDK 26100 under `PORTABLE_MSVC_ROOT` (default `D:\Applications\msvc-portable\msvc`). |
| `msvc-env.bat` | Injects the portable toolchain and `VCINSTALLDIR` into the calling `cmd.exe`, including discovery by third-party `cc` build scripts. Runs automatically from `cargo-msvc.bat`. |
| `cargo-msvc.bat` | `cargo` wrapper that calls `msvc-env.bat` first so Rust picks the MSVC linker rather than Git's GNU `link.exe`. |
| `compiler-paths.ps1` | Asserts `cl.exe` / `link.exe` really come from the portable root (defence against PATH shadowing). |

`package-exe.bat` is adapted for this project: it builds and collects a local
portable test package rather than the ThinkRail binary or a Windows installer.

## First-time setup

1. Install [`uv`](https://docs.astral.sh/uv/) and make sure `curl` is on
   `PATH` (Windows 10 1803+ ships it).
2. From a `cmd.exe` prompt in the repository root:

   ```cmd
   scripts\msvc\setup-msvc.bat
   ```

   This downloads MSVC + Windows SDK and prints `Done.` when the linker is
   reachable. Re-run with `--force` to re-download.

3. Build with the wrapper:

   ```cmd
   scripts\msvc\cargo-msvc.bat check -p model_provider
   scripts\msvc\cargo-msvc.bat test  -p model_provider --lib
   scripts\msvc\cargo-msvc.bat clippy -p model_provider --all-targets -- -D warnings
   ```

   Any `cargo <subcommand> …` works after the wrapper call.

4. Prefer a different install location? Set `PORTABLE_MSVC_ROOT` before
   running `setup-msvc.bat` and keep it set (`setx PORTABLE_MSVC_ROOT …`)
   so later shells find the toolchain.

## Local portable test package

Run from any directory (or double-click the batch file):

```cmd
E:\workspace\autocode-dev\autocode\scripts\msvc\package-exe.bat
```

Output:

- `dist\autocode\autocode.exe`: the editor, built directly as the `autocode` Cargo binary.
- `dist\autocode-x64.zip`: the same folder, including `bin\autocode.exe` (CLI),
  the update helper and ConPTY runtime files. Extract the entire folder.

The default `release-fast` profile is optimized, embeds assets, and avoids
release LTO for local iteration. For the normal release profile:

```cmd
scripts\msvc\package-exe.bat -Profile release
```

Both modes use the portable MSVC wrapper, preserve the current release channel,
and build only the editor and required helper programs. Inno Setup and Appx
packaging tools are not required. The editor build may download ConPTY; missing
runtime files fail packaging instead of producing an incomplete archive.
The first package build also installs Microsoft's matching Spectre CRT into
the portable MSVC 14.44.35207 toolset (about 59 MB download, SHA-256 checked).
It is required by the Python environment discovery dependency. The official
component is cached beside the portable toolchain under `downloads`; no Visual
Studio installation or security-check bypass is needed.

For a separate test data directory:

```cmd
dist\autocode\autocode.exe --user-data-dir "%TEMP%\autocode-local-test"
```

This is an independent autocode build with its own application identity and
settings/data directories. No upstream product updater is used. The package is
for local testing, not a signed installer or a public release distribution.


## Why not Visual Studio Build Tools

They work, but the install is large and not easy to reproduce on CI runners
that are not Microsoft-managed. The portable toolchain is
self-contained — `D:\Applications\msvc-portable\msvc\setup_x64.bat` uses
relative paths, so the folder can be copied to another machine without
re-downloading. CI steps that already run on Microsoft runners skip these
scripts via `GITHUB_ACTIONS=true` (see `msvc-env.bat`).
