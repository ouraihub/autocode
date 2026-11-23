<p align="center"><a href="README.md">English</a> | 中文</p>

<div align="center">
  <img src="crates/autocode/resources/app-icon.png" alt="autocode" width="128">
  <h1>autocode</h1>
  <p>快速的原生代码编辑器和智能体工作区。</p>
  <p>用 Rust 编写，开箱即用。</p>
  <p><a href="#构建"><strong>本地构建</strong></a></p>
</div>

---

autocode 是一个独立的原生代码编辑器和编程智能体工作区，使用 Rust 编写，基于开源 [Zed](https://github.com/zed-industries/zed) 编辑器的派生代码继续开发。

## 功能

- ⚡ **原生、快速。** 不用 Electron，也没有网页视图。使用 GPU 渲染。
- 🍃 **原生资源使用。** [历史上游测试记录](docs/memory-benchmark.md) 仅供背景参考，不代表本项目的实测结果。
- 👾 **内置编程智能体。** 直接支持 Claude Code、Codex 和 OpenCode。
- ✨ **默认就好看。** 外观简洁现代，用起来也顺手。
- 🤫 **无需账号。** 不内置使用遥测；配置的编程智能体和模型服务会自行发起网络请求。

## 构建

在仓库根目录构建 Windows x64 便携应用：

```bat
scripts\msvc\setup-msvc.bat
scripts\msvc\package-exe.bat
```

打包脚本默认以 `release-fast` 配置构建 `autocode`、`cli` 和 `auto_update_helper` Cargo 包，将所需运行文件收集到 `dist/autocode/`，并生成 `dist/autocode-x64.zip`。实际启动目标是 **`dist/autocode/autocode.exe`**；这是便携应用，不是安装程序。使用 `scripts\msvc\package-exe.bat -Profile release` 可选择 release 配置。

隔离本地运行数据的示例：

```bat
dist\autocode\autocode.exe --user-data-dir "%TEMP%\autocode-local"
```

详见[安装与构建说明](docs/INSTALL.md)和 [MSVC 工具链说明](scripts/msvc/README.md)。本仓库未指定公开的 autocode 下载或发布地址；历史上游二进制不是 autocode 构建。

## 文档

- [安装与本地构建](docs/INSTALL.md)
- [Agent 后端扩展规划](docs/agent-backend-extension-design.md)
- [概要设计](docs/02-概要设计.md)、[详细设计](docs/03-详细设计.md)和[实施任务状态](docs/04-实施任务列表.md)

## 许可证

autocode 采用 GPL-3.0-or-later 许可证，标注的部分采用 Apache-2.0。请看 [LICENSE-GPL](./LICENSE-GPL) 和 [LICENSE-APACHE](./LICENSE-APACHE)。

## 致谢

感谢 Zed 团队及历史上游贡献者开源编辑器基础，使本独立项目能够继续开发。
