# 语音交互、回答完整性与唤醒复审：2026-11-20

## 范围与结论

本轮针对用户反馈的「回答不完整、没有响应、小呆小呆难以唤醒」，复审远端 `main` 的 `4ae184b2c11e03e0aa9067cab4a2a2402dda3031`。这是包含 vendored `xiaodai-core`、Luna/HTTP 竞速、逐句播放和触屏补话的新版本；不是重复报告上一轮 PR #30 的修复。

已确认的源码错误分别发生在录音封口、模型流的结束判断、Luna 事件归属、最终正文恢复、语音传输、后台 TTS 调度和失败重试。相关修正随本记录交付。**源码错误成立，不代表已经复现了用户这次每一种实机故障；本轮新版本尚未通过 Mac 和 S31 联调。**

待机唤醒仍使用官方 ESP-SR MultiNet7 的完整词回退，专用「小呆小呆」WakeNet 模型未配置。此前实测已经显示漏识别，本轮没有修改灵敏度、模型或声学门槛，也不宣称唤醒成功率已经改善。

保持 Mac 后端、单一音频 owner、runtime 收音授权、真实播放回执、原有音量及配对设置。普通对话继续使用 Xiaodai core；工具和既有升级路径仍交给 OpenCode。

## 已修正的确定问题

### R1 · P1：用户恢复说话时，旧停顿快照仍能提交前半句

位置：`firmware/components/brain_link/brain_secure.c::secure_task`、`brain_secure_pcm`、`brain_secure_audio_event`。

原路径先复制状态，然后等 PCM 队列、写网络；网络写入可能阻塞。用户在此期间继续说话，旧循环仍按「已经停下」决定 `audio.commit`。最终临界区只检查会话 token/phase，没有重新检查当前说话状态，也没有覆盖「PCM 已获准但尚未入队」的窗口。

修正后，提交同时检查最新 runtime/VAD 状态、输入修订号、PCM 回调在途计数；回调开始和结束均改变修订。新的 VAD END 更新原有 200 ms 收尾起点。封口时没有新的语音或入队活动，才原子撤销 PCM 权限。

VAD 在这里仅能阻止过早提交，不授予收音或联网权限；FreeRTOS queue API 均在 spinlock 外。没有通过扩大录音等待时间掩盖竞争问题。

### R2 · P1：HTTP 流提前结束，仍被报告为回答成功

位置：`server/xiaodai-core/src/client.rs::run`、`src/sse.rs::parse_chunk`。

原代码将无错误的网络 EOF 当成成功，忽略 `finish_reason`。因此无终结标记断流、达到输出长度上限、过滤/中止都可能变成正常 `Done`，用户只听到半句话。

现在要求自然结束 `stop` 或显式 `[DONE]`；`length`、`content_filter`、`aborted` 等非自然结束返回错误，后续 `[DONE]` 不能覆盖该错误。`stop` 同帧携带的最后一段文字会保留。上层继续使用既有未播前升级、已播后明确部分完成的路径。

### R3 · P1：Luna 上一轮迟到的终结事件可结束当前轮

位置：`server/xiaodai-core/src/codex.rs::run_turn`。

原代码只检查顶层 `params.turnId`，但官方 `turn/completed` 的标识位于 `params.turn.id`。在同一 thread 中，先前被取消或超时的回合迟到结束，会被下一回合接受。

现在核对嵌套 turn ID；不同轮次或缺少有效 ID 的终结事件不能终止当前问题。取消、补话仍沿用原有 thread/turn 所有权。

### R4 · P1：Luna 的最终正文没有参与交付

位置：同文件的 `item/agentMessage/delta` 与 `item/completed` 分支。

原实现只消费 delta；只有最终正文的回答会变空，delta 没覆盖完整正文时会丢掉尾句。现在以 item 为单位保存已观察文本，仅补上权威最终正文中可验证的连续后缀。重复快照不重复播；正文与已播前缀冲突时明确失败，不改写已经说出口的内容。补话期间隔离旧 item 的迟到正文。

交叉复审同时收紧补话 handover：文本归属检查、累计状态推进与发送共用 sink 锁，关闭旧尾句在换接收方后误入新问题的窗口。同一 item 在补话后继续追加时，仅恢复确实属于新输入的连续尾部。终止时使用独立 `finishing` 标记拒绝接管；`done` 继续在取消旧订阅、清理 active 之后发布，避免下一轮新订阅被上一轮清理删除。

