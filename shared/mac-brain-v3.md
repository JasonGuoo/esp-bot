# 小呆 Mac Brain v3：设备控制与音频协议

**2026-11-16 当前状态：源码开发完成，待测试。** 新增 `presentation.segments.v1`、分段真实回执、独立输出 lease，以及同一音频 TCP 连接上的多段播放。领域任务/家庭记录与设备 runtime 分开；旧客户端保留原单次回复。精确固件接线见 [HOUSEHOLD_INTEGRATION.md](../firmware/HOUSEHOLD_INTEGRATION.md)，配置及剩余边界见 [实施记录](../docs/specs/household-companion/implementation-2026-11-16.md)。本轮未编译 Rust/TypeScript/ESP-IDF、未运行模型、未执行 Mac GUI/打印/实板验证，没有 CI；[11 月 15 日验收](../docs/review-acceptance-2026-11-15.md) 仅对应旧版本。

**2026-11-13 初始交付快照：** Mac 服务端和 ESP32-S31 固件加入可选自由问答语音、播放就绪/
硬件回执、同回合起音保留与下一句预连接的源码。默认仍为静默图文，用户开启设备
“语音回答”后才协商音频。以下是源代码契约；本次环境没有 Cargo、S31 ESP-IDF
或实板，未做 Rust/固件构建、真实 TTS/模型调用或扬声器验收。已有实机记录见
[接入验收](../docs/specs/server-voice-tasks/acceptance.md)，不能用于声称本次新增链路已跑通。

Mac 监听后通过 Bonjour 公布 `_espbot-brain._tcp.local.`。SRV 提供本次监听的主机名和端口，
TXT 包含 `proto=3`、`path=/v3/brain/ws` 和公有配对 CA 的 DER SHA-256 小写十六进制
`ca`。小呆在每次拨号前通过 mDNS 找到与配对 CA 匹配的 IPv4 地址及端口，用配对时的证书
身份验证 TLS，再提交 Bearer 凭据。Mac 在同一条连接上收发设备消息。

本文对应以下源码；调整实现时应同步更新本文：

- [gateway.rs](../server/bot-server/src-tauri/src/brain/gateway.rs)：握手、会话、请求、取消、音频下行。
- [gateway/segments.rs](../server/bot-server/src-tauri/src/brain/gateway/segments.rs)：分段调度、输出 lease、素材与硬件回执归属。
- [plain_audio.rs](../server/bot-server/src-tauri/src/brain/plain_audio.rs)：逐回合局域网明文音频与完整性校验；连接支持多段下行流。
- [media.rs](../server/bot-server/src-tauri/src/brain/media.rs)：PCM 边界和二进制帧。
- [credentials.rs](../server/bot-server/src-tauri/src/brain/credentials.rs)：设备凭据。
- [mod.rs](../server/bot-server/src-tauri/src/brain/mod.rs)：回合协调和返回值。
- [scene.rs](../server/bot-server/src-tauri/src/brain/scene.rs)：屏幕与语音的语义输出。

部署步骤见 [Mac 网关接入说明](../docs/mac-gateway.md)。本协议仅提供语义回合和媒体，
设备仍独占行为、唤醒、注意力、眼神及物理执行。Mac 不发送逐帧瞳孔坐标或舵机角度。

## 音量和静音

已认证设备可在待机或会话中发送 `device.audio.status` 请求，报告输出电平、静音、本地设置页、音频硬件应用状态，以及最近一条控制请求的 ID 和接纳结果。响应中的 `command` 为 `null` 或 `{ "id": 1, "volume": 30, "muted": null }`；数值范围为 0–60，两个可选字段至少有一个非空。设备经 runtime 校验并交给唯一音频 owner 应用，再在后续状态中报告相同 ID、接纳结果、实际设置和硬件应用状态。Server 只有在这些条件均匹配时才显示已应用；该回执不代表 NVS 保存成功。

命令仅针对当前认证连接和在线设备，本地设置页打开或安全连接失效时拒绝，断线不重放。控制请求不创建语音会话或打开麦克风。旧设备若不发送该状态，控制台保持不可操作，原有对话协议仍可使用。

## 1. 连接与认证

设备连接 `wss://<Bonjour 解析出的地址>:<SRV 端口>/v3/brain/ws`。默认端口为
`7443`，也可配置其他端口或由系统分配。TLS 验证使用首次配对时保存的证书身份。
路径必须精确为 `/v3/brain/ws`，不接受查询参数。设备应使用原生 WebSocket 客户端，
在 TLS 验证成功后携带以下 HTTP 升级请求头：

```http
Authorization: Bearer <设备专属的 64 字符 token>
X-Espbot-Device: xiaodai-001
```

`device_id` 必须匹配 `[A-Za-z0-9_-]{1,64}`。每台设备的 token 由 Mac 本地家长
控制台签发；相同设备重新签发会轮换 token。服务端存储 token 的 SHA-256 摘要，
不提供读取旧 token 的接口。现有软配对和 LAN hello 不产生这些凭据。

TLS 使用服务端证书认证，设备凭据使用 Bearer；当前没有客户端证书认证。设备必须通过
可信的初始化渠道取得服务端公有证书/信任材料并校验已配对的服务端身份。当前仓库没有完成
自动 BLE 配网或证书下发。不得把关闭证书校验或接受任意局域网证书作为接入步骤。

升级阶段可能直接返回 HTTP 错误，尚未进入 JSON 协议：

