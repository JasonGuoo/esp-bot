# Mac OpenCode harness

> 状态：源码与配置流程已写入，尚未编译、运行、调用模型或做硬件联调。遵照本轮要求，没有新增或执行测试，也没有运行 lint / CI。本文中的安装和启动命令是交付后的操作说明，本次没有执行。

本模块让 Mac 的 `bot-server` 管理官方 OpenCode 服务，把已获授权的一轮文字输入交给模型和 MCP 工具，再把结果交回宿主验证。声音转写、说话人 / 人脸匹配、家长确认的记忆、语音合成与设备连接属于相邻模块，见 [Mac 本地模型](mac-local-models.md)、[Mac 记忆](mac-memory.md) 和 [本地优先混合大脑](local-first-hybrid-brain.md)。机器人本体仍然负责唤醒、行为、安全与身体执行。

## 1. 基于哪个官方版本

截至 **2026-11-06**，本次核对的最新 OpenCode 应用版本是 **v2.2.5**，官方发布时间为 **2026-10-30**。代码以该 tag 的服务器协议、配置 schema 和实现源码为准。发布列表中另有 `process-daemon-*` 组件版本，不能把它当成 OpenCode 应用版本；GitHub 的 `releases/latest` 当时仍可能指向 1.x，不能据此选择这套原生 V2 API 的服务端。

