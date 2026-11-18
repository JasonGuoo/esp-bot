# 小呆大脑接入与验收（2026-11-18）

本次把 `~/grok-bot/xiaodai-fast-responder` 的快速应答核心作为源码放进 `server/xiaodai-core`，并设为 Mac bot-server 日常对话的默认大脑。只复制了 `src/`、`tests/`、`Cargo.toml`、`Cargo.lock`、`README.md` 和 `.env.example`，没有复制 `runtime/`、`target/`、`.env` 或任何密钥，也没有使用 `~/GitHub/codex-dialed` 的内容。所有工作都在用户 Mac 的本地检出上完成，没有用 GitHub CI，也没有推送。

## 1. 分工

| 情况 | 由谁回答 |
| --- | --- |
| 普通口语对话：聊天、讲故事、解释、出主意、回忆家里说过的事 | **小呆大脑**（`xiaodai-core`）：先用 Codex `gpt-6-luna`，2 秒后 GLM 和 DeepSeek 一起竞速（dual 模式） |
| 关键词明显需要工具：上网、查天气或新闻、画图、展示图片、打印、准备资料、提醒、播放等 | OpenCode（原有流程） |
| 已选私密档案的回合、正在等待用户补充信息的任务 | OpenCode |
| 核心自己判断需要工具，只回复 `〔转交〕` | 转给 OpenCode；这次核心的输出不会播出 |
| 核心出错或超过 `turn_timeout_ms`（默认 60 秒） | 转给 OpenCode |
| 后台 `agent.work` 任务、`brain_core.enabled=false` | OpenCode |

这样分工，是因为核心只产出流式句子，没有工具、任务、回执和图文呈现。OpenCode 仍负责这些能力，链路保持原样。

## 2. 接入方式

- **配置**：`brain.json` 新增可选的 `brain_core`，默认值为 `enabled=true`、`mode="dual"`、`escalate_to_opencode=true`、`memory_context_chars=6000`、`turn_timeout_ms=60000`、`grok_bot_delegation=false`，可选 `codex_binary` 和 `codex_model`。没有这一项的旧配置照常加载，并默认打开核心。HTTP 路线沿用 `harness` 中已启用的 GLM、DeepSeek、Qwen 设置（模型、端点、密钥变量名），`provider_network` 也一起沿用。
- **密钥**：只取 App 本次会话保存的密钥，没有时取启动环境中的同名变量。保存或删除密钥后立即刷新核心。没有读取、迁移或删除旧的钥匙串项目，也没有打印任何密钥。Codex 使用本机现有的 Codex 登录，不改 `~/.codex/config.toml`。
- **记忆**：核心和 OpenCode 拿到相同的上下文：旧档案记忆、家庭共享记忆（含 Graphiti 结果）和当天的近期记录，作为只供参考的 system 消息，总长不超过 6000 字。回合仍由原有 `journal_input` / `journal_finish` 记进 SQLite，后台整理也没有改。Luna 的会话线程在家庭日期变化、`memory_changed` 或 `identity_revoked` 时重开，避免已删除的内容留在模型线程里。Luna 的可写目录是 `<runtime>/xiaodai-core/workspace` 下的空目录。
- **身份**：声纹和人脸识别仍走独立的 `launch_identity`，与核心无关。
- **流式与 TTS**：核心每产出一句，就冻结为一个呈现分段，并立刻开始这一句的 TTS，同时继续生成后面的句子。`prepare_presentation` 会复用已经合成的音频。设备仍在整轮结束后才开始播放，这次没有改成逐句推送到设备。
- **停止与打断**：Brain 取消回合时会丢弃核心的 Turn，从而发送 Codex `turn/interrupt`。服务停止时关闭 Codex 子进程，重新启动时再打开。
- **Grok Bot 代办**默认关闭，因为它的结果会绕过任务、呈现和回执链路。
- **控制台**：「模型与密钥」页新增「小呆大脑」小框，显示模式、Luna 状态、各引擎中位首字时间、最近一次胜出路线和 latency 日志路径。回合耗时多了 `brain_core` 阶段、首字和首句时间，以及跳过或转交的原因。

延迟日志写在 `<runtime>/xiaodai-core/latency.jsonl`。

## 3. 测过的内容

### 3.1 构建与单元测试

| 项目 | 结果 |
| --- | --- |
| `server/xiaodai-core`：`cargo test` | 65 项通过（26 单元、27 fake、12 mock），1 项真实网络测试按设计忽略 |
| `server/xiaodai-core`：`cargo clippy --all-targets` | 无警告 |
| `server/bot-server/src-tauri`：`cargo test` | 86 项通过，2 项忽略（含新增 `fast_core` 5 项） |
| `server/bot-server/src-tauri`：`cargo clippy --all-targets` | 新代码没有新增警告。`prepare_presentation` 原有的 too_many_arguments 警告仍在，参数从 9 个变为 10 个 |
| `server/bot-server`：`npm run build`（tsc + Vite） | 通过 |

### 3.2 本机端到端（隔离环境）