| 条件 | HTTP 状态 / 原因 |
| --- | --- |
| 路径不同或带查询参数 | `404 unknown_endpoint` |
| 请求中存在 `Origin` 头 | `403 device_api_only` |
| 设备 ID / Bearer 缺失、错误或已撤销 | `401 device_credential_required` |

这是设备专用接口。普通网页 WebSocket、家长 Tauri RPC 和旧版 LAN 接口各自独立。
撤销或轮换凭据会使原有连接失效；设备应丢弃旧会话和播放缓冲，重新取得有效凭据后
再连接。设备身份、会话控制和文字仍通过 WSS 传输；当前固件按用户选择在局域网
使用独立 TCP 连接明文传输 PCM。帧带有认证校验，但不具备保密性，局域网中能抓包的
参与者可能听到语音。这个音频连接不等同于旧 LAN v1 诊断通道。

连接建立后，服务端首先发送：

```json
{
  "v": 3,
  "type": "welcome",
  "device_id": "xiaodai-001",
  "capabilities": ["assistant.text", "audio.pcm16", "speaker.candidate", "scene.cards", "display.reply.v1", "audio.reply.v1", "presentation.segments.v1"],
  "sample_rate": 16000,
  "max_audio_seconds": 30,
  "requires_activation": true
}
```

这里的 capabilities 表示服务端协议入口；不代表本地模型已经安装、模型权重已加载，
也不代替设备的输出能力协商和实际显示/播放回执。当前设备支持有界文字、JPEG 与 PCM；没有网页浏览器或任意视频格式解码器。

## 2. 唯一 JSON 信封与 ID

每个 WebSocket 文本消息承载一个 JSON 对象。请求只有以下顶层字段；未知顶层字段
会被拒绝。示例中的所有 ID、时间和内容均为说明值，不是实测数据。

```json
{
  "v": 3,
  "type": "request",
  "id": "r-open-1",
  "method": "session.open",
  "payload": {}
}
```

`v` 必须为 `3`，`type` 必须为 `request`。请求 `id` 匹配
`[A-Za-z0-9_-]{1,128}`；`method` 是下文定义的方法名。没有参数的方法仍建议显式
发送 `payload: {}`。方法参数当前按字段读取，不能通过额外参数获得家长权限。

普通成功响应：

```json
{"v":3,"type":"response","id":"r-open-1","ok":true,"payload":{}}
```

普通失败响应：

```json
{"v":3,"type":"response","id":"r-text-1","ok":false,"error":{"code":"session_busy","message":"session_busy"}}
```

客户端按 `id` 关联请求，不应要求每一种失败都包含 `runtime`。解析失败无法可靠取得
请求 ID 时，服务端使用空字符串 `id: ""`。普通错误的 `code` 取内部错误信息第一个
冒号之前的部分，`message` 最多 500 个字符。

每条连接只保留最近 128 个已接受格式检查的请求 ID，重复使用会返回
`duplicate_request`。这是短期去重，不是持久化重放协议：旧响应不会重新发送，
重连后去重记录清空，也没有跨连接的 exactly-once 保证。对结果不明的回合不要自动
换一个 ID 重做可能执行过的工具操作。

## 3. 运行时令牌

`session.open`、`assistant.text`、`audio.begin`、`audio.commit` 的 payload 都需要：

```json
{"runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0}}
```

四个字段均为 `u32`，必须全部存在，未知字段会被拒绝。它们应来自设备运行时；
服务端当前没有为这些字段额外规定非零值，也不替设备生成 epoch 或选择注意力对象。

| 字段 / 标识 | 当前作用 |
| --- | --- |
| `runtime.session_id` | 后续回合必须与 `session.open` 一致，否则 `runtime_session_stale` |
| `runtime.epoch` | 同一轮保持不变；设备开始新一轮会递增，服务端拒绝倒退或换轮不递增 |
| `runtime.turn_id` | 不得小于打开会话时的值；提交过一个回合后，后续回合必须严格增大 |
| `runtime.focus_generation` | 随结果返回；`audio.commit` 必须与 `audio.begin` 的完整令牌一致 |
| 请求 `id` | 关联单个协议请求及其响应，与运行时回合号不同 |
| `conversation_id` | Mac 为此次明确激活生成的会话字符串 |
| 返回的字符串 `turn_id` | Mac 内部回合标识，用于取消和日志关联，与 `runtime.turn_id` 不同 |

设备接收结果时仍须根据自己的最新完整令牌判断是否过期。服务端只校验上述会话与
递增约束，不替代设备对当前注意力和本地取消的判断。同一会话内不支持 `u32` 回合号
回绕后继续递增；需要结束旧会话，再按设备真实运行时状态创建新会话。

Mac 生成的会话格式为 `device-<device_id>-<32 位 UUID>`，内部回合格式为
`<conversation_id>-<runtime.turn_id>`。客户端可把它们视为不透明关联标识。

## 4. 方法与状态

