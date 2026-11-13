# 接口规格｜语音命令、执行结果与设备显示

本文是[开发计划](README.md)的实施契约。`display.reply.v1`、`turn.progress`、`display.asset.read`、`display.report` 已有源码；2026-11-13 再加入 `audio.reply.v1`、播放就绪/硬件回执、同 token 起音保留和追问预连接。**本次新增内容为源码实现，尚未进行 Cargo/S31 IDF 构建、模型调用或实板验收**。消息中的 ID、摘要、尺寸和时间均为示例值。基础 v3 请求、`turn.accepted`、会话令牌、取消和语音帧沿用[在用协议](../../../shared/mac-brain-v3.md)。

## 1. 一句话对应哪个任务

| 时序 | 输入 → 输出 | 负责方 |
| --- | --- | --- |
| 上电与重连 | Mac 开始监听并公布 `_espbot-brain._tcp` → 小呆查询 SRV、TXT、IPv4 → 主动建立 TCP、验证 TLS 并以设备 ID 和 token 认证 | Bonjour 提供本次监听地址及端口。Mac 在已接受的连接上返回结果。联网和认证可在待机时完成 |
| 明确激活 | 触摸 → 取得当前 `epoch / session_id / turn_id / focus_generation` | `bot_runtime` 管理权限，`bot_audio` 唯一打开麦克风 |
| 建立会话 | WSS 认证 → `welcome` → `session.open` → 确认输出能力 | `brain_secure.c` ↔ `brain/gateway.rs`。Mac 分配字符串 `conversation_id` |
| 收音与提交 | `audio.begin` → TCP PCM → `audio.commit` → `turn.accepted` | 沿用 16 kHz、单声道、16 位小端，`transport="plain_tcp_v1"`。逐帧校验。commit 带 stream、实际帧数及原令牌。Mac 等待这批帧到齐再提交 |
| 理解与执行 | 本地转写 → `TurnRequest.user_text` → OpenCode 工具或直接回答 | `Brain::turn` 继续控制并发和取消。Mac 内部字符串 `turn_id` 关联 harness、权限与工具。设备侧数字 `turn_id` 用于本地仲裁 |
| 返回 | 执行结果 → 校验过的场景 → 本次 commit 的唯一终态响应 | 成功响应先交付文字、表情及可选图片描述。Mac 设备页使用同一次结果 |
| 显示 | 本地接纳结果 → 刷新文字 → 按需拉图、解码、刷新图片 → 分项回执 | `bot_runtime` 仲裁，`face` 渲染。静默回复经 `BOT_COMMAND_REPLY_DONE` 进入等待下一句 |
| 可选语音 | 本地 TTS → 结果和 PCM 描述 → 硬件队列 ready → TCP PCM → 真正播放终态 | `BOT_COMMAND_REPLY_PRESENT` 先保留文字；音频 owner 的 DONE 后才允许下一句，图文独立保留 |

为了响应“显示照片”，Mac 在本机取得并转换图片字节，OpenCode 只需定位文件名或来源。原始录音留在现有设备到 Mac 的音频路径，由本机 ASR 处理。Mac 把当前配置允许的转写、上下文和已授权工具资料交给所选路线。设备请求的 `profile_id` 保持空。执行期间沿用项目现有的个人记忆、留存、遗忘和权限边界。

## 2. 协商输出，兼容现有客户端

新 Server 在 `welcome.capabilities` 广告 `display.reply.v1` 和 `audio.reply.v1`。现有的 `scene.cards` 仅描述 Server 能返回场景。设备默认通过 `session.open.payload` 声明自己可以显示图片，并请求静默回复；用户开启设备“语音回答”设置后，只有 Mac 广告音频能力才改为 `audio:true`。

```json
{"v":3,"type":"request","id":"r-open-1","method":"session.open","payload":{"activation":"touch","runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0},"output":{"version":1,"audio":false,"cards":["text","steps","image"],"image":{"mime":"image/jpeg","max_width":480,"max_height":272,"max_bytes":65536}}}}
```

