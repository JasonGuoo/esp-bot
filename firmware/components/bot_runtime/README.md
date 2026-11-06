# bot_runtime：本地行为与会话授权

**状态：源代码已实现；没有编译、运行测试、刷机或实机验证。** 当前目标是 ESP32-S31-Korvo-1 V1.1。这个组件不依赖摄像头、麦克风或 LCD 的具体驱动，也不启动自己的任务；应用以约 20ms 周期从唯一任务调用 `bot_runtime_step()`。

## 职责

runtime 是本地行为状态的唯一主控。音频、视觉、UI 和网络都是事件生产者，不能直接改变脸部状态、开启会话或授予麦克风上行权限。组件输出一份 `bot_runtime_snapshot_t`，由应用在 `ops.apply_state()` 内向平台、音频和绘制模块提交请求。`apply_state()` 必须很快返回；I/O、网络发送、推理和动画播放交给对应的所有者任务。

这层复用 ESP-IDF / FreeRTOS 已有队列与时间模型，新增的代码只表达小呆的本地产品规则。没有重新实现事件线程池、媒体总线、推理框架或驱动。相邻的 [`bot_bus`](../bot_bus/README.md) 负责有界消息传送，[`attention_manager`](../attention_manager/README.md) 负责唯一视觉目标。

## 行为生命周期

以下时间从最近一次明确交互或会话结束开始累计，默认值来自组件的 `Kconfig`，也可以在 `bot_runtime_config_t` 中覆盖：

| 状态 | 默认进入条件 | 输出与资源请求 |
|---|---|---|
| `idle` | 交互后 0–60 秒 | 安静待机表情；无会话、摄像头关闭 |
| `bored` | 60–180 秒 | 无聊表情；不会主动上传声音或发起对话 |
| `drowsy` | 180–300 秒 | 打瞌睡表情 |
| `sleeping` | 超过 300 秒 | 请求屏幕休眠；摄像头关闭；本地关键词监听继续 |
| `night_dark` | 平台报告时钟有效且处于夜间，当前没有会话 | 请求黑屏、停视觉；本地关键词监听继续 |
| `waking` | 完整关键词或有意触摸 | 500ms 唤醒表情；打开会话和摄像头请求 |
| `engaged` | 唤醒动画时间结束 | 由会话子状态决定表情；允许当前会话范围内的交互 |

`night_dark` 使用独立的 `clock_valid`。没有有效时钟时，runtime 不会猜测夜间。夜间策略在进行中的会话结束后生效。夜间不是 ESP 深度睡眠，不能停止唯一音频所有者。长时间停在 sleeping 时有睡眠状态锁存，不会因为 32 位毫秒计数跨周期而自行回到 idle。

只有以下两种输入能够唤醒：

- 音频模块报告 `BOT_WAKE_READY`，并识别**完整的一个“小呆小呆”声学关键词**。runtime 不接受两个“小呆”事件拼接、普通 VAD、声源方位或有人经过作为唤醒。
- UI/平台报告 `BOT_EVENT_TOUCH_WAKE`，并明确设置 `intentional=true`。点击设置按钮应发送设置事件，不应同时发送触摸唤醒。

声学模型缺失是 `BOT_WAKE_MODEL_MISSING`；不可用是 `BOT_WAKE_UNAVAILABLE`。官方 AFE 没有校准后的置信度输出时，`confidence_available=false`；不能填入一个虚构的 1.0。存在分数时只接受有限的 `[0,1]` 数值。当前音频适配器缺少已验证的 AEC 参考，自动 VAD 打断默认关闭。说话期间关键词还须 `echo_safe=true` 才可通过 runtime 的第二层保护；本地停止和有意触摸不依赖回声检测。

## 会话与打断

`waking/engaged` 内的子状态是 `listening → thinking → speaking → waiting`；文字回复可直接从 thinking 进入 waiting。`ending` 是撤销权限时的短暂发布状态，随后回到被动生命周期。默认 60 秒无会话状态进展会结束会话。这是**硬性进展时限**：持续说话超过 60 秒，或一次连续播放超过 60 秒，也会被本地停止；它不是仅在静音时累计的空闲计时器。PCM 帧、表情和字幕更新不延长这个时限。长篇讲解须明确提高 `session_timeout_ms`（菜单项 `BOT_RUNTIME_PROGRESS_TIMEOUT_MS`），后续若区分长篇播放和输入时限，应在 runtime 增加独立策略。