| 方法 | 主要前提 | 结果 |
| --- | --- | --- |
| `session.open` | 没有已打开会话、执行中回合或待发送音频 | 打开一次明确激活的会话，立即响应 |
| `assistant.text` | 已打开会话，没有采集、执行或播放 | 使用设备已有 ASR / 明确文本，完成整个回合后响应 |
| `audio.begin` | 已打开会话，没有采集、执行或播放 | 在内存开始接收一段 PCM，立即响应 |
| `audio.commit` | 已打开会话，存在对应 PCM，当前未执行或播放 | 封装 WAV，接入 ASR/身份通道；分段模式可返回 `presentation_pending` 并继续异步交付 |
| `audio.playback.ready` | 当前回复声明 `playback_ack=hardware_v1`；令牌和 stream 匹配 | 设备已保留唯一硬件播放队列，Mac 才开始发送 PCM |
| `audio.playback.report` | 当前播放的完整令牌和 stream 匹配 | 上报 `started / finished / failed / cancelled`，终态释放该音频流，不自动结束整份呈现 |
| `presentation.segment.report` | 已协商分段；runtime、plan、lease、index、segment、seq 全部匹配 | 汇总该片段的实际媒体终态及停留；成功后才允许下一片段 |
| `assistant.cancel` | 无需 runtime 参数 | 取消本连接采集、回合和音频下行，保留已打开会话 |
| `session.close` | 无需 runtime 参数 | 同上，并结束本连接会话 |

同一设备连接仍只允许一段输入采集或一个前台回合/呈现流程。同一 OpenCode 前台会话串行接纳输入；持久后台任务使用独立执行会话与有限调度槽，不以原单个 `ActiveTurn` 承载全部工作。worker 和任务容量仍有界，连接数上限不等于并行推理数。

### 4.1 明确唤醒或触摸

```json
{
  "v":3,"type":"request","id":"r-open-1","method":"session.open",
  "payload":{
    "activation":"wake_word","keyword":"小呆小呆",
    "runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0}
  }
}
```

或者使用 `activation: "touch"` 并携带相同结构的 runtime；触摸不要求 keyword。
仅完整 **小呆小呆** 是生产唤醒词。单独“小呆”、VAD、有人脸、视线和声音方位都不是
合法的激活方式。Mac 校验设备上报的激活类型和词字符串，不会在此处重新验证唤醒音频。
因此固件必须先执行自己的唤醒/触摸策略，再发送请求。

返回示例：

```json
{
  "v":3,"type":"response","id":"r-open-1","ok":true,
  "payload":{
    "conversation_id":"device-xiaodai-001-0123456789abcdef0123456789abcdef",
    "runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0}
  }
}
```

再次打开未关闭的会话返回 `session_already_open`。重连后没有旧会话恢复，需要再次
依据实际明确激活发送 `session.open`，不能自动重放旧麦克风内容。

### 4.1.1 图文和语音能力

当前固件在上述 payload 内发送输出能力；只有服务器广告 `presentation.segments.v1` 时才请求分段扩展：

```json
{
  "output": {
    "version": 1, "audio": false, "cards": ["text", "image"],
    "image": {"mime":"image/jpeg","max_width":480,"max_height":272,"max_bytes":65536},
    "presentation": {"version":1,"max_segments":64,"max_duration_ms":120000,"frame_ack":true},
    "video": {"mime":"video/x-motion-jpeg","transport":"jpeg_frames","max_width":480,
      "max_height":272,"max_frame_bytes":65536,"max_fps":5,"max_duration_ms":15000}
  }
}
```

Mac 确认 `payload.output.presentation` 后才发送分段事件；缺少该确认时保持原单次图文/语音路径。当前 `video` 是设备上限描述，Mac 不把它作为任意 codec 协商，也不逐项回显 `video`；实际 JPEG 帧限制见 §7.1。

用户开启设备的 `spoken_answers` 设置且 Mac 广告 `audio.reply.v1` 时，`audio`
改为 `true`。Mac 在 `payload.output` 原样确认音频选择，之后每轮按此会话值设置
`TurnInput.speak`，继续调用既有 Mac 本地 TTS。默认 `false` 不调用回复 TTS。
该选项不会改变 OpenCode、GLM Coding Plan → DeepSeek Flash 路线或工具权限。
只有旧的 `display.reply.v1` 能力时，设备继续图文模式并记录需要更新 Mac 的日志。
没有图文能力或确认值不同则停止此次会话。没有 `output` 的旧 v3 客户端保留原先的
完整响应/音频行为，不要求新增硬件回执；Mac 控制台仍使用自己的语音选项。

### 4.2 设备已经有 ASR 时：文本输入

```json
{
  "v":3,"type":"request","id":"r-text-1","method":"assistant.text",
  "payload":{
    "runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0},
    "text":"为什么会下雨？"
  }
}
```

文本不能为空白，原始输入最多 4,000 个字符；提交后会裁去首尾空白。本路径跳过 Mac
ASR，也不从文字进行声纹识别。设备不能通过请求指定 `profile_id` 或授予工具权限。
本地 TTS 是否执行来自已协商的 `output.audio`；旧客户端沿用原行为。TTS 不可用时
仍返回文字和场景，现代设备响应的 `payload.audio_status` 标明原因。

提交被接受后，服务端先发送 `turn.accepted` 事件（包含请求 `id` 和 `runtime`），
最终响应仍在 ASR（如需）、harness、场景整理和可选 TTS 完成后返回。这个事件不代表
任务已经成功。回合一旦提交到执行任务，`runtime.turn_id` 已被消耗；即使后续推理失败、
忙碌或取消，下一次提交也必须使用更大的运行时回合号。

### 4.3 设备没有 ASR 时：开始音频

```json
{
  "v":3,"type":"request","id":"r-audio-begin-2","method":"audio.begin",
  "payload":{
    "runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0},
    "stream_id":7,"format":"pcm_s16le","sample_rate":16000,"channels":1,
    "transport":"plain_tcp_v1"
  }
}
```

`stream_id` 必须为非零 `u32`。仅支持 16,000 Hz、单声道、有符号 PCM16 little-endian。
开始成功：

