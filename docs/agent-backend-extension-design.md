# Agent 后端扩展交付记录

## 已交付

- Harness 公共运行契约、代次隔离、启动取消、进程生命周期和有界 JSONL framing。
- Pi Native RPC、OMP protocol-v2 `rpc-ui`、DSH ACP stdio 适配。
- OMP chunk 重组、物理行/逻辑帧上限、EOF 中断检测。
- Pi/OMP/DSH 能力报告、Native session ID、模型与历史边界。
- 六后端聊天枚举、菜单动作、设置映射、模型发现和 commit-message 消费者。
- Provider URL 规范化、不可变路由快照、Direct/Managed 投影、Gateway key round-robin。
- Credential pending-put/pending-delete、回读校验、孤儿清理和启动 reconcile。
- 问题选项稳定 ID、过期/撤回清理、历史工具调用与结果去重。

## 验收命令

```text
scripts\\msvc\\cargo-msvc.bat test -p model_provider --lib
scripts\\msvc\\cargo-msvc.bat test -p agent_harness --lib
scripts\\msvc\\cargo-msvc.bat test -p agent_chat --lib
scripts\\msvc\\cargo-msvc.bat check -p settings_ui
```

最后一次验证结果：

- `model_provider`: 41 passed。
- `agent_harness`: 247 passed。
- `agent_chat`: 135 passed。
- `settings_ui`: cargo check passed。

## 真实 CLI 环境记录

- Pi 1.4.0：可启动 `pi --mode rpc`。
- OMP 18.22.2：可启动 `omp --mode rpc-ui --approval-mode always-ask`，返回 `ready`，声明 protocol v1/v2。
- DSH：当前验收环境未安装，`dsh --profile acp` 返回 `command not found`；代码路径保留显式 `NotInstalled` 错误，不伪造成功。

## 使用说明

1. Native 模式：直接使用对应 CLI 的本地模型和配置。
2. Managed 模式：在 Model Services 页面配置 Provider、endpoint、模型和凭据；明文密钥只进入凭据存储，不写入生成配置。
3. Provider 变更后，重新打开模型选择器或调用显式模型刷新；运行中的 chat 使用启动时的不可变路由快照。
4. Gateway 模式按请求轮换启用凭据；租约按 chat 隔离。
5. DSH 使用前需安装并确保 `dsh` 在 PATH，或通过启动配置提供可执行文件路径。

## 已知边界

- 真实模型请求依赖用户本地 CLI 登录状态、Provider 凭据和网络；本次验收只验证协议握手、fixture 链路、状态生命周期和安全边界。
- DSH 未安装，因此没有宣称 DSH 的真实 CLI 请求成功。