音频 `VAD_STARTED` 仅在已有会话、音频可用且 token 匹配时生效。在 waiting 收到新一轮有效 VAD 时，runtime 建立新 turn。thinking/speaking 时的自动打断还需要 `allow_barge_in=true` 和 `echo_safe=true`，默认都不具备。建立新 turn 的旧 token START 只触发重绑定，并不设置 `utterance_active`。`apply_state` 立即向音频 owner 提交新 token；音频清空 AFE/VAD 历史，在新 token 下重新观察到语音后发送新的 START。只有该 START 可以由同 token END 完成。完全发生在这个边界之前的极短话语会被舍弃，不会被重标或作为新一轮声音上传。不能用收到迟到回调时的“最新 token”重标旧消息。

本地停止走 `bot_bus_request_stop()` 的独立锁存位，不会排在普通队列后，也不会因队列已满而丢失。runtime 在每个处理批次及消息之间查看这个锁存位。处理顺序是：

1. 提升 epoch、清除会话授权，立即发布关闭的上行/音频会话权限。
2. 调用 `stop_output()` 取消本地播放。
3. 调用 `cancel_remote()` 通知 Mac 取消原 token 对应的任务。
4. 回到 idle 或 night_dark；停止之前排队的唤醒不能重新启动会话。

停止由下一次 owner step 生效，因此应用不能在主循环或回调中执行长时间阻塞操作；20ms 是调用目标，不是未经测量的时延保证。播放驱动自身仍应保留可独立触发的停止锁存。

## 权限与过期指令

`epoch/session_id/turn_id` 都是本次启动内的临时编号。新会话、明确中断、停止或可信链路丢失会改变授权，旧任务的结果将被拒绝。计数器耗尽不会静默绕回可复用的编号，而会进入故障状态；需要重新启动并建立新的传输握手。

每条远程命令均须同时满足：

| 检查 | 要求 |
|---|---|
| 本地状态 | 活跃会话，没有设置界面或故障 |
| 链路 | connected、authenticated、encrypted 三项均为真 |
| 会话 | epoch、session、turn 完全匹配且非零会话 |
| 视觉绑定 | focus_generation=0 表示无视觉绑定；非零必须匹配当前且位置有效的 focus |
| 年龄 | 由 ESP 接收入口盖本地 `received_ms`；默认最长排队 5 秒，命令可缩短 TTL |
| 内容 | 已知命令和表情；字幕不超过 383 字节、NUL 终止且为合法 UTF-8 |

目前可接受的命令是表情、字幕、thinking、文字回复完成及结束会话。没有远程唤醒、直接瞳孔位置、每帧电机或提升权限的命令。收到的远程“播放完成”不能代替真实音频硬件完成事件。三个本地 playback 事件必须携带音频 owner 返回的非零 `data.playback.id`；完成消息还要匹配当前正在播放的 id，同一 turn 的旧播放完成事件不能结束后续的新播放。远程回复重复结束不能不断刷新 waiting 超时。

所有被取出的命令通过 `command_result()` 返回 accepted 或具体 rejected 原因。队列接收失败由网络入口立即报告，不能声称已排队。`received_ms` 限制的是本地排队时间，不能伪称已测量 Mac→ESP 的端到端网络年龄。

`runtime.token.focus_generation` 始终为 0，会话与视觉跟踪没有强制绑定。需要针对某个画面目标的命令另外携带 `runtime.focus.generation`。声音 completion 匹配会话三元组，播放 completion 还匹配本地 playback id，不会因为孩子暂时移出画面就错误归属另一轮会话。

## 音频、视觉与界面的仲裁

`audio_session_requested` 仅打开唤醒后的本地 VAD/会话交付，不创建第二个麦克风所有者。`audio_uplink_permitted` 另外要求 `allow_audio_uplink=true`、音频可用和当前可信加密链路。默认配置是 false；已有 LAN discovery/hello 不是身份验证，不具备这个授权。

runtime 不保存原始 PCM，不传送媒体数据，不回放唤醒之前的缓存。音频所有者在会话授权变化时还须清空/隔离 DMA 与已排队帧，使其不会作为“唤醒后”声音泄露。具体边界由 [`bot_audio`](../bot_audio/INTEGRATION.md) 实现。