```json
{"v":3,"type":"response","id":"r-audio-begin-2","ok":true,"payload":{"stream_id":7,"max_bytes":960000,"plain_audio":{"host":"192.0.2.1","port":40000,"key":"<64 位十六进制临时密钥>"}}}
```

服务端在本次控制连接对应的 Mac IPv4 地址临时监听一个端口，只接受与控制连接相同
来源 IP 的连接，10 秒内未连接则关闭。每次采集使用新生成的 32 字节密钥，仅通过 WSS
交付，不写入日志或设备持久配置。设备连接此 TCP 端口后发送第 5 节的帧；音频没有
加密。上行没有 WAV 头、JSON/base64 音频、Opus 或分片识别结果。PCM 在本次连接的
内存中积累，直到 commit 才调用本地 ASR。

服务端也接受省略 `transport` 的客户端，并让其使用 WSS Binary；当前固件明确请求
`plain_tcp_v1`，失败时不会自动切换。

设备将“本地采集就绪”和“网络可发送”分别上报为带完整令牌的
`BOT_EVENT_CAPTURE_READY`（来源分别为 AUDIO / LINK）。TCP 连接完成只表示后者，
不会单独打开麦克风上行。已明确激活后，音频 owner 可以在本轮 RX DMA/AFE fence
之后保留最多配置量的短起音（默认 800 ms，PSRAM）；网络未就绪时不发送，释放时
仍使用原令牌和采集时间。被动/唤醒前音频、上一回合音频不能填入该缓存。

回复完成后 runtime 可先分配下一句的新令牌并提前 `audio.begin`，同时继续显示
上一轮结果。`followup_armed` 期间 PCM 上行仍关闭；有效的本地 VAD START 消费同一
预授权令牌后才能交付。改变 token、停止、设置打断、播放或本地固定命令会清理
缓存。35 秒采集墙钟从首个有效 PCM 开始，阅读时间不算录音；会话空闲期限仍生效。

### 4.4 提交音频

```json
{
  "v":3,"type":"request","id":"r-audio-commit-2","method":"audio.commit",
  "payload":{
    "runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0},
    "stream_id":7,"frames":100
  }
}
```

`frames` 是本次已经发送的上行帧数。由于控制和音频分别经过两条连接，服务端最多
等待 3 秒接收完指定帧数，再开始处理。`stream_id` 和完整 runtime 必须与 begin 一致。
提交匹配失败会丢弃已经取出的采集内容，
不会保留给另一个 commit。通过后 Mac 在内存添加 44 字节 WAV 头，再通过受监督的
本地模型进程调用 ASR；不存在网络 ASR 回退。

媒体封装层拒绝少于 1,600 字节 PCM（50 ms）的输入。本地 ASR 还要求至少 250 ms；
声纹提取至少需要 3 秒及其他质量条件。不要把 50 ms 的封装下限理解为可用语音时长。
声纹不足或模型缺失可以作为警告降级，不会为了识别人延长被动录音。具体模型契约见
[mac-local-models.md](../docs/mac-local-models.md)。

## 5. 二进制 PCM 帧

上、下行的音频内容共用固定 **20 字节**头。这个 XDAI 帧内部的整数使用 little-endian；
不要直接发送带编译器 padding 的 C/C++ 结构体。

当前 `plain_tcp_v1` 在 TCP 字节流上额外封装：4 字节 big-endian 长度，随后是 1 字节
类型、数据和 32 字节 HMAC-SHA256。长度计算包括类型、数据和 HMAC，不包括长度自身；
HMAC 覆盖类型与数据。上行类型为 `I`，下行类型为 `O`，数据均为完整 XDAI 帧。
下行结束类型为 `E`，数据是 little-endian 的 `stream_id` 和帧数（各 4 字节）。
**`E` 仅结束一条音频 stream，不关闭 TCP，也不结束任务或整份呈现。** 分段模式复用 `audio.begin` 已认证的同一 TCP/HMAC 通道，顺序接收各自新 stream ID 的 `O…E`；各流 PCM 序号从 0 重新开始，先独立 ready，再发送、校验并等待硬件终态。不得复用上一流的序号、改贴新 token 或在中间段 E 后关闭 socket。最终呈现结束、进入追问准备、取消/断线时由所有者释放该通道。
设备必须验证 HMAC、方向、stream 和连续帧数。此校验只能防篡改，不能隐藏语音内容。
省略 `transport` 的客户端则直接以 WebSocket Binary message 发送完整 XDAI 帧。

| 偏移 | 长度 | 字段 | 说明 |
| --- | --- | --- | --- |
| 0 | 4 | magic | ASCII `XDAI`，十六进制 `58 44 41 49` |
| 4 | 4 | `stream_id` | `u32`，对应 begin 或回复的顶层 audio |
| 8 | 4 | `sequence` | `u32`，从 0 连续递增 |
| 12 | 8 | `timestamp_ms` | `u64`；上、下行的时间含义见下文 |
| 20 | 剩余 | PCM | 有符号 PCM16LE、16 kHz、单声道 |

上行每帧 PCM 至少 2 字节、最多 1,280 字节，且为偶数；加头后总长 22–1,300 字节。
推荐每 20 ms 发送 320 个样本，即 640 字节 PCM + 20 字节头 = 660 字节。
按此尺寸计算为每秒 50 帧、33,000 字节，不包含传输封装和 TCP/IP 开销；这是
格式计算，不是硬件带宽或延迟测量。

例如 stream 7、第 0 帧、设备单调采集时间 1,000 ms 的头部为：

