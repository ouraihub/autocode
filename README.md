<p align="center">English | <a href="README.zh-CN.md">中文</a></p>

<div align="center">
  <img src="crates/autocode/resources/app-icon.png" alt="autocode" width="128">
  <h1>autocode</h1>
  <p>The fast, native code editor and agent workspace.</p>
  <p>Written in Rust. Batteries included.</p>
  <p><a href="#building"><strong>Build locally</strong></a></p>
</div>

---

autocode is an independent, native code editor and coding-agent workspace written in Rust, derived from the open-source [Zed](https://github.com/zed-industries/zed) editor lineage.

## Features

- ⚡ **Native and fast.** No Electron or webviews. Renders on the GPU.
- 🍃 **Native resource usage.** See the [archived upstream benchmarks](docs/memory-benchmark.md) for historical context, not measurements of this project.
- 👾 **Coding agents included.** Built-in support for Claude Code, Codex, and OpenCode.
- ✨ **Beautiful by default.** Clean, modern look with a delightful UX.
- 🤫 **No account required.** No built-in usage telemetry; configured coding agents and model services make their own network requests.

## Building

Build the Windows x64 portable application from the repository root:

```bat
scripts\msvc\setup-msvc.bat
scripts\msvc\package-exe.bat
```

The packaging script builds the `autocode`, `cli`, and `auto_update_helper` Cargo packages with the `release-fast` profile by default. It collects the required runtime files into `dist/autocode/` and creates `dist/autocode-x64.zip`. Launch **`dist/autocode/autocode.exe`**; this is a portable application, not an installer. Use `scripts\msvc\package-exe.bat -Profile release` for the release profile.

For an isolated local run:

```bat
dist\autocode\autocode.exe --user-data-dir "%TEMP%\autocode-local"
```

See [installation and build notes](docs/INSTALL.md) and the [MSVC toolchain guide](scripts/msvc/README.md). This repository does not designate a public autocode download or release endpoint; upstream binaries are not autocode builds.

## Docs

- [Installation and local builds](docs/INSTALL.md)
- [Agent backend extension design](docs/agent-backend-extension-design.md)
- [Architecture](docs/02-概要设计.md), [detailed design](docs/03-详细设计.md), and [implementation task status](docs/04-实施任务列表.md)

## Licensing

autocode is licensed under GPL-3.0-or-later with Apache-2.0 components where marked. See [LICENSE-GPL](./LICENSE-GPL) and [LICENSE-APACHE](./LICENSE-APACHE).

## Acknowledgements

Thanks to the Zed team and historical upstream contributors for building and sharing the editor foundations on which this independent project is based.