### R5 · P1：播完短应答再遇到失败，错误提示使用了设备不接受的音频通道

位置：`server/bot-server/src-tauri/src/brain/gateway.rs` 的 `TurnUpdate::Completed(Err)`。

原路径在撤销 ACK 呈现时也清掉 `plain` TCP socket，然后又创建需要语音的失败提示。后续 PCM 落到 WSS 二进制回退，而 S31 明确拒绝未协商的 WSS 音频；TCP EOF 本身也能导致播放失败。

现在 ACK 的硬件/分段回执排空后，同 token 的失败提示保留已协商的 PCM socket。没有待播替代提示、停止或最终结束时，仍按原有生命周期释放通道。

### R6 · P1：应答语音预热争抢正式回答，且可能在 Stop 后继续启动模型

位置：`brain/gateway.rs` 的 `session.open`、`brain/acks.rs`、`brain/mod.rs`、`brain/worker.rs`。

原实现每次缓存未齐的会话打开都会生成一个游离任务；第一句预热占用 `tts_first`，其余占用 `tts`。等待前台空闲的循环到 120 秒后仍强行继续。任务没有服务生命周期归属，后续调用还会读取新的 worker generation，因而可能在 Stop 后继续工作。正式语音的排队或提示期限会被这些冷加载消耗。

修正为由 Brain 持有的单一缓存任务：弱引用、固定模型 generation、Stop 中取消并等待结束；只在没有设备活跃会话及前台回合时使用普通 TTS 通道的非排队准入。新前台/会话能抢占预热，繁忙时继续等待或退出，不再超时后强行合成。不得通过全局取消模型来抢占预热，以免取消独立后台任务。

设备连接的退出清理也改由既有 `OnlinePeer` 析构保护完成，覆盖 `?` 提前返回和任务取消。最后一个连接退出时，在线计数与活动缓存清理共用成员锁；新连接不能被旧退出路径覆盖为无会话。外部事件发送在锁外，状态快照为准，避免残留的 busy 状态让预热一直等待。

缓存未就绪时仍可以省略这一条短 ACK；不能为了准备 ACK 阻塞真实回答。

### R7 · P2：长回答达到分段上限后，静默丢弃中间内容或结尾

位置：`brain/fast_core.rs::bounded_tagged_segments`。

原算法达到段数上限后只试着塞进最后一段；塞不下时只跳出当前句子的内层循环，后续短句仍可能被加入，造成「跳过中间内容直接读后面」。前面未发送分段的容量也没有被利用。

现在保持已发送 live 前缀不可变，只合并未发送、相邻、表情一致且符合字数限制的分段。仍无法容纳时保留连续前缀，明确追加「这段还没讲完，你可以让我分段接着讲。」，标记 `partial / presentation_limit`，不静默跳段。默认 16 段、每段 200 字边界不变。

### R8 · P2：播到一半失败，结构化结果却仍标作完整完成

位置：`brain/fast_core.rs::LiveSpeech::escalate`。

原结果的诊断 summary 是 `partial`，但 `structured.outcome` 是 `answered`，会让后续日志/记忆把半截回答写成正常完成。两者现在一致标为 `partial`。

### R9 · P2：纯标点被当作提问；空语音失败后反复自动重听

位置：`brain/orchestration.rs`、`brain/gateway.rs::Engagement` 及提交/完成分支。

此前 ASR 文本只检查非空，`.`、`……` 会进入模型；11 月 19 日实录也出现过输入 `.`、回答「我在呢」。现在要求至少包含 Unicode 字母或数字，保留中文、英文、数字以及「嗯」等正常表达。

同一激活中的 `asr_no_speech`、`no_speech`、`audio_too_quiet`、`audio_too_short` 只允许一次自动重听。第二次连续失败发送 token 绑定的 `session.expired / asr_retry_exhausted`，不再播新提示并自动开麦。有效结果或新的明确激活重置预算。

连不足 1,600 字节、在模型调用之前失败的短录音，也经过同一完成/重试分支；协议和 token 错误不计作空语音。实板显示「我先安静一会儿，需要时再叫我吧。」并结束当前收音。

### R10 · P2：失败字幕在同一步被清空

位置：`firmware/components/bot_runtime/bot_runtime.c` 的 `FAILURE_CAPTION → REPLY_DONE`。