```text
58 44 41 49 07 00 00 00 00 00 00 00 E8 03 00 00 00 00 00 00
```

其后直接拼接 640 字节 PCM。等价编码表达式为
`struct.pack("<4sIIQ", b"XDAI", 7, 0, 1000) + pcm_640_bytes`；这是说明表达式，
本轮没有执行。下一帧应使用 sequence 1；若连续采集 20 ms，其采集时间为 1,020 ms。

**上行时间**来自设备本地单调采集时钟。当前服务端仅检查除首帧外不倒退，允许相等；
不将其解释为 UTC，也没有基于这个字段做跨设备绝对新鲜度验证。

**下行时间**为 `sequence × 20`，表示该片段相对播放位置，首帧为 0。
它不复制上行时间，不代表服务端 UTC，也不保证设备声卡恰在此时已经播出。

上行序号或 stream 不匹配、时间倒退、帧长度错误或累计超限时，服务端清空本段音频，
并发送无请求 ID 的错误：

```json
{"v":3,"type":"error","code":"audio_sequence_mismatch"}
```

该类错误不自动结束已打开会话。客户端应放弃本段采集，用新请求 ID 重新 begin，
不能在清空后继续旧流。当前无逐帧 ACK、丢帧补传、采集暂停恢复或分段 commit。

## 6. 完整回复与屏幕内容

以下是未协商 `output` 的旧客户端完整响应结构。回复正文保留 `payload.audio: null`，音频已经从本地
WAV 转为所选通道的 PCM 二进制；可播放音频的描述在**顶层 `audio`**。

```json
{
  "v":3,"type":"response","id":"r-audio-commit-2","ok":true,
  "runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0},
  "payload":{
    "turn_id":"device-xiaodai-001-0123456789abcdef0123456789abcdef-2",
    "conversation_id":"device-xiaodai-001-0123456789abcdef0123456789abcdef",
    "transcript":"为什么会下雨？",
    "scene":{
      "speech":"云里的小水滴越聚越多，变重后就落下来啦。",
      "expression":"curious",
      "cards":[{"kind":"steps","title":"雨从哪里来","body":"水蒸发，遇冷凝结，水滴聚大后落下。","url":null}]
    },
    "provider_id":"configured-provider-id",
    "model_id":"configured-model-id",
    "harness_session_id":"example-session-id",
    "audio":null,"identity_candidate":null,"warnings":[]
  },
  "audio":{"stream_id":41,"format":"pcm_s16le","sample_rate":16000,"channels":1,"bytes":64000,"frame_samples":320}
}
```

`bytes` 是不含帧头的 PCM 总字节数，示例 64,000 字节等于 2 秒。没有有效音频时，
顶层 `audio` 为 `null`，也不会有对应二进制流。模型不可用等 TTS 失败通常出现在
`warnings`；不要仅凭 `ok: true` 就启动空音频播放。

场景字段由 [scene.rs](../server/bot-server/src-tauri/src/brain/scene.rs) 限制：

| 字段 | 当前契约 |
| --- | --- |
| `speech` | 默认最多 1,200 字符，配置允许 100–4,000；TTS 最多使用前 1,000 字符，截断有警告 |
| `expression` | The 24 IDs in [eve-expressions.json](eve-expressions.json); 20 EVE reference poses plus four lifecycle/animation compatibility poses |
| `cards` | 最多 4 张；`kind` 为 `text` / `steps` / `image` / `web` |
| 卡片内容 | `title` 最多 80 字符，`body` 最多 2,000 字符，`url` 为字符串或 null |
| 远程媒体 | 仅允许家长 `media_hosts` 精确名单中的 HTTPS 主机；默认名单为空，未获准 URL 降级成文本 |

URL 不包含凭据，不允许非 443 端口、IP 字面量、localhost 或 `.local` 主机。
场景不承载可执行 HTML、脚本、动作轨迹或摄像头启动命令。设备应按自己的屏幕能力
选择展示方式；`image` 卡片并不等同于已实现 GIF 解码，`web` 卡片也不等同于板端
已经有浏览器。旧单次路径协商文字和有界静态图；分段扩展可顺序执行有界 JPEG 帧。后台任务只有在原 runtime/output lease 仍有效时才可继续输出，完成任务本身不能主动唤醒设备或领取新许可。

`identity_candidate` 是本地识别候选结果。它不是鉴权、家长确认、当前注意力对象或
可自动使用个人记忆的证明；设备请求始终不携带受信任 profile。身份与记忆规则见
[mac-memory.md](../docs/mac-memory.md)。

静默图文扩展使用会话能力协商、当前回合的图片句柄和实际绘制回执。板端已经绘制的结果没有阅读倒计时。超时会撤销语音上行和未交付的资源，已呈现的文字与图像在本地保留到关闭、停止或下一份成功结果。详见[静默回合及图文扩展](../docs/specs/server-voice-tasks/protocol.md)。

保留答案期间，设备固定状态区显示当前任务进度、确认请求和错误。图片单独失败时保留正文与字幕，并上报带有稳定 `code` 的 `unavailable`。`rendered` 绑定到具体控件的可见绘制及驱动 flush-wait 完成；普通整屏刷新或 USB 快照不足以确认一张尚未进入滚动视口的图片。Mac 仍只接受当前结果令牌在交付后 30 秒内的回执，晚于该窗口才首次看到的图片可保持“未确认”。本次修正为源码变更，尚未运行或进行真机验收。

## 7. 音频下行、完成与打断

协商 `output.audio=true` 的新客户端收到紧凑图文结果及如下顶层音频描述：