服务端把能力交集保存在该连接的 `Engagement`，在 `session.open` 成功响应的 `payload.output` 返回实际选项，照常回传 `conversation_id` 和令牌。每轮从会话取值。M1 客户端只有字幕时声明 `text`，省略 `image`。M3 客户端按已实现的视图加入 `steps / image`。只有双方都接受 `image` 才可返回图片句柄。选项不扩大已有工具权限。

| 组合 | 预期行为 |
| --- | --- |
| 新设备、新 Server（默认） | 设备确认 `output.version=1`、`audio=false` 后才准备首轮收音。Server 将 `speak: Some(false)` 交给 `Brain::turn`。`scene.speech` 用作可读回答，线上的 `audio` 为 `null` |
| 新设备、新 Server（用户开启语音） | 双方确认 `audio=true`，Server 使用既有本地 TTS；音频另经 ready 和硬件回执，文字和图片仍独立展示 |
| 旧设备、新 Server | 请求没有 `output` 时走原 v3 响应和原先的输出行为。只给主动协商成功的设备发送新增事件、紧凑结果及资源 |
| 新设备、旧 Server | 有 `display.reply.v1`、无 `audio.reply.v1` 时继续图文并记录更新提示；没有图文能力或未确认输出值则提示更新 Mac 并停止此次会话。Mac 全局默认语音开关不代替设备会话协商 |
| Mac 控制台主动输入 | 继续使用自己的 `TurnInput.speak` 和现有卡片。设备输出选项只跟随相应设备会话 |

## 3. 从转写到可核对的命令

1. **构造输入。** 继续使用 `TurnInput{text 或 wav_base64, conversation_id, turn_id, profile_id, speak}`。开发时可用 `assistant.text` 隔离 ASR，真机口述验收必须走 `audio.commit`。当前 Mac 音频上限 30 秒，固件实际说话/首帧起约 29 秒封顶。设备分别记录本地 RX DMA/AFE fence 就绪与 TCP 就绪；仅网络连上不表示已经可以收音。明确激活后的同 token 短起音可由 owner 有界保留（默认 800 ms PSRAM），网络就绪且 runtime 允许才释放。空语音、音频残缺或转写超长时不投递任务。
2. **解释意图。** 原始有效转写直接构成 OpenCode 的自然语言请求。遇到多个合理对象，Mac 通过 `outcome="needs_input"` 和具体问题结束本轮，下一句在原 `conversation_id` 下澄清。图文设备会话在创建以及重用、切换路线前，均通过 OpenCode 的会话权限关闭 `question` 表单工具，问题由小呆显示。Mac 主动发起的会话可以使用该表单。文件和浏览器等工具的操作授权继续由 Mac 处理。[OpenCode 原生会话接口](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/protocol/src/groups/session.ts) · [按会话权限选取工具](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/core/src/tool.ts)
3. **确认执行效果。** 扩展当前 `harness_tool_activity` 的内部关联信息。以本轮、assistant message 和 tool call 标识配对 `session.tool.called` 与 success/failed，按调用去重。对读取类任务关联实际来源，对保存类任务检查实际写入回执或按需读回，对作图类任务登记实际生成的字节。模型的文字、工具启动通知都不能单独作为任务完成的证据。一般知识问答允许无工具结果。
4. **沿用工具策略。** 原生工具按当前配置自主执行或在 Mac 确认。登录浏览器操作复用现有逐次批准。审批、拒绝、超时都绑定本轮。停止之后，晚到的允许不能继续已取消的工作。当前三条模型路线按原顺序回退，沿用 harness 对“没有部分输出、没有已执行工具”的判定。发生过写入或结果未知时向用户说明，不自动重投。

例如口述“读最新观察表，画出两盆的高度”，ASR 转写直接填入现有 `TurnRequest.user_text`。原有 `conversation_id`、Mac `turn_id`、`system_prompt` 和 `response_schema` 同次传入。OpenCode 在同一任务内查找并读取资料。“读取哪个表、比较哪两行、用什么单位”来自该次请求和工具读到的数据。无需给每一种中文说法写口令，也无需第二次模型调用把结论改成设备格式。