原实现写入错误字幕后立刻 `arm_followup()` 清空它；用户可能只看到机器人重新等待。首轮尚无旧阅读结果时，本地失败还缺少有界重听。

现在首次失败保留文字并允许一次自动重听，第二次本地失败结束监听并保留说明；新的明确唤醒或真实成功结果重置预算。既有阅读结果的失败追问继续保留旧结果，不让旧 token 的迟到事件影响新问题。

## 唤醒：当前事实与未解决的问题

### 待机说「小呆小呆」没有触发

- 当前默认是 ESP-SR 2.6.1 的 MultiNet7 `mn7_cn`，连续识别完整 `xiao dai xiao dai`，不是专用 WakeNet 模型。待机词表没有单独「小呆」；不能把两次短词事件拼成唤醒。
- 核对了官方 MultiNet 接口的窗口单位、输入 chunk、阈值范围，以及官方 AFE/命令识别示例。此前低能量重置和词 ID/index 的修正已在本次基线中，不重复声称为新修复。
- [11 月 19 日唤醒对照](review-voice-issues-2026-11-19.md#唤醒对照)中，合成完整词在标准档共 1/6 触发，高档 3/6；样本小、顺序不随机、还有 USB 诊断丢包。这能证明当时存在漏识别，不能当作真人准确率，也不能证明调高灵敏度（降低阈值）后的误唤醒风险可接受。
- 下一步应对同一构建做真人/合成音、距离/方向/噪声对照和负样本统计，再决定 MultiNet 标定或专用 WakeNet 模型路线。本轮没有从通用模型凭空生成专用唤醒模型。

### 回答播放中叫它没有反应

默认仍是半双工；扬声器播放和尾音防回授期间抑制语音检测/会话输入，尾音窗口还包含约 800 ms 以及 DMA/AFE 清理。当前没有通过声学验证的全双工 AEC，不能承诺边播边喊即停。使用既有触屏「停止 / 补一句」进行可靠的显式打断；真正语音打断继续由 HC-14 验收。

### 已经应答，但下一句话没被收进去

页面的「我在听 / 可以继续说」要求本地音频 fence 和 Mac 媒体链路都就绪；本地唤醒应答结束不自动证明网络收音已就绪。追问还受原始 ADC −42 dBFS 起音门槛约束，ASR 有另一个录音质量检查。轻声或尾音窗口内抢话可能失败，具体原因须关联就绪时间、VAD、能量、输入帧和 ASR 事件。

本轮 R1 修复的是恢复说话时的提交竞争，不保证任意长停顿仍属于同一句，也没有通过盲目降低门槛接受房间底噪。

## 仍开放的独立问题：极快补话的 RPC/通知顺序 · P2

`codex.rs::steer()` 在等待 `turn/steer` RPC 答复之后才设置 `steer_pending` 并换接收方；RPC 答复和 item 通知由不同消费者处理。存在以下调度顺序：读端已经分发 ACK，`run_turn` 先处理对应的 `userMessage`，之后等待 RPC 的任务才恢复并设 pending。这样会错过补话已消费的边界，后续文字被抑制，直到 `turn/completed` 走 `steer_unanswered`，再用新 turn 补答，增加生成和等待。

R4 的同锁修正解决的是检查与换接收方之间的归属窗口，不能为两个消费者的事件处理建立先后顺序。只把 pending 提前会让 ACK 前的新文字进入旧接收方；把接收方也提前换掉，还需要正确处理请求失败、超时和旧通知回滚。现有 fake 曾通过固定 10 ms 延迟避免这一交错，不能证明它不存在。

此问题单独建 P2 待修 issue，不标「开发完成」。后续需要限定在 steer 请求范围内的顺序屏障或有界缓冲，并覆盖 ACK 前/后立即到达的 userMessage、delta、terminal、请求拒绝及取消；不引入新的通用 Agent 状态机。本轮尚未复现其真实发生频率，保留现有新 turn 补答路径。

## 本轮验证与限制

已执行：

- 对上述路径做静态跨端核对和独立交叉复审。
- 使用现有 `wake_native` host shims，以 `cc -std=gnu11 -Wall -Wextra -Werror` 编译真实 `bot_runtime`、`attention_manager`、`bot_responses`、`bot_voice` 以及现有 `test_runtime_near_face.c`；程序退出码为 0。新增案例覆盖失败字幕、第二次失败退出、新唤醒重置，以及 server 重试耗尽事件。
- `git diff --check`。

新增但未执行的 Rust 回归源码覆盖：HTTP 非自然结束/EOF/终结帧尾字、Luna 最终正文和迟到事件、补话隔离、连续空输入重试、纯标点 ASR、长回答前缀不变与不跳段。

未执行：Rust/Cargo 或 Tauri 构建、完整 ESP-IDF/`brain_secure.c` 编译、模型推理、Mac App 安装、S31 烧录、真实麦克风/扬声器/AEC 测试、GitHub Actions。纯 C 主机测试没有覆盖 FreeRTOS 网络并发或声学质量。前一天的构建/实板通过不能替代本次版本验收。

## 拉取后的针对性验收

Mac App 和固件都需更新到本次修复；只更新其中一端不足以验证录音竞争和新的静默退出提示。沿用 [声音交互入口](voice-interaction.md#1-拉取后怎样开始)和 [ESP-IDF 工作流](esp-idf-development.md)，保留音量、配对、模型/数据分区及当前 App 的密钥来源。

| 场景 | 应观察的结果 | 状态 |
|---|---|---|
| 同一问题停顿后恢复说话，并叠加网络写入延迟 | 恢复的 PCM 不被旧停顿提交切掉；按最终输入帧提交 | 待 S31 |
| HTTP 正常末帧 / 无终结 EOF / `length` | 正常末字保留；中断走明确失败或部分完成，不能标完整成功 | 回归源码已写，待 Rust |
| Luna 只发最终正文 / 丢末段 delta / 旧 turn 迟到完成 | 正文不空、不漏尾、不重复，新 turn 不被旧通知结束 | 回归源码已写，待 Rust/真实 Luna |
| 慢回答先 ACK，随后模型不可用 | 同一已协商 PCM 通道播失败提示，没有 WSS 音频拒绝或无声挂起 | 待 Mac + S31 |
| 缓存冷启动，多次会话打开，然后 Stop / Restart | 只有一个低优先预热任务；会话/前台能抢占；Stop 后不再发起预热 | 待 Mac |
| 超过 16 个短句 / 超过整体呈现容量的长故事 | 能合并的内容完整；超限保留连续前缀并说明未讲完，没有跳中间句 | 回归源码已写，待 Rust/实板 |
| ASR 输出 `.` / 连续无语音或过轻 / 极短录音 | 不调用模型回答标点；一次重听后静默结束，新唤醒可重新开始 | 主机 runtime 已过，跨端待测 |
| 失败后无语音可播 | 失败文字可见，不会同一步消失或无限重听 | 主机 runtime 已过，显示待测 |
| 待机完整唤醒、单个「小呆」、电视/普通对话负样本 | 分开统计漏唤醒、误唤醒、延迟和原始分数 | 待声学验收 |
| 播报中触屏停止/补话，另测语音打断 | 显式触屏继续有效；不得把它记成 AEC 语音打断通过 | 待回归 / HC-14 仍开放 |

记录同一个 runtime token/turn ID 下的 capture-ready、VAD start/end、commit 帧数、ASR 文本、core 结束原因、呈现结果和硬件回执。已知 USB 诊断存在丢包，应同时保留正式 server 事件；不要根据缺失的一条 USB 日志直接归因。

## 参考依据

- [ESP-SR MultiNet 接口，固定提交](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/include/esp32s31/esp_mn_iface.h)
- [ESP-SR AFE 接口，固定提交](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/include/esp32s31/esp_afe_sr_iface.h)
- [ESP-Skainet 官方中文命令示例](https://github.com/espressif/esp-skainet/blob/97885d425d951b41a50747e2c33743d0e233a3e3/examples/cn_speech_commands_recognition/main/main.c)
- [ESP-IDF FreeRTOS 临界区](https://docs.espressif.com/projects/esp-idf/en/stable/esp32p4/api-reference/system/freertos_idf.html#critical-sections)
- [DeepSeek Chat Completions 的结束原因](https://api-docs.deepseek.com/api/create-chat-completion/)
- [Codex App Server 的 turn/item 事件](https://learn.chatgpt.com/docs/app-server)
- [11 月 19 日多轮对话、停顿、插话实录](review-dialogue-rhythms-2026-11-19.md)
- [11 月 19 日唤醒、输入和模型可用性复核](review-voice-issues-2026-11-19.md)