- [官方 v2.2.5 release](https://github.com/anomalyco/opencode/releases/tag/v2.2.5)
- [官方发布列表](https://github.com/anomalyco/opencode/releases)
- [原生服务器协议](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/protocol/src/api.ts)
- [配置 schema](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/schema/src/config.ts)

启动时通过 `GET /api/info` 检查版本，接受稳定版 `2.2.5 <= version < 3`。这表示版本入口检查，不表示未来所有 2.x 已得到联调验证。当前参考版本固定保留在 `SOURCE_VERSION`，升级 OpenCode 后应重新检查 API；程序不会下载二进制或自行升级。

### 原生 V2 与常见旧示例的差异

| 用途 | 本实现使用的 V2 接口 / 字段 |
| --- | --- |
| 服务信息 | `GET /api/info` |
| 创建会话 | `POST /api/session` |
| 投递用户输入 | `POST /api/session/{id}/prompt` |
| 事件流 | `GET /api/event`，`{type, data, ...}` |
| 停止本轮 | `POST /api/session/{id}/interrupt?resume=false` |
| 模型引用 | `{ "providerID": "...", "id": "..." }` |
| 权限回复 | `POST /api/session/{id}/permission/{requestID}/reply` |
| 删除会话 | `DELETE /api/session/{id}`，官方实现同时删除子会话 |
| MCP 状态 | `GET /api/mcp?directory=...` |
| 配置 | `providers`、`agents`、`permissions`、`mcp.servers` |
| HTTP 认证 | 固定用户名 `opencode`，密码环境变量 `OPENCODE_PASSWORD` |

不要把 V1 的 `/global/health`、`/session/{id}/message`、`modelID` 或单数 `provider` 配置直接套入这里。协议依据见 [session API](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/protocol/src/groups/session.ts)、[event API](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/protocol/src/groups/event.ts)、[permission API](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/protocol/src/groups/permission.ts) 和 [认证实现](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/server/src/auth.ts)。

## 2. 已写入的职责

实现位于 `server/bot-server/src-tauri/src/harness/`：

| 文件 | 职责 |
| --- | --- |
| `config.rs` | 有序模型路线、参数边界、原生 V2 配置 overlay、工具权限 |
| `http.rs` | loopback Basic auth、有限 JSON 响应、增量 SSE 解析 |
| `routing.rs` | 套餐额度、临时限流、认证、配置与传输错误的区分 |
| `turn.rs` | 会话创建、上下文替换、输入投递、事件收集、明确可安全重试时的模型切换 |
| `mod.rs` | 官方进程生命周期、取消与 Drop 清理、状态、家长权限回复、会话删除与持久化 |

模型协议、MCP 注册 / 执行、服务端会话和工具轮转复用官方 OpenCode。本仓库没有另写一套 Qwen / GLM / DeepSeek HTTP 推理实现，也没有用命令行一次性 `run` 冒充长驻 harness。

默认一时间只接受一轮输入，不在后台排长队。模型输入仅含宿主传入的文本、当前指令与明确允许外发的记忆；本层没有音频、图片或人脸模板上传接口。它不会监听环境声音、启动摄像头、判断孩子在场或主动决定唤醒。

## 3. 安装与启动

### 3.1 安装官方 Apple Silicon CLI

Mac M4 Pro 使用 release 中的 [opencode-darwin-arm64.zip](https://github.com/anomalyco/opencode/releases/download/v2.2.5/opencode-darwin-arm64.zip)，校验文件见同 release 的 [SHA256SUMS](https://github.com/anomalyco/opencode/releases/download/v2.2.5/SHA256SUMS)。这是 CLI 包；桌面应用的 DMG 不等于本模块要启动的 CLI 路径。

按官方发布说明安装，并把 `brain.json` 中的 `harness.executable` 指向实际二进制。通过 Finder 启动的应用不一定继承终端的 `PATH`，因此 Mac 上推荐配置绝对路径，例如 `/Users/你的用户名/.local/bin/opencode`。不要把带参数的 shell 命令填入 `executable`；实现会直接创建子进程，参数固定为 `serve --hostname ... --port ...`。

安装、模型下载、账号登录与第一次真实请求均留待在 Mac 上操作。本轮没有执行这些步骤。

### 3.2 管理模式：默认选择

`harness.mode = "managed"` 时：

1. 本地控制台的启动动作创建一个独立的 `opencode serve` 进程。
2. 默认监听 `127.0.0.1:4097`，不对机器人所在 LAN 暴露 OpenCode API。
3. 每次启动生成随机 Basic auth 密码，仅放在内存和子进程环境中；若设置了 `XIAODAI_OPENCODE_PASSWORD`，则使用该值。
4. 生成宿主的配置 overlay，保留用户已有全局配置与明确指定的 MCP 配置。
5. 等待 `/api/info` 就绪，再接受对话。
6. 正常停止会取消当前会话，终止自己创建的进程组，并清理仍未退出的 MCP 子进程。

端口已占用时直接返回可见错误，不会悄悄连接那个端口上的其他进程。HTTP 客户端禁用代理和跳转，Basic auth 凭据不会通过重定向送到其他地址。密码和 provider key 不进入状态返回值或配置文件。

macOS / Unix 的进程组清理用于正常停止与对象析构；它不承诺处理宿主被系统强制杀死等所有异常情况。普通启动失败与命令不可用会返回状态，不会触发自动安装。

### 3.3 `brain.json` 设置示例

`brain.json` 与原有的 `config.json` 是不同配置。下面是 `brain.json` 中的 `harness` 对象示例；其他顶层设置保留已有值。路径改成自己的 Mac 路径，key 通过启动应用的进程环境提供。

```json
{
  "harness": {
    "mode": "managed",
    "executable": "/Users/yourname/.local/bin/opencode",
    "base_url": "http://127.0.0.1:4097",
    "inherit_config": "/Users/yourname/.config/opencode/opencode.jsonc",
    "working_directory": null,
    "routes": [
      {
        "id": "qwen-coding-plan",
        "label": "Qwen Coding Plan",
        "provider_id": "xiaodai-qwen-plan",
        "model_id": "qwen3.7-plus",
        "base_url": "https://coding.dashscope.aliyuncs.com/v1",
        "api_key_env": "QWEN_CODING_PLAN_API_KEY"
      },
      {
        "id": "glm-coding-plan",
        "label": "GLM Coding Plan",
        "provider_id": "xiaodai-glm-plan",
        "model_id": "glm-5.3",
        "base_url": "https://api.z.ai/api/coding/paas/v4",
        "api_key_env": "GLM_CODING_PLAN_API_KEY",
        "context_tokens": 131072,
        "body": {
          "thinking": { "type": "enabled" },
          "reasoning_effort": "low"
        }
      },
      {
        "id": "deepseek-flash",
        "label": "DeepSeek Flash",
        "provider_id": "xiaodai-deepseek",
        "model_id": "deepseek-flash",
        "base_url": "https://api.deepseek.com/v1",
        "api_key_env": "DEEPSEEK_API_KEY",
        "context_tokens": 131072
      }
    ]
  }
}
```

省略字段采用源码默认值。`routes` 数组整体替换默认路线，按数组顺序执行；不能只在数组中放一项期待它和另外两项自动合并。删除 `inherit_config` 或设为 `null` 可只继承 OpenCode 的常规全局配置；该字段若存在，必须是实际存在的绝对路径。

`working_directory = null` 会创建专用工作目录。若手动配置工作目录，它必须已经存在；程序不会修改那个现有目录的权限。对话 ID 和轮次 ID 使用 1–128 个英文字母、数字、下划线或连字符。

### 3.4 凭据与现有 provider

默认生成的三个 provider 分别读取以下环境变量的值：

| 路线 | 环境变量 |
| --- | --- |
| Qwen | `QWEN_CODING_PLAN_API_KEY` |
| GLM | `GLM_CODING_PLAN_API_KEY` |
| DeepSeek | `DEEPSEEK_API_KEY` |

程序只把变量名写入 OpenCode 配置的 `env`，由官方 provider 加载器取值。启动 bot-server 的进程需要继承这些变量。macOS GUI 应用不会因为某个终端设置过变量就自动拥有它们；可使用自己已有的安全启动流程或 OpenCode 凭据存储，不要把真实 key 写进仓库示例。

如果已经在 OpenCode 中配置了 provider，可在对应路线设置：

```json
{
  "id": "existing-provider",
  "label": "我已配置的模型",
  "provider_id": "与现有配置完全相同的provider标识",
  "model_id": "该provider实际可用的模型标识",
  "base_url": null,
  "api_key_env": null
}
```

此时不生成该 provider 定义，也不要求应用环境中存在专用 key；登录、OAuth、端点和模型定义全部由已有 OpenCode 配置负责。上面两个中文占位值必须换成真实标识，`provider_id` 只能使用英文字母、数字、`_`、`-`。不会根据品牌名称猜测用户已登录的 provider ID。

## 4. 三个模型路线与账号条件

以下默认值来自本次查阅的供应商官方文档，均允许在 `brain.json` 中改动。

| 顺序 | 默认模型 | 默认端点 | 说明 |
| --- | --- | --- | --- |
| 1 | `qwen3.7-plus` | `https://coding.dashscope.aliyuncs.com/v1` | 阿里云中国站 Coding Plan 专用端点 |
| 2 | `glm-5.3` | `https://api.z.ai/api/coding/paas/v4` | Z.AI Coding Plan 专用端点 |
| 3 | `deepseek-flash` | `https://api.deepseek.com/v1` | 独立 DeepSeek API 账户，按其账户余额 / 计费规则使用 |

### Qwen

Coding Plan 使用专用的 `sk-sp-...` key；普通百炼 `sk-...` key 与这个专用端点不是同一类凭据。国际站使用 `https://coding-intl.dashscope.aliyuncs.com/v1`，需要对应站点的套餐、key 和可用模型，不能拿中国站地址或 key 想当然互换。官方资料见 [中国站 Coding Plan](https://help.aliyun.com/zh/model-studio/coding-plan)、[国际站 Coding Plan](https://www.alibabacloud.com/help/en/model-studio/coding-plan) 和 [官方 OpenCode 配置指南](https://www.alibabacloud.com/help/en/model-studio/opencode)。

**部署前要注意账号用途边界**：阿里云当前 [Coding Plan FAQ](https://help.aliyun.com/zh/model-studio/coding-plan-faq) 限制将套餐 key 用于自建应用后端、自动脚本等场景。这个仓库按照要求实现了 Qwen 优先路线，但在后面包一层 OpenCode 不会改变套餐适用范围。机器人服务实际部署时，应使用供应商明确允许该用途的 API 凭据和端点，或禁用这一路线并配置适用的按量 API。代码不绕过套餐限制，也不宣称已经取得此用途授权。

### GLM

本次默认采用官方已列出的 `glm-5.3`，并使用 Coding Plan 专用端点。该模型持续进行推理，应用默认请求较低的 reasoning effort，以控制语音交互等待时间；这不是关闭推理。官方文档见 [OpenCode 接入](https://docs.z.ai/devpack/tool/opencode)、[Coding Plan](https://docs.z.ai/devpack/overview)、[套餐最新模型](https://docs.z.ai/devpack/latest-model) 和 [GLM-5.3](https://docs.z.ai/guides/llm/glm-5.3)。具体套餐可用范围、地区和额度以账户及当前官方规则为准。

### DeepSeek

使用官方模型别名 `deepseek-flash`，而非臆造的 `deepseek-chat-flash` 等名称。本次官方更新记录将该别名指向 V4.1 Flash 系列；别名后面的具体版本可继续由供应商更新。端点兼容 `/v1`。依据见 [模型与计费](https://api-docs.deepseek.com/quick_start/pricing/)、[更新记录](https://api-docs.deepseek.com/updates/) 和 [快速开始](https://api-docs.deepseek.com/)。

本模块的 `context_tokens` / `output_tokens` 是应用给 OpenCode 的较小预算，**不是**对供应商最大上下文窗口的声明。所有默认 provider 都只配置文本输入 / 输出；人脸照片、声纹、原始麦克风内容不会通过模型路线上传。

## 5. 额度耗尽时怎样切换

每轮按配置顺序选择可用路线。禁用的路线、缺少明确要求的环境变量的生成式 provider，以及处于已确认额度冷却期的路线会被跳过。不会估算一个虚假的“剩余套餐百分比”。

| 情况 | 默认行为 |
| --- | --- |
| 明确的套餐额度 / 余额耗尽 | 记录冷却期；满足未产生输出 / 工具副作用的条件时尝试下一路线 |
| 普通 429、并发限制、临时 QPS 限流 | 保留为临时限流，默认不切换；由整轮时限约束官方内部重试 |
| API key 错误或无权访问 | 返回认证错误，避免用备用路线掩盖配置问题 |
| 模型 / 端点配置错误 | 返回配置错误 |
| 网络超时、SSE 中断、响应无法确认 | 取消本轮，不自动重放 |
| 已有文字输出、工具执行或不能确认执行状态 | 取消本轮，不把同一输入自动交给另一个模型重做 |

额度分类结合原生 V2 的结构化错误、HTTP 状态与明确语义。例如，阿里云 `hour/week/month allocated quota exceeded` 属于套餐窗口限制；`concurrency allocated quota exceeded` 和 `usage allocated quota exceeded` 属于临时限制，不能把所有含 `quota` 的错误都判成套餐用光。DeepSeek 官方区分 `402` 余额不足与 `429` 速率限制。来源见 [阿里云错误码](https://help.aliyun.com/zh/model-studio/error-code) 和 [DeepSeek 错误码](https://api-docs.deepseek.com/quick_start/error_codes/)。

若某家提供明确但当前分类器未包含的额度错误码，可把**已核实的精确码**加入该路线的 `quota_codes`。没有把未经官方核实的 GLM 数字错误码硬编码成余额状态。

真正切换前会：

1. 确认上一模型的执行已中断。
2. 查看官方会话投影，回溯到本轮的唯一输入 ID。
3. 确认其间没有工具调用、可见文字、外部插入的用户输入或不明状态。
4. 从该输入**之前** fork 会话，保留以前的对话，并只投递一次本轮输入到下一模型。

这是为了防止模型请求已经执行了工具，却因返回链路出错被另一个模型重复执行。断开的 SSE 不自动重连，也不把未确认的 prompt 再投递。

默认额度冷却为 3600 秒，临时限流冷却为 60 秒；若官方错误包含 `retryAfterMs`，则采用经过上限约束的提示。这个时间是下一次允许尝试的本地退避时间，**不保证套餐在此时重置**。状态中的 `remaining_quota` 明确为 `null`。家长控制台可重置单条路线冷却期；重置不充值、不改变供应商余额。

`fallback_on_rate_limit` 默认为 `false`。明确设为 `true` 时，临时限流也可进入同样的安全切换流程；仍然不能重放已有工具或部分输出的轮次。

## 6. 继承 browser / deepseek-search

### 配置继承

管理模式的 overlay **不含 `mcp.servers`**，不会把用户已有的 MCP 定义清空。OpenCode 仍按官方加载器读取全局配置，以及 `inherit_config` 指向的 JSON / JSONC 文件。现有的 `OPENCODE_CONFIG_CONTENT` 若为合法 JSON，会与应用 overlay 合并；应用安全配置与同名 provider / agent 字段优先。若这个环境变量使用 JSONC 注释，请把它移到配置文件，因为 Rust 端只对 inline JSON 做合并。

管理模式设置 `OPENCODE_DISABLE_PROJECT_CONFIG=true` 和 `OPENCODE_DISABLE_CLAUDE_CODE=true`，避免专用机器人工作目录的祖先项目指令和其他工具配置被偶然加载。需要复用某个项目中的 MCP 时，明确把其配置作为 `inherit_config`，而不是依赖当前工作目录恰好落在那个项目下。

OpenCode 原生 V2 的 MCP 结构是：

```json
{
  "mcp": {
    "servers": {
      "你的服务名": {
        "type": "local",
        "command": ["绝对路径或已有命令", "参数"]
      }
    }
  }
}
```

已有服务就继续使用；没有时可按仓库的 [opencode-tools.example.jsonc](../server/bot-server/examples/opencode-tools.example.jsonc) 补上缺失定义。该示例使用官方 Microsoft Playwright MCP 的独立浏览器会话，并提供本仓库对官方 DeepSeek Search / Fetch API 的 MCP 适配器。相关实现和依赖说明见 `server/bot-server/tools/`。不要把整个示例覆盖到已有配置上。

默认期待的服务名是 `browser` 与 `deepseek-search`。服务名不同，就同时调整 `required_mcp` 和 `read_tools`。工具 action 由 OpenCode 用 `服务名_工具名` 生成，并把不支持的字符替换成 `_`；本应用配置的服务名限于字母、数字、连字符、下划线，以免出现模糊匹配。实现来源见 [官方 MCP 工具实现](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/core/src/tool/mcp.ts)。

状态会真实区分未配置、连接中、已连接、关闭、失败或需要授权。配置里写着 `required_mcp` 并不会凭空安装或启动一个不存在的服务。工具连接失败也不会被报告为“已继承成功”。

### 只读调用与家长操作

权限从全拒绝开始，只允许配置中的 MCP 前缀进入受控的询问流程。OpenCode V2 在 `execute` 被禁用时，会把剩余 MCP 工具直接提供给模型，因此这里**不开放任意代码执行**；shell、文件读写、子 agent、skill 执行等原生工具也保持拒绝。依据见 [官方工具过滤实现](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/core/src/tool.ts) 和 [权限实现](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/core/src/permission.ts)。

默认自动通过一次的精确 action 为：

- `browser_browser_snapshot`：仅没有写文件参数的空输入形式。
- `deepseek-search_search`。
- `deepseek-search_deepseek_search`：兼容已有服务的另一种工具名；不存在的工具不会因此出现。
- `deepseek-search_fetch`。

这些调用也先收到官方权限事件，再由宿主检查实际参数后回复 `once`。出现 `filename`、`file`、`path`、`output` 等输出位置参数不会自动通过；浏览器截图、导航、点击、输入、提交以及其他 MCP 操作进入家长控制台，展示实际工具名与输入供决定“允许一次”或“拒绝”。服务端不接受模型自己声明“家长同意”，也不把孩子声音 / 人脸识别结果作为工具批准凭据。

控制台不提供 `always`，不会新增永久授权。审批等待仍受整轮时限限制，过期请求不会影响下一轮。家长操作本身是否符合具体场景，由宿主控制台和家长决定；本层不会根据工具描述里的 `readOnlyHint` 自动放大权限。

**边界**：这是模型工具入口的权限约束，不是操作系统沙箱。用户继承的 MCP 可执行程序、OpenCode 插件及其已有持久权限属于可信的 Mac 配置；恶意或配置错误的插件本身能使用其宿主账户权限。运行机器人时应使用专用目录和独立浏览器会话，避免把日常登录浏览器的广泛权限顺手授予工具。本应用不会把 MCP 的原始报错、配置命令、凭据或工具输出作为状态日志外发。

## 7. 多轮上下文、取消与记忆删除

### 同一上下文的多轮会话

一个宿主 `conversation_id` 映射到官方 OpenCode 会话。每轮选择 `xiaodai` agent、指定模型，并通过官方 experimental instruction entry 更新当前指令。结构化回复 schema 作为指令传入，最后解析 JSON 对象；原生 V2 没有在这里使用 V1 的 `StructuredOutput` 工具，不能把提示词约束宣称成服务端强制 JSON schema。

返回给宿主的是 `text`、可选 `structured`、实际 provider / model / session 标识。宿主负责验证 speech 长度、表情枚举、卡片和媒体 URL，决定 TTS 与屏幕输出；harness 不执行模型给出的 HTML、网页脚本或舵机动作。SSE 文字增量仅用于控制台进度，不直接驱动设备说话。

### 上下文变化不保留过期记忆

OpenCode 的 instruction entry 会替换当前值，但历史指令更新也可能保留在会话记录里。因此仅覆盖最新 system prompt 不足以保证已经过期或撤销的记忆从旧上下文消失。

本实现保存 `system_prompt + response_schema` 的 SHA-256 指纹。指纹变化时，先用官方 DELETE 删除已跟踪的旧根会话及其 fork 子会话，再创建新会话。相同指纹才沿用历史。家长编辑记忆、改变允许外发的记忆集合、不同人物上下文或 TTL 到期导致宿主注入内容变化时，旧内容不会继续通过原会话历史带入下一轮。

这个选择会牺牲一部分多轮连续性：若宿主按问题检索出的记忆集合每轮不同，就会重新建立模型会话。长期记忆仍由宿主的明确授权记录负责，不能让模型转录历史替代可撤销的记忆库。

### 取消、超时与调用方丢弃 future

取消会先发出本地取消信号，再向官方 API 请求 interrupt。超过整轮预算、SSE 中断或处理失败同样触发清理。文字或最终回复在取消后不会被正常交付。

即使 Tauri / 网络调用方直接丢弃 `Harness.run` 的 future，RAII guard 也会移除 active / pending 状态、标记会话为废弃，并安排 upstream interrupt。失败或取消的会话不在下一轮复用，而是先删除，再创建新根会话；这样旧的异步 interrupt 不能迟到后打断下一轮。自己的并发 gate 在当前 future 生命周期内限制重复投递。

投递输入前还会先持久化这一废弃标记，只有本轮确认成功交付时才清除。这样宿主进程突然退出、来不及执行析构时，下次启动也不会把未完成的旧 inbox 输入随新问题恢复执行；已经正常完成的会话可以继续保留。

### 删除必须真正删除上游本地会话

`forget_conversation` 不是只清掉一个映射：

1. 取消该会话活跃轮次并等待 turn gate。
2. 把所有已知根会话标为 `delete_pending`，先持久化。
3. 如果 OpenCode 尚未启动，临时启动 / 连接本地服务完成删除，不投递 prompt；之后恢复停止 / 断开状态。
4. 调用官方 DELETE；已不存在的 404 视为删除完成。
5. 全部成功后才移除本地 ledger。失败时保留映射和 pending 标记，供下次重试。

应用重启后仍然知道自己创建过哪些根会话。未能完成的删除不会被静默当成“忘记成功”，也不会允许该会话继续使用。宿主的记忆编辑 / 删除流程会先清除已注入的 OpenCode 上下文，再提交本地记忆变更。

OpenCode 自己的本地数据库含有原始文字对话和工具结果。它与宿主的显式记忆库是两种数据；禁止模型自动写长期记忆不等于禁止 OpenCode 保存会话。删除 API 处理的是本机 OpenCode 的会话记录，不代表撤销供应商已收到的 API 请求或其依政策保留的数据，也不承诺擦除文件系统备份。

## 8. 持久化与状态

宿主把 OpenCode 相关文件放在 runtime 目录的 `opencode/` 子目录中：

| 文件 / 目录 | 内容 |
| --- | --- |
| `opencode.generated.json` | 本应用生成的无明文凭据 overlay，便于本地检查 |
| `sessions.json` | 宿主 conversation 到所有 OpenCode 根会话的归属、上下文指纹、删除 / 废弃状态 |
| `routing.json` | 最近失败类型、本地冷却时间及其是否来自供应商 retry hint |
| `workspace/` | 未自定义目录时使用的专用 OpenCode 工作目录 |

目录默认权限为 `0700`，状态文件使用 `0600`，经临时文件、同步和 rename 写入。读取大小有上限；损坏的 ledger 会导致明确错误，而不是丢掉会话归属后声称没有待删数据。文件权限只保护本机其他账户，不替代磁盘加密或备份管理。

状态输出包括服务版本、是否健康、路线顺序、凭据**变量名及是否存在**、冷却情况、MCP 连接摘要、活跃轮次和待审批数量。不会输出密码、API key、原始模型错误体、推理内容或原始工具结果。

常见可操作错误：

| 状态 / 错误 | 下一步 |
| --- | --- |
| `opencode_process` | 检查 CLI 绝对路径、release 版本、端口占用；必要时按相同配置手动启动查看本地错误 |
| `opencode_version` | 使用支持原生 V2 的稳定版，当前最低 2.2.5 |
| `credential_env_missing` | 检查启动应用的进程环境，或改用已登录 provider |
| `provider_authentication` | 对照地区、套餐和专用 endpoint 检查凭据 |
| `rate_limited` | 等待冷却或按需调整明确的限流策略 |
| `all_routes_unavailable` | 检查路线启用、key 和已确认额度冷却 |
| `turn_not_replayed` | 当前执行结果不适合自动重复，重新发起新的用户轮次 |
| `conversation_delete_pending` | 保留数据归属，修复服务连接后重试删除 |
| MCP `configured: false` | 将已有定义明确继承，或配置仓库示例中缺少的服务 |

默认请求预算：启动 30 秒、普通本地 API 10 秒、事件空闲 45 秒、整轮 180 秒、最多 12 个 agent step。家长审批等待使用剩余整轮预算；不会因默认 45 秒模型空闲计时提前取消正在等待的审批。输出有字符数与 SSE / JSON 字节上限，不让断续流或异常服务无限占用内存。

## 9. Attach 模式

`mode = "attach"` 适合已经独立管理原生 V2 OpenCode 服务的环境。它只连接字面 loopback 地址，仍要求显式端口与 Basic auth。

独立服务需要满足：

- 已配置 `xiaodai` agent 和路线中实际存在的 provider / model。
- 与宿主使用相同工作目录，以获得预期的 MCP 配置。
- `OPENCODE_PASSWORD` 与 bot-server 环境的 `XIAODAI_OPENCODE_PASSWORD` 相同，密码长度至少 16 字节。
- 具备同样的受控配置；宿主只会为自己持有的 session 设置权限和模型，不会修改这个独立服务的全局配置。

可以参考 [opencode-attach.example.jsonc](../server/bot-server/examples/opencode-attach.example.jsonc) 创建独立服务配置；provider / MCP 定义需要与自己的路线一致。手工启动时使用官方 CLI：

```sh
opencode serve --hostname 127.0.0.1 --port 4097
```

启动前通过安全的进程环境设置密码和配置文件路径；不要把密码放在 CLI 参数中。`OPENCODE_CONFIG` 指向独立配置，已有全局配置仍由官方加载器合并。attach 模式的 `inherit_config` / 生成 overlay 不会自动作用于那个已运行进程，修改配置后需要由进程所有者重新加载 / 启动。

停止 harness 只中断自己跟踪的会话并断开，不杀死外部服务。删除会话也只操作本应用 ledger 记录的根会话，不扫描或批量删除用户其他 OpenCode 对话。

## 10. 当前交付边界

已落地的是能接入官方原生 V2 长驻服务的 Rust 代码、默认三路线、明确的降级规则、MCP 继承、一次性家长权限、取消清理与隐私删除流程。它仍需在真实 Mac 上完成首轮安装、账号配置和运行联调；本轮没有声称这些已经通过。

没有实现套餐余额查询 UI、供应商计费管理、自动下载 OpenCode、自动安装第三方 MCP、自动获得浏览器账号授权或通用 OS 沙箱。没有新增 wake word 路径：唯一唤醒词仍是“小呆小呆”，设备 / 本地 runtime 保持各自的权限和行为边界。

后续联调时，应优先验证官方服务握手与版本、真实账号端点、一次完整文字轮次、MCP 一次性批准、取消后的新会话以及断网后的删除重试，再接入设备；这是一份交付后的顺序建议，本次没有执行这些操作。