```json
{"audio":{"stream_id":41,"format":"pcm_s16le","sample_rate":16000,"channels":1,"bytes":64000,"frame_samples":320,"playback_ack":"hardware_v1"}}
```

设备校验完整令牌、格式、非零 stream 和 120 秒总字节上限，先用
`BOT_COMMAND_REPLY_PRESENT` 保留可读结果，再以原 token 预留唯一硬件播放队列。
成功后发送：

```json
{"v":3,"type":"request","id":"r-ready-2","method":"audio.playback.ready","payload":{"runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0},"stream_id":41}}
```

Mac 收到 ready 后才约每 20 ms 发送最多 640 字节 PCM。序号从 0 开始，
最后一帧可以不足 640 字节；调度延迟使用 Delay 策略，不会为追赶时钟而主动补发
一串过期 tick。这里的节拍仍不是实时播放时序保证，设备需要有限的播放缓冲。

所有 PCM 帧发送完后：

```json
{"v":3,"type":"event","event":"audio.complete","stream_id":41,"runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0}}
```

**`audio.complete` 不表示孩子已经听完。** 明文 TCP 路径上，WSS 和音频是独立连接，
设备以经过校验的 `E` 帧作为网络音频结束标记，检查累计帧数和字节数都与描述一致，
再等待唯一音频 owner 的真实 DMA 排空事件。收到 E 只进入 DRAINING，不推进会话。
设备队列暂满时保留原 TCP 帧及序号，最多重试 2 秒；乱序、错误 stream/HMAC、
时间戳或长度不会被跳过。所有异步回调仍绑定原 token 和 playback ID。

硬件 STARTED / DONE / FAILED / CANCELLED 分别映射为：

```json
{"v":3,"type":"request","id":"r-played-2","method":"audio.playback.report","payload":{"runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0},"stream_id":41,"status":"finished","frames":100,"bytes":64000,"code":""}}
```

`status` 可为 `started / finished / failed / cancelled`。finished 仅在服务器已发送
完整内容、已收到 started、帧数和字节数吻合时接纳。重复终态回执幂等；新流不会
接纳旧流回执。Mac 后端事件/回执流的 `device.audio` 记录 ready、硬件状态或 unavailable/unconfirmed，
不将“排队/发完”记为“听完”。纯音频无 ready 最多等 5 秒；分段中带图像时给首次显示 30 秒准备窗口。ready 后最多等 PCM 时长加 10 秒，超时释放媒体并发带原 token/stream 的 `audio.cancelled`。家庭控制台可查看呈现计划及独立显示、音频和汇总回执；Mac 本地预览不记作 Bot 已播放。

旧单次回复的正常硬件 DONE 由 runtime 开启下一句；分段路径改为等待 §7.1 的汇总完成。旧路径如果预留失败，或首个 STARTED 前硬件失败，
保留已接纳的文字并按文字回复结束。TTS 失败、WAV 格式错误或超长时，顶层 audio
为 null，`payload.audio_status={requested,available,code}` 和 Mac `device.audio`
说明原因，不发送半条截断 PCM。硬件完成仍不能证明扬声器连线、音量或人耳确实听到；
实板验收独立进行。旧的无 output 客户端保持发完即释放的旧行为。

夜间且时钟有效时，每条回复在预留时锁存 `night_gain_percent`，设备对 PCM 缩放
一次；codec 仍应用用户音量和静音，网络路径不会解除静音。真实硬件写入进展刷新
本轮无进展计时，WSS 心跳或网络发送进度不会延长硬件播放许可。

默认链路按半双工工作：新协商客户端等待实际硬件终态期间，新的 begin/text 返回 `session_busy`。
需要打断时，设备先在本地立即停播并清空旧缓冲，再发：

```json
{"v":3,"type":"request","id":"r-cancel-2","method":"assistant.cancel","payload":{}}
```

若仍有等待结果的活动回合，服务端先用**原请求 ID**结束它：

```json
{
  "v":3,"type":"response","id":"r-audio-commit-2","ok":false,
  "runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0},
  "turn_id":"device-xiaodai-001-0123456789abcdef0123456789abcdef-2",
  "error":{"code":"cancelled","message":"The active turn was stopped."}
}
```

然后确认取消请求：

```json
{"v":3,"type":"response","id":"r-cancel-2","ok":true,"payload":{"cancelled":true}}
```

若原回合已经成功响应、只剩音频下行，不会为同一请求再发送失败响应。设备自己的
cancel/close 会清空播放状态并返回确认；不能要求此路径一定再收到 `audio.cancelled`。
旧二进制数据可能已经在网络或设备队列中，设备必须按流及当前 runtime 丢弃它们。

家长控制台针对某个回合取消时，等待中的请求也收到上述相关联取消响应；若该回合
已经进入音频下行，则发送：

```json
{"v":3,"type":"event","event":"audio.cancelled","stream_id":41,"runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0}}
```

`session.close` 的请求形式与 cancel 相同，只需替换 method；其确认也是
`payload: {"cancelled": true}`，并要求下一次交互重新明确激活。关闭会话不删除历史。
当前默认不提供全双工效果保证。真实 DAC 参考 AEC 和播放中语音打断已有默认关闭的可选源码，未做声学校准；不能因开关存在声称效果通过。流式 ASR 和边生成边 TTS 也不在当前保证内。

### 7.1 分段呈现、输出许可与汇总回执

`audio.commit` 的成功响应可带 `payload.presentation_pending=true`。这表示前台请求已接纳，呈现/后台任务仍在原 runtime 下等待交付；固件不得同时走旧单次回复完成逻辑。服务端用 WSS `presentation.segment` 顺序发段，例如：

