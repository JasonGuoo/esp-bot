# 小呆 Mac Brain v3：设备 WSS 协议

**状态：2026-11-06，Mac 服务端源码契约。** 服务端实现已写入仓库，本轮没有运行
构建、测试、模型或真实设备。现有 ESP32-S31-Korvo-1 V1.1 固件尚未接入本协议，
不能把本文的消息示例理解为已经完成的板端通信记录。

本文对应以下源码；调整实现时应同步更新本文：

- [gateway.rs](../server/bot-server/src-tauri/src/brain/gateway.rs)：握手、会话、请求、取消、音频下行。
- [media.rs](../server/bot-server/src-tauri/src/brain/media.rs)：PCM 边界和二进制帧。
- [credentials.rs](../server/bot-server/src-tauri/src/brain/credentials.rs)：设备凭据。
- [mod.rs](../server/bot-server/src-tauri/src/brain/mod.rs)：回合协调和返回值。
- [scene.rs](../server/bot-server/src-tauri/src/brain/scene.rs)：屏幕与语音的语义输出。

部署步骤见 [Mac 网关接入说明](../docs/mac-gateway.md)。本协议仅提供语义回合和媒体，
设备仍独占行为、唤醒、注意力、眼神及物理执行。Mac 不发送逐帧瞳孔坐标或舵机角度。

## 1. 连接与认证

连接地址为 `wss://<Mac 证书中的主机名或 IP>:7443/v3/brain/ws`；监听地址和端口可配置。
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
可信的初始化渠道取得服务端公有证书/信任材料并校验连接主机名。当前仓库没有完成
自动 BLE 配网或证书下发。不得把关闭证书校验或接受任意局域网证书作为接入步骤。

升级阶段可能直接返回 HTTP 错误，尚未进入 JSON 协议：

| 条件 | HTTP 状态 / 原因 |
| --- | --- |
| 路径不同或带查询参数 | `404 unknown_endpoint` |
| 请求中存在 `Origin` 头 | `403 device_api_only` |
| 设备 ID / Bearer 缺失、错误或已撤销 | `401 device_credential_required` |

这是设备专用接口。普通网页 WebSocket、家长 Tauri RPC 和旧版 LAN 接口各自独立。
撤销或轮换凭据会使原有连接失效；设备应丢弃旧会话和播放缓冲，重新取得有效凭据后
再连接。服务端不回退到明文传输。

连接建立后，服务端首先发送：

```json
{
  "v": 3,
  "type": "welcome",
  "device_id": "xiaodai-001",
  "capabilities": ["assistant.text", "audio.pcm16", "speaker.candidate", "scene.cards"],
  "sample_rate": 16000,
  "max_audio_seconds": 30,
  "requires_activation": true
}
```

这里的 capabilities 表示服务端协议入口；不代表本地模型已经安装、模型权重已加载，
也不代表设备已经支持网页、图片或音频渲染。

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
| `runtime.epoch`、`runtime.session_id` | 后续回合必须与 `session.open` 一致，否则 `runtime_session_stale` |
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
| `audio.commit` | 已打开会话，存在对应 PCM，当前未执行或播放 | 封装 WAV，调用 Mac ASR，再完成回合后响应 |
| `assistant.cancel` | 无需 runtime 参数 | 取消本连接采集、回合和音频下行，保留已打开会话 |
| `session.close` | 无需 runtime 参数 | 同上，并结束本连接会话 |

同一连接只允许一段输入采集或一个回合/播放流程。不同设备连接和 Mac 控制台还共享
单个 Brain 回合执行门，繁忙时可能返回 `brain_busy`；连接数上限不等于并行推理数。

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
当前设备回合请求本地 TTS；TTS 不可用时仍可返回文字和场景，并在 `warnings` 中说明。

没有单独的“已接受”成功响应。最终结果在 ASR（如需）、harness、场景整理和可选 TTS
完成后返回。回合一旦提交到执行任务，`runtime.turn_id` 已被消耗；即使后续推理失败、
忙碌或取消，下一次提交也必须使用更大的运行时回合号。