同一次设备对话内保持输出 schema 稳定。现有 harness 在系统提示或 schema 指纹改变时会重建模型会话，即使设备 `conversation_id` 相同。实施 M1 时检查正常追问能沿用前文。因身份切换、遗忘或上下文撤销而重建时，清掉旧的待澄清参数，必要时请用户重新指明对象。验收同时检查正常的“那五盒呢”和重置后的歧义反馈。

Mac 场景使用 `speech / expression / cards`。协商图文能力的设备另用 `scene::device_schema()`，包含 `outcome`、`effect` 和 `visual`。OpenCode v2.2.5 以会话指令接收这份结构，模型也可能只返回普通文字。Rust 将来源或数值与同一轮真实工具结果匹配，校验后才交给设备。普通文字可以显示，但单凭模型的一句“已经完成”无法验证工具效果。[场景校验](../../../server/bot-server/src-tauri/src/brain/scene.rs) · [OpenCode 2.2.5 原生输入](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/schema/src/prompt-input.ts)

`visual.requested` 表示用户要求显示图像，包括对上一轮看图请求的明确选择。明确要求的图片缺失或准备失败时，已有文字仍可读，设备显示图片失败，Mac 标明部分完成。普通问答中模型额外建议的插图失败时，Mac 保留诊断原因，设备照常显示答案。对于有多个合理对象的指代，先按用户的原称呼提问；`needs_input` 回合不制图，补充后再取指定的资料。

**可见进度。** 网关可把同轮本地事件投影为 `turn.progress`。事件携带 commit 请求 ID、完整令牌和同轮递增序号。允许的阶段为 `transcribing`、`understood`、`working`、`awaiting_parent`、`preparing_image`、`synthesizing`。`understood` 可带按 UTF-8 边界截取的转写预览，仅进入本轮显示。其余阶段使用固定提示。模型推理增量、工具原始参数、凭据和个人资料不进入设备诊断日志。

```json
{"v":3,"type":"event","event":"turn.progress","id":"r-commit-1","runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0},"payload":{"seq":2,"stage":"understood","preview":"读观察表，告诉我哪盆高，再画出来。"}}
```

设备当前进展超时默认 60 秒，OpenCode 整轮默认 180 秒。新方案从 commit 起由设备本地单调时钟另设 180 秒整轮上限，Server 使用自身更短的实际时限时可提前结束。只允许同令牌的新阶段或真实工具进展刷新 60 秒无进展计时，等家长确认可在明确的 pending 状态等到整轮上限。重复进度、心跳、图片读取和普通环境音不续时。任一端到期即传播取消，释放权限弹窗和资源。

## 4. 设备结果格式

下面是同时返回文字和一张图的 commit 终态。完整 Mac `TurnOutput` 可留在本轮本地视图。设备收到单独序列化、已计量字节的精简结果。

```json
{
  "v":3,"type":"response","id":"r-commit-1","ok":true,
  "runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0},
  "payload":{
    "turn_id":"device-korvo-01-example-1","outcome":"done",
    "scene":{"speech":"蓝盆 11 厘米，红盆 7 厘米。蓝盆高 4 厘米。","expression":"happy",
      "cards":[{"kind":"image","title":"两盆豆苗的高度","body":"从零刻度比较，单位是厘米。",
        "asset":{"id":"img-01","mime":"image/jpeg","width":480,"height":272,"bytes":14321,
          "sha256":"b64638a23a403549c2039fa12cd732a5f2665570c7cd18d42647419a2bd68dfa"}}]}
  },
  "audio":null
}
```

| 字段 | 规则 |
| --- | --- |
| `outcome` | Server 根据执行情况赋值。`answered` 为直接回答，`done` 为有相应执行证据的任务，`needs_input` 为澄清，`partial` 表示部分完成（列明已完成及缺失部分）。请求错误、拒绝、超时、取消、可能已执行但结果未知使用 `ok:false` 与稳定错误码 |
| `scene.speech / expression` | 核心结论必须可单独阅读。设备字幕最多 383 UTF-8 字节并保持字符完整。长答案在卡片内分段，超出设备预算时提示到 Mac 阅读全文。表情仍由既有枚举和 runtime 校验 |
| `scene.cards` | 上限 4 张文字/步骤/图片卡，首版每轮至多一张图片。每张有可读的 `title` 和 `body`。设备通过这些字段显示图注或故障降级文字。网页按标题和来源文字展示 |
| 设备 JSON 预算 | 整个序列化后的 UTF-8 消息上限 6144 字节（含信封、转义和令牌），给现有 8192 字节收包上限留余量。先精简次要卡片及正文，保留结论、错误和有效图片描述。原始数据、图片 base64 全量、工具输出和绑定身份候选不混入终态 JSON |
| `ok:false` | 复用现有 `error.code`，在已知请求时回显令牌。将 `asr_no_speech`、`brain_busy`、`turn_not_replayed` 等映射为明确的中文提示。无令牌的旧错误只允许匹配本连接当前待答的请求 ID，忽略过期错误 |