```json
{
  "v":3,"type":"event","event":"presentation.segment",
  "runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0},
  "payload":{
    "presentation_id":"plan-1","output_lease_id":"lease-1","presentation_index":1,
    "segment_id":"segment-1","seq":1,"purpose":"task_ack","final":false,
    "hold_ms":200,"scene":{"speech":"好，我来看看。","expression":"thinking","cards":[]},
    "audio":null
  }
}
```

图卡沿用现有 `asset` 描述（JPEG、尺寸、字节数、SHA-256），音频沿用 §7 的 PCM 描述；一段最多一张图。无音频时 `audio:null` 合法。JPEG 动态帧使用 `media_kind:"mjpeg_frame"`；普通段必须省略 `media_kind`，不能发送 `null`。

| 字段/约束 | 当前执行语义 |
| --- | --- |
| `presentation_id` / `output_lease_id` | 冻结计划与本次输出许可分别编号；读取素材、发段和回执均检查来源与 lease 有效性 |
| `presentation_index` | 同 runtime 从 1 开始；仅在上一个 `task_ack`/`progress` 计划全部完成且没有在播段时，允许下一个 index、新计划和新 lease 从 seq 1 开始 |
| `seq` / `segment_id` | 计划内从 1 严格递增；当前只有一段在途；每个 runtime 累计最多 64 段，每份计划最多 120 秒 |
| `purpose` / `final` | 远端为 `task_ack`、`progress`、`content`、`final`；`final` 布尔值须与 final 用途一致。本地唤醒 ACK 和试听另有 runtime 许可 |
| `hold_ms` | 0～30,000 ms；实际媒体就绪/完成后的最短停留，不能替代显示或音频证据 |
| JPEG / PCM | JPEG ≤480×272、65,536 字节；PCM16 LE、16 kHz、单声道、每帧最多 320 样本；不在设备缓存完整视频 |
| JPEG 帧短片 | 每帧可见停留至少 200 ms，连续墙钟预算 15 秒；Mac 准备最多 12 秒/60 帧，回压可能耗尽预算，不声明已达到 5 fps |

设备先等文本/图像实际 LVGL flush，或明确记录图像失败降级，再发音频 ready。硬件 DONE 只清理该流并回到思考态；所有必要媒体成功及停留结束后，发送：

```json
{
  "v":3,"type":"request","id":"r-segment-1","method":"presentation.segment.report",
  "payload":{
    "runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0},
    "presentation_id":"plan-1","output_lease_id":"lease-1","presentation_index":1,
    "segment_id":"segment-1","seq":1,"status":"completed","code":""
  }
}
```

`status` 只有 `completed` 或 `failed`。网关核对完整标识后才派下一段，并分别保存显示、音频和汇总证据；失败不会归成成功显示/播放。设备只有最后 `final` 段的汇总完成才准备下一句，ACK/进度播完继续等待。失败、取消、断线或新 runtime 撤销旧输出，旧段不得换贴新 token 后回流。

后台任务可在 ACK/进度之后使用递增 index 交付新冻结计划，但不会通过修改原计划来混用 revision。预算不足时服务端保留 Mac 上的完整产物，记录 `device.presentation.deferred` 并用 `presentation_budget_exhausted` 结束设备交付；不能截断尾部再报告完整完成。设备停播或用户插话仅收回相应输出许可，不自动取消已提交的打印等持久工作；取消任务需要明确的任务范围。

## 8. 主动事件、超时与资源边界

| 事件 | 语义与设备动作 |
| --- | --- |
| `session.expired` | 未执行/未发送音频且超过会话空闲期限；清除会话和采集，再次交互需明确激活 |
| `session.cancelled` | 家长全局取消，或取消广播接收落后后保守关闭会话；立即清空旧媒体与会话 |
| `audio.cancelled` | 对应音频下行被家长取消；停止该 stream 并丢弃其剩余帧 |
| `audio.complete` | 服务端结束该 stream 的发送；明文音频仍需等待独立通道的 `E` 帧和本地播放缓冲排空 |
| `presentation.segment` | 在当前 runtime/lease 下执行一段冻结内容；收齐该段实际回执后才接下一段 |

前两个无关联请求 ID，新 Server 携带实际采集/回合的完整 runtime；设备仍兼容旧 Server
省略 runtime 的整会话取消。音频取消必须校验原 token 和 stream。
2026-11-14 源码在全局/指定设备会话取消时先发 `session.cancelled`，再结束活动请求和音频，避免错误响应先准备下一轮令牌。事件增加可选 `code`：`parent_stop`、`memory_changed`、`identity_revoked`、`service_stopped`、`models_unloaded`、`cancel_events_lost`。同批的 `audio.cancelled` 携带 `session_cancelled:true`，单轮取消为 false；客户端仍须校验原 token/stream。旧版缺少这些字段时保留原有关闭逻辑，不能假定始终存在活动请求。

控制台 IPC 的 `assistant.cancel` 与本连接 WSS 方法分开：IPC 接受显式 `scope:turn` + Mac `turn_id`、`scope:device_session` + 网关 `conversation_id`，或 `scope:all`；空 IPC 参数不再取消全部。上文 WSS `payload:{}` 仍只操作当前认证连接。

