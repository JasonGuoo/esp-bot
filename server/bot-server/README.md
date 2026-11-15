# 小呆 Bot Server — Mac 控制台

当前验证状态见 [2026-11-15 本机与 S31 验收](../../docs/review-acceptance-2026-11-15.md)，接续 [Review 源码修正](../../docs/review-fixes-2026-11-14.md)。已执行本机测试、签名 release 打包及真实语音链路检查；未运行 CI。Mac 直接录音的系统授权限制另列。普通设置保存不再自动停止服务；显式重载后手动启动生效。

Rust + Tauri 2 桌面应用，当前唯一对话入口为“小呆”。
界面提供固定的八个功能入口：对话、家庭成员、记忆库、模型与密钥、本地能力、工具与授权、设备连接、运行设置。
功能清单、交互结构和验收范围见 [控制台设计说明](../../docs/mac-console-design.md)。

## 构建与启动

本机要求：Apple Silicon Mac、Xcode、Rust stable、Node.js 22。

```sh
cd server/bot-server
npm ci
npm run tauri build
open "src-tauri/target/release/bundle/macos/ESP Bot Server.app"
```

构建先执行 TypeScript 检查与 Vite production build，再编译 Rust 和打包 App。
App 内嵌前端、Python worker 源码和工具示例，直接打开不依赖 Vite。
原生 macOS 标题栏负责拖动、缩放和窗口按钮。

开发时使用 `npm run tauri dev`，前端固定为 `127.0.0.1:1421`，避免与本机另一应用占用的 1420 冲突。直接启动 debug 二进制仍需要 Vite。
依赖缓存完整时，可用 `npm run tauri build -- -- --offline` 完成离线构建。
本项目不使用 GitHub CI / Actions 验证。

这台 Mac 经用户授权使用现有 Apple Development 证书签名本地 App。
`npm run tauri build` 会读取 Git 忽略的 `runtime/macos-signing-identity` 中的公开证书指纹；
证书私钥始终留在钥匙串。显式设置的 `APPLE_SIGNING_IDENTITY` 优先。
本地身份文件不存在时维持 Tauri 默认签名行为；清理 `runtime/` 后需重新选择签名身份，
否则重新打包可能使 macOS 再次要求钥匙串授权。开发签名用于本机开发，不等于公证发布。

## 在控制台配置 API Key

1. 打开 **模型与密钥 → API Key**。
2. 选择 GLM Coding Plan 或 DeepSeek Flash，粘贴对应 Key，点击 **保存密钥**。Qwen Coding Plan 保留为默认停用的可选路线。
3. 保存后密钥输入框清空，只显示“已配置”；点击 **启动服务** 生效。

密钥保存在本机 macOS 登录钥匙串，不写入 `brain.json`、日志或浏览器存储。
保存或移除密钥会先停止服务，避免旧进程继续使用旧密钥。
已保存的钥匙串密钥优先于启动环境变量；移除后已有环境变量仍可生效。
此入口用于 managed 模式的显式 provider。attach 模式及继承的 provider 由外部 OpenCode 配置凭据。

`brain.json` 位于 `~/小呆 Server/brain.json`。
模型、端点与优先级在 **模型与密钥** 编辑；本机模型环境在 **本地能力**；启动偏好和完整 JSON 在 **运行设置**。
**2026-11-13 默认顺序：GLM → DeepSeek Flash**；Qwen 默认停用，仍可在控制台重新启用。
旧默认配置的升级条件见 [模型顺序迁移](../../docs/mac-opencode.md#旧配置的模型顺序升级)。更新并重新打开 App 后生效；单独拉取源码不会改变已运行的 App。本次仅修改源码和文档，没有运行测试、构建、模型请求或 CI。配置密钥不代表已确认账户额度或模型可用性。

OpenCode 使用官方 v2.2.5 或兼容的后续 v2。此次本机安装位于被 Git 忽略的
`runtime/bin/opencode`，不是 App 内置安装器。其他 Mac 需要自行安装并配置可执行路径。
本地 ASR、身份模型需要单独按文档准备；打开窗口不会下载模型或启动采集。

## 实现与边界

- `src/companion.ts`：当前中文控制台与单一 `companion_request` IPC。
- `src-tauri/src/brain`：主动回合、本地模型、设备 WSS v3 与控制台 API。
- `src-tauri/src/harness`：官方 OpenCode 管理、路线、权限、取消、会话和钥匙串。
- `src-tauri/src/memory`：SQLite 档案、记忆和身份登记。
- `local-models`：本地 ASR、身份与系统 TTS worker。

已删除旧 Chat、Provider、Events、Mock 预览和相关原型适配代码；不再读取旧 `config.json`。
Mac 暂保留默认构建特性 `lan-diagnostics`，旧诊断服务只显示状态，不传私密媒体。新固件默认不启动 LAN v1，开发时可用 `CONFIG_ESPBOT_LAN_DIAGNOSTICS` 显式开启；配对 Mac 发现及设备会话走已有认证 WSS v3。BLE 仍待集成，实板结果与本轮尚未验证的源码分开记录，见上方状态入口。

详细设置与实际验收范围见 [Mac 控制台](../../docs/mac-console.md)、
[Mac 大脑](../../docs/mac-bot-server.md)、[OpenCode](../../docs/mac-opencode.md)、
[本地模型](../../docs/mac-local-models.md) 和 [WSS 网关](../../docs/mac-gateway.md)。