服务端在文字已解出后给图像准备一个有界时间（首版建议 3 秒，可根据本机样本调整）。已有文字可先通过进度视图反馈。图片超时或来源失效时返回结论和原因。终态成功只说明 Server 有可交付的结果，显示状态由设备另行回报。

## 5. 两类图像的来源和传输

**照片或插图。** 从本轮成功工具结果中登记确切的图片来源。首版用已授权读取的测试工作区文件。例如 OpenCode 用 `glob` 查得唯一的“观察资料/叶片.jpg”，Mac 核验路径和已有读取权限，本地打开原图并转换，模型只需文件名和说明。若当前读取策略要求确认，先在 Mac 完成原有授权。不能用文件查找成功代替读取权限。若启用联网取图，仅处理工具真实返回且符合 `media_hosts` 的 HTTPS 图片。宿主将已验证的来源映射成会话内的不透明资源 ID。模型给出的路径或 URL 本身不足以授权读取 Mac 文件或下载。工作区媒体检查实际解析后的路径与范围，外部下载限时、限流量，校验每次重定向和解析出的目标地址，不转发凭据。仅为这次媒体读取约束来源，OpenCode 原有工作能力仍由其设置掌管。

**数据图。** 第一种模板为双柱比较。新增仅供宿主校验的结构建议，如 `{"kind":"bars","unit":"厘米","series":[{"label":"红盆","value":7},{"label":"蓝盆","value":11}]}`。渲染器只接收有界标签、相同单位和有限非负数，数值来自本轮确认的表格行或用户明确给出的数字。两个数都为零、等高、缺单位等情况须给出正确说明。Mac 用同一组数计算结论和柱高，再用 Pillow 绘图，确保中文标签有字形。指令、HTML、脚本或任意绘图代码均不属于模板字段。旧客户端继续使用已有场景结构。