| 边界 | 当前值与处理 |
| --- | --- |
| 并发连接 | 默认 4，运行时限制到 1–16；超出容量直接关闭新 TCP 连接 |
| TLS / WebSocket 升级 | 分别最多 10 秒 |
| WebSocket message / frame | 最多 4,500,000 字节，不是 4.5 MiB |
| 写缓冲 | `write_buffer_size = 0`，最大 4,565,536 字节 |
| Socket 写入 | 每次写入最多 3 秒；音频二进制写入另有更短的 2 秒外层限制 |
| 心跳 | 每 15 秒发原生 WS Ping；收到任意 WS 消息刷新连接活跃时间 |
| 无上行消息 | 心跳检查时超过 50 秒则断开；不是精确 50 秒定时器 |
| 会话空闲 | 默认 300 秒，可配置 30–1,800 秒；按 15 秒心跳检查 |
| 空闲计时刷新 | 打开会话、audio.begin、有效输入 PCM、提交 text/commit；播放回执、图片读取和心跳不刷新 |
| 上行 PCM | 最多 960,000 字节 = 30 秒；原始媒体仅保存在本次内存流程 |
| 采集墙钟时长 | 首个有效 PCM 后超过 35 整秒，在 push 或心跳检查中清空；预连接阅读不计入录音 |
| 下行 PCM | 最多 3,840,000 字节 = 120 秒；本地 WAV base64 先受 6 MiB 字符串上限约束 |
| 请求去重 | 每连接最近 128 个请求 ID |
| 取消广播 | 容量 64；接收落后按全局取消处理 |
| 网关停止 | 给连接任务最多 5 秒退出窗口；stop 最多等待网关 8 秒后终止任务 |

读取过慢会触发写入期限，不会无限积累回复。设备应保持有界接收/播放队列，过载时
执行本地取消；不能依靠服务端持续缓存音频直至设备恢复。

断连、凭据撤销和服务停止均应立即使设备停止播放、清空流和会话。连接已经关闭时，
未完成请求不会获得取消响应。取消后的晚到结果会被服务端丢弃，也必须被设备本地
令牌校验挡住。重新连接不恢复旧输入、旧音频或旧 conversation。

## 9. 常见协议错误

| code | 含义 / 下一步 |
| --- | --- |
| `invalid_request` / `duplicate_request` | 修正信封或使用新的请求 ID；重复 ID 不会取回旧结果 |
| `explicit_activation_required` / `explicit_session_required` | 依据真实明确唤醒/触摸打开会话 |
| `session_already_open` | 保持当前会话或明确 close 后再开 |
| `runtime_token_required` / `runtime_token_invalid` | 补齐四个 u32 字段 |
| `runtime_session_stale` / `runtime_turn_stale` | 丢弃过期请求，使用设备当前真实令牌 |
| `session_busy` / `brain_busy` | 等待或明确取消当前操作；不同连接同样共享执行门 |
| `audio_capture_in_progress` | 先提交或取消采集，再提交文本 |
| `audio_format_unsupported` / `stream_id_invalid` | 使用规定 PCM 格式和非零 u32 stream |
| `audio_session_required` / `audio_session_mismatch` | 当前采集不存在或不匹配，重新开始一段采集 |
| `audio_frame_invalid` / `audio_sequence_mismatch` | 放弃本段，重新 begin；不补传旧帧 |
| `audio_timestamp_regressed` / `audio_limit_exceeded` | 修正单调时间、帧/总量/墙钟边界 |
| `audio_too_short` / `asr_no_speech` | 仅在用户仍明确交互时重新采集有用语音 |
| `audio_playback_stale` | 回执不属于当前完整 token/stream；不重新播放旧流 |
| `audio_playback_incomplete` | finished 的帧/字节或 STARTED 证据不完整；保留未确认状态 |
| `cancelled` | 当前回合已经结束；不再等待其成功响应 |
| `unsupported_device_method` | 此方法未暴露给设备，不能改用家长 RPC 越过边界 |

本地模型、harness、权限和网络错误会按错误响应继续上送，具体 code 以对应模块为准，
上表不是全部内部错误的穷举。

## 10. 固件接线和仍未完成的部分

[brain_secure.c](../firmware/components/brain_link/brain_secure.c) 现有独立 WSS 客户端、
设备凭据和证书配置、运行时令牌、激活后的明文 PCM 上行和有界 PCM 播放。旧 LAN 连接
仍只用于诊断，不负责媒体。断线和本地停止会撤销会话，超出容量时停止而不继续发送。
这些接线必须以实机和真实服务验证；连接上旧诊断通道不等于通过安全握手。

USB 配对、WSS、既有语音与图文链路的旧版本结果见 [2026-11-15 验收](../docs/review-acceptance-2026-11-15.md)；本轮家庭/分段/AEC 源码没有重新构建或实板执行，稳定延迟、连续播放、声学效果和长期内存余量都须按新版本验收。

专用 WakeNet 模型仍未准备，但固件已有官方 MultiNet7 的完整“小呆小呆”命令路径和 104 条本地应答，不能再把当前入口描述为“只能触摸”。LVGL/官方 JPEG 路径已支持有界静态图，并接入逐帧 JPEG 短片；网页仍是来源链接，没有板端浏览器，也没有 MP4/H.264 通用解码器。AEC 与语音打断默认关闭，现有半双工及触摸停止保持兼容。

本版 v3 **没有图像上传、人脸注册或人脸识别方法**。Mac 控制台已有明确点击的本地
相机/文件识别入口，但不属于本设备协议。设备也不能通过本接口管理记忆、删除档案、
签发凭据、修改配置、批准工具或执行任意 RPC。新增能力需先扩展版本化契约，再接线。