测试用临时的 ignored 测试 `core_default_brain_live` 驱动真实 Brain，跑完后从源码中删除。测试目录是 `~/小呆 Server/runtime/tmp/xiaodai-core-test/`，配置复制自正式 `brain.json`，但关闭设备网关和自动启动，使用独立的 OpenCode 端口 4197 和独立的 SQLite，打开历史记录，时区设为 Australia/Perth。语音由 macOS `say`（Tingting）合成为 16 kHz WAV，经过真实的 Qwen3-ASR、pyannote 和 ECAPA 声纹路径。模型 Key 只放在测试进程的环境里。

先建立测试档案「测试小林」，登记 3 段声纹，然后依次跑了下面几轮。第二次运行结果如下，时间为 2026-11-18 15:40 前后（AWST）：

| 回合 | 输入 | 结果 |
| --- | --- | --- |
| 预热 | — | 6.8 秒后 Codex 线程就绪；GLM、DeepSeek 预连通过 |
| t1 语音 | ASR 文本：「你好，小呆。我叫小林。我最喜欢的水果是芒果。」 | 由 `xiaodai-core`（`codex/gpt-6-luna`）回答：「你好，小林！ 记住啦，你最喜欢芒果。 芒果甜甜的，闻起来也香香的。」首字 791 ms，首句 950 ms，核心总计 1355 ms。ASR 1365 ms。`brain_core` 阶段 4512 ms（含 3 句 TTS 预合成），整轮 5924 ms。声纹匹配到测试小林，得分 0.870 |
| t2 语音，新线程 | 先 `forget_thread`，确保答案来自 SQLite 当天记录。ASR 文本：「小呆，你还记得我最喜欢的水果是什么吗？」 | 回答：「记得呀，你最喜欢的水果是芒果。 甜甜香香的，难怪你喜欢！」首字 1014 ms，首句 1288 ms，ASR 137 ms，整轮 3294 ms。声纹匹配到测试小林，得分 0.858 |
| t3 文字 | 「帮我上网搜索一下今天珀斯的天气」 | 预先判断为 `tool_request`，交给 OpenCode。第一次运行成功（GLM 联网后回答，17.5 秒）。第二次运行时，OpenCode 的回复被原有呈现校验以 `chart_data_unverified` 拒绝，这一轮记为 failed。这属于 OpenCode 这条路的原有行为，与核心无关 |
| t4 停止 | 「给我讲一个关于小猫的长故事，至少十句话」，收到首字后立即取消 | 核心 768 ms 出首字。取消后 0.025 ms 发出 Codex interrupt，6.4 ms 收到 `interrupted`，之后没有再出句子。回合在 SQLite 中记为 cancelled |
| t5 停止后 | 「你好呀」 | Luna 首字 2039 ms，GLM 和 DeepSeek 在 2 秒加入竞速但落后，最终 Luna 回答 |

**SQLite（隔离库）**：

- `household_turns` 中 t1、t2、t5 为 completed，输入和回复全文都在；t3 为 failed，t4 为 cancelled。
- `household_segments` 中 t1、t2 的 `automatic_person_id` 都是测试小林的档案。
- 后台整理任务完成了 5 项 organize。`household_derived` 新增两条：fact「小林最喜欢的水果是芒果」和 topic「小林喜欢芒果」。

**第一次运行**中，t4 被 Luna 自己转交给了 OpenCode（转交前花了 2.5 秒），说明转交提示太宽。之后改了提示，讲故事、聊天、解释由核心自己回答，并把 HTTP 的 `max_tokens` 调到 600。第二次运行正常，就是上表的结果。

测试启动的 OpenCode（4197）、Codex app-server 和 Python worker 都已退出。正式 App（pid 32470）、正式 OpenCode（4097）、ChatGPT 自带的 codex 和正式 `brain.json` 都没有动。

## 4. 没有测或仍有限制的内容

- **实板回合没有做**。S31 已通过 USB 连接，但正式 App 不是这次启动的。替换并重启它会丢掉 App 本次会话中保存的模型 Key，而且这些 Key 不在启动环境里，所以没有安装新 App，也没有让设备跑新大脑。设备音量、配对和固件都没有改，也没有刷写。
- 正式 App 尚未更新。正式 `brain.json` 没有 `brain_core` 项，安装新版 App 后核心会默认打开；需要旧行为时，可设 `"brain_core": {"enabled": false}`。
- 没有单独测试只靠后台整理结果（`household_derived` 或 Graphiti）而不靠当天近期记录的召回，也没有测试跨日召回。Graphiti 在测试环境中没有配置。
- 设备仍在整轮结束后才开始播放。`brain_core` 阶段里会等 TTS 预合成完成，约占 1.5 到 3 秒。
- 私密档案回合和多人重叠说话仍走 OpenCode，这次没有实测。
- HTTP 竞速路线在本次各轮中都没有胜出过，所以通过 HTTP 胜出路线播出回答这条路没有在 bot-server 中实测。核心自己的 mock 和 fake 测试覆盖了这条路。