**Mac 准备。** 首版接收 JPEG/PNG 静态输入，建议原文件 ≤2 MiB、解码前检查像素 ≤4 百万。应用 EXIF 方向、等比缩放、去元数据，编码为 baseline JPEG。首版输出不大于 480×272、64 KiB，必要时降低质量/尺寸，再失败就用文字。使用既有 Python 环境的 Pillow，避免在网络事件循环或等待显示锁时做同步解码。[Pillow 缩略图示例](https://pillow.readthedocs.io/en/stable/handbook/tutorial.html) · [ImageDraw 示例](https://pillow.readthedocs.io/en/stable/reference/ImageDraw.html)

**按需读取。** 首版通过同一条认证 WSS 用小 JSON 消息分块拉取，单块最多 3072 原始字节（base64 最多 4096 字节）。每台设备同一时刻只有一个块请求在途。按最大块长，64 KiB 的图需 22 次请求，最后一块为 1024 字节。每个响应计入同一 6144 字节预算。这样可继续使用当前 v3 JSON 解析与请求 ID，无需第二条 TLS 连接。该设计专用于偶发静态图。

```json
{"v":3,"type":"request","id":"r-image-1","method":"display.asset.read","payload":{"runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0},"asset_id":"img-01","offset":0,"length":3072}}
```

响应 `payload` 含 `asset_id, offset, bytes, data_base64, eof`，顶层回显完整令牌和请求 ID。`bytes` 是此块解码后的原始字节数。末块可缩短，且只有其末尾恰好等于描述中的总长时才有 `eof=true`。服务端检查认证设备、当前连接、所属会话、已完成回合的完整令牌、资源 ID、偏移及长度。**读取资源和显示回执属于刚完成的那一轮**，需要校验该轮的结果记录，不能调用专用于接收新回合、会拒绝 `turn_id <= last_turn` 的现有 `check_token`。

一个资源最多在 Mac 内存保留交付后的 30 秒。下一句仅预授权 token / `audio.begin` 时保留上一结果；真正的新句首个 PCM、提交 text/commit、停止/取消、设置打断、设备掉线、撤销凭据或服务关闭使旧资源失效。读块超时允许在原连接同一令牌下，仅对同偏移用**新请求 ID**重试一次。设备按请求 ID、偏移、实际长度、总长和最终 SHA-256 组装。重复块不追加，损坏或过期则丢弃。图片重读不会重新提交语音或执行工具。

**设备解码和显示。** 复用项目已解析的 `espressif/esp_new_jpeg 1.0.2` 及其官方解码示例。USB 预览当前使用编码方向，本次需单独验收解码。先解析 JPEG 头并核对实际尺寸与描述，再申请符合 16 字节对齐的 RGB565 输出，在有界工作任务完成解码。480×272 的像素占 261,120 字节，加上最多 65,536 字节压缩输入，合计 319 KiB，另计库工作内存、网络及 LVGL。优先 PSRAM，记录内外堆及最大连续块。图未能分配时保留文字并释放本轮资源。[乐鑫固定版本解码示例](https://github.com/espressif/esp-adf-libs/blob/daee964980d14d7fcf9da10821827399ef600577/esp_new_jpeg/test_app/main/test_decoder.c) · [组件及支持格式](https://components.espressif.com/components/espressif/esp_new_jpeg/versions/1.0.2/readme)

大缓冲由媒体工作任务持有，bus 只传有代次的句柄和状态。提交到 LVGL 前再次核验当前令牌和 runtime 显示许可。`face` 在显示锁内用 `lv_image_set_src` 挂载像素描述符。视图被替换、关闭或销毁时，先解除图像源和缓存引用，再回收像素。收包回调不访问 LVGL，不占显示锁解码。参考[LVGL Image 控件](https://lvgl.io/docs/open/9.4/details/widgets/image)及仓库解析的 LVGL 9.6.0~1 头文件。

文字一旦接纳即可结束静默语音回合；有音频时先通过 `BOT_COMMAND_REPLY_PRESENT` 保留文字，硬件 DONE 才结束回合。runtime 随后分配下一句 token 进入 `WAITING`，保留独立的 `reading_token`。图片下载/绘制和回执仍属于上一结果 token，可与新 token 的 `audio.begin` 预连接并行，直到新句真正 START 或视图撤销。旧音频 TCP 关闭，图片仍经 WSS。等待表情重置时保留已呈现的文字与结果视图。用户收起卡片时立即取消该视图的待绘图回调，避免迟到图片把视图弹回来。开始新一轮即撤销旧令牌的下载和绘制权；本机已经绘制的完整结果可以继续阅读，直到用户关闭、停止，或下一份有效结果替换它。收音超时结束会话并释放上行权限。已有结果后，如果追问只收到空输入或其他失败结果，也结束当前会话，保留已呈现的内容。已绘制的结果没有阅读倒计时。

保留旧答案时，结果视图上方的固定状态区显示当前收音、处理进度、Mac 确认请求和错误，滚动答案不会把状态移出屏幕。runtime 单独保存追问失败提示；自动结束收音后仍能看到该提示，下一次用户任务、关闭或停止时清除。图片失败使用另一条提示，不覆盖正文、图注或当前任务状态。

图片失败覆盖描述/块校验、请求发送、服务端拒绝或过期、块超时重试耗尽、SHA-256、JPEG 头/解码、描述/块/JPEG/RGB565 分配、传递队列和 LVGL 挂载失败。transport 用固定邮箱记录当前令牌的首个失败，向 runtime 投递提示并上报 `unavailable`。记录失败本身无需申请内存；命令或回执队列满时在当前令牌有效期间重试，不以图片错误关闭整个会话。正常的新任务、停止和掉线仍按既有会话撤权规则处理。

## 6. 结果送达、失败和停止

新设备用 `display.report` 单独上报文字与图片。在当前场景挂载、相关区域实际重绘并完成该次 LVGL 刷新后才报 `rendered`。Mac 分别保存该轮的文字和图片回执，两者的到达顺序不影响最终状态。USB 预览独立刷新，核验内容时等待带有该轮实际文字或像素的新帧。

当前固件源码将回执绑定到具体文字/图片控件的可见 `LV_EVENT_DRAW_MAIN_END`、同次 `LV_EVENT_REFR_READY` 和之后的 `LV_EVENT_FLUSH_WAIT_FINISH`，最后复核挂载代次、完整运行时令牌及视图可见状态。`REFR_READY` 或 `FLUSH_FINISH` 本身可能早于异步驱动最后一次传输完成，所以挂载代次会等待后续 flush-wait fence；固件为此仅追加一次刷新。图片完全位于滚动视口之外时不产生绘制回执，需滚动至实际可见后再确认。关闭、替换和新任务撤权会清除所有待回执代次。实现依据为 [LVGL v9.6.0 官方绘制事件示例](https://github.com/lvgl/lvgl/blob/v9.6.0/examples/event/lv_example_event_draw.c)和[刷新/flush 源码](https://github.com/lvgl/lvgl/blob/v9.6.0/src/core/lv_refr.c)。

Mac 的资源和回执授权仍限定为同连接、当前结果的完整令牌和交付后 30 秒。设备上保留的结果不延长这个窗口；超过窗口才首次滚动到图片，Mac 可以继续显示“未确认”。无法识别有效资源 ID 的畸形描述会触发本机图片错误，但不能伪造一个可被 Mac 接纳的资源回执。

Mac 设备页分别显示当前请求的状态和同一设备最近一次已完成的回复。下一次收音、空转写或工具等待时仍能阅读之前的结果。换设备或停止服务时清除旧结果。若存在多个合理的指代对象，返回 `needs_input`，屏幕提出一个具体问题。用户补充后沿用同一对话，Mac 根据新的明确请求取图。

本地事件 `device.turn.completed` 附带 `processing_ms`、`tts_calls` 与 `reply_pcm_bytes`。分别表示 Mac 从开始处理到结果准备好的时间、实际调用本地 TTS 的次数和为该回合安排的回复 PCM 净荷字节数。静默模式不创建下行音轨。USB 朗读测试中，Mac 通过自身扬声器播放的测试指令属于输入，不计入回复 TTS。

```json
{"v":3,"type":"request","id":"r-display-2","method":"display.report","payload":{"runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0},"part":"image","asset_id":"img-01","status":"rendered","elapsed_since_commit_ms":8700}}
```

`part=text|image`，`status=rendered|unavailable`。失败带稳定 `code`，包括 `asset_expired`、`image_descriptor_invalid`、`image_request_failed`、`image_no_memory`、`image_queue_full`、`image_chunk_invalid`、`image_timeout`、`image_digest_mismatch`、`image_decode_failed` 和 `display_unavailable`；服务端已返回的合法有界错误码也可保留。Server 对同连接、令牌、part 和资源做幂等更新，响应普通 v3 确认，并在 `device.display` 记录原因。尚无回执显示“未确认”。回执只更新展示记录，不刷新会话、不启动下一轮。

**2026-11-13 修正状态：** 上述固定状态区、图片错误提示和实际绘制/flush 回执已修改源码，仅做静态源码审阅；本次没有运行构建、测试、lint、刷机、硬件操作、模型调用或 CI。新布局、内存压力下的提示及驱动回执顺序仍待另行真机观察。

设备按本地停止优先级先关上行、禁止旧画面、解除媒体引用，再由工作任务发送 `assistant.cancel` 或 `session.close`。Server 取消模型/工具的未完成部分，清除该轮待批请求和资源。外部副作用已经发生时保留已知结果，无法判定时提示检查。重连沿用新会话，不重放旧语音命令。异常与边界的演练见[验收矩阵](acceptance.md)。

声音阶段已按下节接入同一能力协商和硬件回执的源码。图文结果继续按各自回执计数；实际出声、停止听感和内存/时延须分别实板验收。


## 7. 可选自由问答语音与追问收音

`output.audio` 默认 false。用户在设备声音设置开启 `spoken_answers` 后，新会话
明确选择 true；这不改 Mac OpenCode harness、GLM Coding Plan → DeepSeek Flash
路线、个人记忆或既有本地 ASR/TTS。现有 `macos_say` 仍先合成整段 WAV，网关完成
格式/120 秒上限检查后下发 PCM。这次没有声称逐 token TTS 或已测得首音延迟。

新音频描述带 `playback_ack:"hardware_v1"`。设备只接受完整当前 token、非零 stream、
16 kHz 单声道 PCM16LE、每帧最多 320 样本和正偶数字节总量。先接纳可读答案，
再经 `bot_audio_playback_begin` 预留唯一音频 owner；成功后发送
`audio.playback.ready`。Mac 在 ready 前不发送 PCM，避免独立 TCP 比 WSS 描述先到。
ready 期限 5 秒，之后等待 PCM 时长加 10 秒的硬件终态。具体 JSON 见
[音频回执协议](../../../shared/mac-brain-v3.md#7-音频下行完成与打断)。

设备不在 WSS 回调中播放或阻塞写扬声器。媒体工作任务验证 HMAC、stream、连续
sequence、相对时间戳、实际字节量；播放队列满时原帧不推进，最多有界等待 2 秒。
`E` 帧只表示传输完结，完整字节和帧数匹配后才能排入硬件 END。音频 owner 的
实际 STARTED / DONE / FAILED / CANCELLED 通过非阻塞固定队列送给媒体任务，再以
`audio.playback.report` 上报。完成回执不能由网络的 `audio.complete` 代替。
Mac 拒绝不匹配的 token/stream、没有 STARTED 的 finished 以及不完整帧/字节统计；
重复终态回执幂等。`device.audio` 区分 ready、started、finished、failed、cancelled、
unavailable 和 unconfirmed；不代表麦克风测得的声学播放成功。
这些是 Mac 后端事件/日志中的回执状态，本轮没有新增 Mac 前端播放状态面板。

首条硬件 STARTED 前预留或播放失败时，设备保留已接纳的文字，以 REPLY_DONE
结束文字回退；已 START 后真实 FAILED 由 runtime 处理。停止/设置/新唤醒先撤销
原 token 和精确 playback ID，再取消 Mac 操作。晚到旧回执或旧数据不能取消新的
本地唤醒应答。用户音量和静音由 codec owner 应用；有效夜间设置在每条回复预留时
锁存，PCM 只按 `night_gain_percent` 缩放一次。真实硬件写入进展延续同 token/id
的无进展预算，网络心跳不充当播放进展。

本次使用 `BOT_EVENT_CAPTURE_READY` 的两个来源分开表达就绪：LINK 在 TCP 完成
连接/离开采集时上报，AUDIO 在本 token 的 DMA/AFE fence 完成后上报。root runtime
同时掌握输出/设置/会话许可；其中一个 ready 不会自动授权上传。
`followup_armed` 在上一回复完毕后预分配下一句 token，允许提前 session/audio.begin，
同时保持 PCM 上传关闭。有效 VAD START 消费同一 token，而不是重新命名已经缓存
的 PCM。短起音只在明确授权后保留于有界 PSRAM，停止、播放、token 改变或本地命令
立即清空；溢出不能偷偷丢帧后提交一个残缺命令。预连接本身不会触发 29/35 秒录音
预算，首个实际语音/PCM 才启动；会话空闲与停止仍能收回授权。

同一回合收到固定本地命令时，音频 owner 会关该 token 的 PCM，runtime 执行本地
动作并撤销已开始的旧上行。这不把普通房间声音、旁人说话、看到人或对视当成唤醒，
也不使处于被动状态的小呆开始向 Mac 上传。

升级后需 Mac 与设备同时使用此协议版本才能试语音；保持静默的旧图文用法继续
工作。当前交付只证明源码连接和已说明的本地检查，不代表真实 TTS、扬声器、
夜间音量、Wi-Fi 抖动、打断尾音或第一字识别已验收。