### 4.3 设备没有 ASR 时：开始音频

```json
{
  "v":3,"type":"request","id":"r-audio-begin-2","method":"audio.begin",
  "payload":{
    "runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0},
    "stream_id":7,"format":"pcm_s16le","sample_rate":16000,"channels":1
  }
}
```

`stream_id` 必须为非零 `u32`。仅支持 16,000 Hz、单声道、有符号 PCM16 little-endian。
开始成功：

```json
{"v":3,"type":"response","id":"r-audio-begin-2","ok":true,"payload":{"stream_id":7,"max_bytes":960000}}
```

之后发送第 5 节定义的 WebSocket 二进制帧。上行没有 WAV 头、JSON/base64 音频、Opus
或分片识别结果。PCM 在该连接的内存中积累，直到 commit 才调用本地 ASR。

### 4.4 提交音频

```json
{
  "v":3,"type":"request","id":"r-audio-commit-2","method":"audio.commit",
  "payload":{
    "runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0},
    "stream_id":7
  }
}
```

`stream_id` 和完整 runtime 必须与 begin 一致。提交匹配失败会丢弃已经取出的采集内容，
不会保留给另一个 commit。通过后 Mac 在内存添加 44 字节 WAV 头，再通过受监督的
本地模型进程调用 ASR；不存在网络 ASR 回退。

媒体封装层拒绝少于 1,600 字节 PCM（50 ms）的输入。本地 ASR 还要求至少 250 ms；
声纹提取至少需要 3 秒及其他质量条件。不要把 50 ms 的封装下限理解为可用语音时长。
声纹不足或模型缺失可以作为警告降级，不会为了识别人延长被动录音。具体模型契约见
[mac-local-models.md](../docs/mac-local-models.md)。

## 5. 二进制 PCM 帧

上、下行共用固定 **20 字节**头；每个 WebSocket Binary message 是一个完整音频帧。
全部整数使用 little-endian。不要直接发送带编译器 padding 的 C/C++ 结构体。

| 偏移 | 长度 | 字段 | 说明 |
| --- | --- | --- | --- |
| 0 | 4 | magic | ASCII `XDAI`，十六进制 `58 44 41 49` |
| 4 | 4 | `stream_id` | `u32`，对应 begin 或回复的顶层 audio |
| 8 | 4 | `sequence` | `u32`，从 0 连续递增 |
| 12 | 8 | `timestamp_ms` | `u64`；上、下行的时间含义见下文 |
| 20 | 剩余 | PCM | 有符号 PCM16LE、16 kHz、单声道 |

上行每帧 PCM 至少 2 字节、最多 1,280 字节，且为偶数；加头后总长 22–1,300 字节。
推荐每 20 ms 发送 320 个样本，即 640 字节 PCM + 20 字节头 = 660 字节。
按此尺寸计算为每秒 50 帧、33,000 字节，不包含 WebSocket/TLS/TCP/IP 开销；这是
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

以下是带音频的最终响应结构。回复正文保留 `payload.audio: null`，音频已经从本地
WAV 转为 WSS 二进制；可播放音频的描述在**顶层 `audio`**。

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
| `expression` | `idle` / `happy` / `curious` / `thinking` / `surprised` / `sleepy` / `shy` |
| `cards` | 最多 4 张；`kind` 为 `text` / `steps` / `image` / `web` |
| 卡片内容 | `title` 最多 80 字符，`body` 最多 2,000 字符，`url` 为字符串或 null |
| 远程媒体 | 仅允许家长 `media_hosts` 精确名单中的 HTTPS 主机；默认名单为空，未获准 URL 降级成文本 |

URL 不包含凭据，不允许非 443 端口、IP 字面量、localhost 或 `.local` 主机。
场景不承载可执行 HTML、脚本、动作轨迹或摄像头启动命令。设备应按自己的屏幕能力
选择展示方式；`image` 卡片并不等同于已实现 GIF 解码，`web` 卡片也不等同于板端
已经有浏览器。现协议没有设备渲染能力协商或后台主动推送任务。