视觉只在活跃会话中请求启用。缺少可用摄像头时会话可继续，`visual_tracking_permitted=false`，不伪称看到孩子或发生眼神接触。旧快照保留原捕获时间，过期后位置失效；被动期清除 latest snapshot 及其排序时间，防止长时间停摄像头后保留过旧的比较基准；唤醒之前的帧不能成为新会话目标。当前只有临时人脸 track，无跨会话身份或人脸识别记忆。

打开设置立即结束会话、关闭视觉和上行、请求屏幕保持打开。设置 UI 对表情层有最终绘制所有权；关闭设置不会开启会话。`display_awake_requested`、`camera_requested` 是请求，`display_awake`、`display_sleep_supported` 和 `display_full_blackout_verified` 是平台报告的实际能力或状态，两者不能混为“已实机实现黑屏”。

## 故障与线程约束

关键事件队列满会锁存 `critical_fault`，撤销会话并停止输出。不会自动继续猜测缺失的 utterance-end、停止或状态变化。应用解决生产者/队列问题之后，可以从 owner 任务调用 `bot_runtime_clear_fault()`；它不会恢复旧会话，仍须一次新的明确唤醒。普通 UI/网络命令不能清除此故障。

- 所有 runtime API 由一个 owner 任务调用。`get_snapshot()` 不是跨核无锁读接口；其他任务使用从 `apply_state()` 复制的快照并自行加锁。
- `apply_state/stop_output/cancel_remote/command_result/now_ms` 均在 owner 任务执行，不得重入 `step/destroy/clear_fault`。
- 所有时间是 `esp_timer_get_time()/1000` 的低 32 位；使用有符号差值判断年龄，配置必须小于 `2^31ms`。应用应连续调用 step，禁止改用墙上时钟或服务端时间。每类事件的短期排序历史、唤醒防抖和停止屏障都会过期，不把开机时的稀疏事件永久保存为比较基准。
- destroy 前先停止生产者和 owner 循环，保持各输出适配器仍存活，以便完成撤销。

异步音频、视觉和 UI 生产者运行时，应用须接入 `ops.now_ms(context)`，返回与生产者同源的当前本地单调毫秒。runtime 在取出每条事件/命令并收集停止信号后、复制最新视觉快照后、最终处理会话时限前重新读时钟，避免把本次 step 开始后才发布的合法消息误判为“来自未来”并永久丢失。真实晚于该读数的时间戳仍被拒绝；不会使用消息时间推进本地时钟，也没有放宽未来时间容差。该回调必须快速、无阻塞。未设置回调时，保留显式 `step(runtime, now_ms)` 的时间语义，但调用者必须同步提供输入，保证全部输入时间不晚于传入的 `now_ms`。主程序在 step 返回后做显示/注视 reconciliation 时也应重新读取当前时间。

完整调用映射见 [`INTEGRATION.md`](INTEGRATION.md)。当前实现没有网页渲染器、GIF 下载、持久人物档案、语音身份、Mac harness 调度器或主动通知服务；它提供这些上层能力需要遵循的本地授权与表现边界。

## 配置菜单

`Component config → Xiaodai local runtime` 提供 BORED/DROWSY/SLEEP 三个绝对空闲阈值、WAKING 动画时间、会话硬性进展时限、唤醒防抖，以及 sleeping 时是否请求显示休眠。三个被动阈值必须严格递增；非法组合由 `bot_runtime_create()` 返回 `ESP_ERR_INVALID_ARG`，不会悄悄调换或忽略用户设置。上行许可和自动打断没有默认打开的菜单开关；集成者仍须在 C 配置及真实能力满足后明确授权。

## 官方复用来源

- [ESP-IDF FreeRTOS 文档](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-reference/system/freertos_idf.html)：有界任务队列、互斥量和临界区；实现位于 `bot_bus`。
- [ESP Timer](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-reference/system/esp_timer.html)：应用提供的单调时间来源。
- [ESP-SR AFE 文档](https://docs.espressif.com/projects/esp-sr/en/latest/esp32s3/audio_front_end/README.html)：实际音频 owner 对接上游 AFE/WakeNet。该文档的芯片选择不表示本项目改用 S3；目标始终是 S31。
- [项目上游复用清单](../../../docs/upstream-reuse-catalog.md)：BSP、板型、音频、视觉组件的具体版本与限制。