`identity_candidate` 是本地识别候选结果。它不是鉴权、家长确认、当前注意力对象或
可自动使用个人记忆的证明；设备请求始终不携带受信任 profile。身份与记忆规则见
[mac-memory.md](../docs/mac-memory.md)。

## 7. 音频下行、完成与打断

服务端先发完整成功响应，再约每 20 ms 发送最多 640 字节 PCM。序号从 0 开始，
最后一帧可以不足 640 字节；调度延迟使用 Delay 策略，不会为追赶时钟而主动补发
一串过期 tick。这里的节拍仍不是实时播放时序保证，设备需要有限的播放缓冲。

所有 PCM 帧发送完后：

```json
{"v":3,"type":"event","event":"audio.complete","stream_id":41,"runtime":{"epoch":7,"session_id":12,"turn_id":2,"focus_generation":0}}
```

**`audio.complete` 仅表示音频已写入 WebSocket 发送流程，不表示孩子已经听完。**
设备还要等待自己的输出缓冲排空。当前没有 `audio.ack`、`playback.finished` 或扬声器
播放进度回报；服务端此时即释放播放状态。

当前链路按半双工工作：服务器正在发送音频时，新的 begin/text 返回 `session_busy`。
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
当前没有全双工、声学回声消除、自动声学打断、流式 ASR 或边生成边 TTS 的保证。

## 8. 主动事件、超时与资源边界

| 事件 | 语义与设备动作 |
| --- | --- |
| `session.expired` | 未执行/未发送音频且超过会话空闲期限；清除会话和采集，再次交互需明确激活 |
| `session.cancelled` | 家长全局取消，或取消广播接收落后后保守关闭会话；立即清空旧媒体与会话 |
| `audio.cancelled` | 对应音频下行被家长取消；停止该 stream 并丢弃其剩余帧 |
| `audio.complete` | 服务端发送结束；等待设备自身播放缓冲排空 |

前两个无关联请求 ID，例如 `{"v":3,"type":"event","event":"session.expired"}`。
全局取消还可能先结束活动请求、发音频取消事件。客户端不能假定始终存在一个活动请求。

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
| 空闲计时刷新 | 打开会话、audio.begin、提交 text/commit；单个二进制帧不刷新此计时 |
| 上行 PCM | 最多 960,000 字节 = 30 秒；原始媒体仅保存在本次内存流程 |
| 采集墙钟时长 | 超过 35 整秒后，在 push 或心跳检查中清空；心跳清空不保证另发专属事件 |
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
| `cancelled` | 当前回合已经结束；不再等待其成功响应 |
| `unsupported_device_method` | 此方法未暴露给设备，不能改用家长 RPC 越过边界 |

本地模型、harness、权限和网络错误会按错误响应继续上送，具体 code 以对应模块为准，
上表不是全部内部错误的穷举。

## 10. 当前未实现的接线

现有 [brain_link](../firmware/components/brain_link/README.md) 只接旧版未认证 LAN，
明确报告 `authenticated=false` / `encrypted=false`，拒绝远程请求为 `AUTH_REQUIRED`。
[app_main.c](../firmware/main/app_main.c) 尚未给音频采集提供已认证传输的 PCM consumer。
因此当前固件不会因为 Mac 网关源码加入，就开始上传麦克风、相机、面部轨迹或对话。

后续固件适配需要实现可信证书/凭据配置、WSS 客户端、运行时令牌映射、唤醒后的
PCM consumer、下行播放队列及取消、语义卡片到本地能力的映射。专用完整唤醒词声学
模型、板端播放和资源占用仍需硬件/模型实际准备后处理。

本版 v3 **没有图像上传、人脸注册或人脸识别方法**。Mac 控制台已有明确点击的本地
相机/文件识别入口，但不属于本设备协议。设备也不能通过本接口管理记忆、删除档案、
签发凭据、修改配置、批准工具或执行任意 RPC。新增能力需先扩展版本化契约，再接线。
