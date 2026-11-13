# ESP32-S31 单板自主交互与本地 Agent 可行性研究

**研究日期：2026-11-13**

**目标设备：小呆，ESP32-S31-Korvo-1 V1.1；实板摄像头为 SC101IOT。**

**范围：官方文档、官方示例、相关开源实现与同板运行报告；不是本项目的整机验收记录。**

## 1. 结论与适用范围

**小呆可以走“ESP32-S31 独立运行交互与 Agent，按需直接调用云服务”的路线，常规使用不必依赖一台持续开机的 Mac。** 板端可以承担本地人脸检测、已登记人脸匹配、唤醒、语音前处理、有限命令识别、部分声纹验证、中文语音合成、表情、字幕、图片、工具执行、会话与轻量记忆。开放式对话所需的大模型、较强的自由语音识别、复杂视觉理解及生成图片，仍应交给远程服务。[S01][S04][S07][S08][S10][S11][S14]

这与“所有模型全部离线跑在 ESP32 上”有明显的资源边界。官方模型和成功项目证明的是小型感知模型、语音组件和云协作能力，并没有证明这块 16 MB PSRAM 的板可以承载当前桌面方案所用的完整 ASR、通用大模型、自然多语种 TTS 与浏览器运行环境。

关键新证据是乐鑫自己的 **ESP-Claw**：它已有板端 C Agent 执行循环，且有 `esp32_s31_korvo1` 专用板级目录。小智也已有同板适配与双向语音运行报告，ESP-Brookesia 提供语音聊天、表情和设备工具的官方组合示例。[S14][S15][S18][S19][S20]

**建议将单板自主运行作为可选部署方向，复用现有模块及官方组件推进。** 本研究不变更当前 Mac 方案的 OpenCode harness，也不改动其 GLM → DeepSeek 默认顺序。现行架构与实现状态仍以 [architecture.md](../architecture.md)、[local-runtime.md](../local-runtime.md)、[mac-opencode.md](../mac-opencode.md) 和对应验收记录为准。

## 2. 证据等级与项目当前状态

本研究区分以下证据，避免把“某个库支持”直接写成“小呆已经完成”：

| 证据 | 能支持的结论 | 不能推出的结论 |
| --- | --- | --- |
| 官方 S31 数据手册、模型表、组件实现 | 硬件或组件对 S31 有明确支持 | 所有组件在本项目中并发稳定 |
| 同板官方示例、板级配置 | 可复用的外设与应用接入基础 | 用户实板的相机、声学与屏幕效果已经验收 |
| 开源作者的同板上机报告 | 某个配置下已有成功实例 | 我们复现过其结果，或其结果适用于所有版本 |
| 本文的组合与调度建议 | 有依据的工程设计方向 | 已完成代码、已烧录、已测得性能 |

仓库截至本次研究的交付记录显示：真实 S31 板已经接入 Mac 的发现及 WSS 控制链路；BLE 配网、物理扬声器播放等仍有待验收事项。11 月 13 日的模型路由更新是 source-only。旧文档中的历史状态不能覆盖较新的记录。参见 [AGENTS.md](../../AGENTS.md)、[首次实板记录](../board-bringup-2026-11-06.md) 与 [服务端语音验收记录](../specs/server-voice-tasks/acceptance.md)。

## 3. 先确认真实硬件：S31，不是 S3

| 项目 | 已核实能力 | 对小呆的意义 |
| --- | --- | --- |
| 处理器 | 双核 RISC-V，最高 320 MHz；128 位 SIMD/PIE 能力有核上的约束 | 模型和音频优化代码要按 S31 的任务亲和性要求安排 |
| 片内内存 | 512 KB HP SRAM，另有 32 KB LP SRAM | PSRAM 充足不等于内部 DMA、任务栈、实时缓冲不紧张 |
| 板载存储 | 16 MB Flash + 16 MB PSRAM | 足以组合轻量功能；模型、字体、资源、OTA 分区要统一预算 |
| 屏幕 | 配套 SUB3：4.3 英寸、800×480、RGB565，带 GT1151 触摸 | 可显示表情、字幕、图片和简单可视化 |
| 音频 | ES8389、双麦、双 NS4150B 功放输出 | 可进行采音、前处理和播放；扬声器及实际声学效果仍需实板确认 |
| 摄像头 | 官方板配置存在 OV3660、SC101IOT 两种组合 | 本项目已经确认是 **SC101IOT**，不可沿用 OV3660 的默认格式假设 |
| 多媒体辅助 | JPEG、PPA、二维 DMA 等硬件能力 | 可减轻图像转换和搬运负担，但需要对应驱动正确接入 |
| 存储扩展 | microSD | 适合音频片段、图片、较大模型或资源；不能替代运行内存 |

以上硬件信息来自数据手册、开发板指南和 Board Manager 定义。[S01][S02][S03]

### 3.1 SC101IOT 与网上 OV3660 案例的关键差别

官方 SC101IOT 配置提供 YUV422 采集格式，包括 720p 和 VGA 档位；相关示例可借助 PPA 转换后显示或供模型使用。很多现有 Korvo 项目默认使用 OV3660 的 JPEG 输出，传输量、驱动配置、帧缓冲大小和解码路径都不同。[S03][S12]

因此，复制小智、ESP-Claw 或其他同板项目时，要复用本项目已经确认的 SC101IOT 适配，并逐项核对格式、旋转、镜像和颜色顺序。同一主板型号并不能保证预编译固件的相机配置适合这块实板。

## 4. 按用户需求划分板端能力

| 用户希望的能力 | 板端能承担什么 | 仍需的条件或外部能力 |
| --- | --- | --- |
| “看到人” | 本地人脸检测、位置与关键点 | 光照、视场和相机方向正确；检测结果有时效与置信度 |
| “认出家人” | 注册人脸特征并比对 | 人脸录入、拒识阈值、姿态和儿童样本验证；不能当作高可靠身份认证 |
| “看着对方” | 按当前相机坐标驱动屏幕眼睛方向 | 相机与屏幕几何标定；现有固定底座不能物理转头 |
| “知道对方是否看我” | 可用脸和关键点建立粗略证据 | 真正眼球注视需额外模型、分辨率与验证，不能由检测到脸直接推断 |
| “听到人说话” | VAD、降噪、回声消除等语音前处理 | VAD 只说明可能有语音，不说明内容、身份或是否在叫小呆 |
| “小呆小呆”唤醒 | 专用 WakeNet，或用官方 MultiNet 做完整短语的本地识别 | 专用 WakeNet 模型需要获取/定制；MultiNet 路线需独立校准误触发与漏唤醒 |
| “听懂固定指令” | MultiNet 中文/英文有限命令集 | 命令词注册和模型配置；不是任意内容的语音转写 |
| “听懂自由聊天” | 采集、编码、流式上传和会话控制 | 云 ASR 或端到端实时语音模型；Mac 本地 ASR 可继续作为可选后端 |
| “分辨谁在说话” | 官方 S31 speaker verification 模型可提取和比对声纹 | 3 秒或 6 秒窗口、登记样本、多人/远场/回声条件下的阈值验证 |
| 调用大模型的本地 Agent | 本地上下文、记忆、工具循环、HTTP/MCP、调度 | 通用大模型推理在云端；桌面工具需远程节点 |
| 语音回答 | 本地音频片段、官方中文 TTS、流式播放云 TTS | 自然多语种/特定音色通常交给云或 Mac |
| 图片及可视化回答 | 下载/缓存/解码图片，LVGL 图表、文字、卡片和动画 | 复杂图像生成在云端；任意网页或桌面浏览器另需执行环境 |
| 断网时仍有互动 | 本地唤醒、表情、有限命令、预置应答 | 离线反馈必须有本地音频资源或已接好的本地 TTS |

该表是能力分配，不是当前 firmware 的完成清单。[S04]–[S12][S14]–[S20]

## 5. 本地视觉：检测、身份和注视要分别实现

### 5.1 官方 S31 人脸模型已存在

ESP-DL 的人脸检测与识别组件已明确列出 S31。不能因为 ESP-WHO 顶层开发板表还没有列出 S31，就认定 ESP-DL 不支持；也不能将其他芯片的示例不经核对直接作为 S31 现成工程。[S04][S05][S06]

官方模型时延如下。**条件为 S31 CPU 320 MHz、PSRAM 250 MHz、int8、性能优化编译；这是模型阶段基准，不是本项目整机帧率。**

| 模型/阶段 | 输入 | 前处理 | 模型推理 | 后处理 | 用途 |
| --- | --- | ---: | ---: | ---: | --- |
| MSR S31 | 120×160×3 | 1.9 ms | 19.0 ms | 0.2 ms | 两阶段人脸检测的候选生成 |
| MNP S31 | 每候选 48×48×3 | 0.4 ms | 3.5 ms | 0.1 ms | 对 MSR 每个候选进一步判断 |
| ESPDet Pico Face 224 | 224×224×3 | 3.5 ms | 85.0 ms | 0.4 ms | 一阶段人脸检测 |
| ESPDet Pico Face 416 | 416×416×3 | 9.7 ms | 281.2 ms | 0.6 ms | 较大输入的一阶段人脸检测 |
| MFN S31 | 112×112×3 | 2.6 ms | 155.0 ms | 0.1 ms | 人脸特征提取 |
| MBF S31 | 112×112×3 | 2.6 ms | 277.1 ms | 0.1 ms | 较大的人脸特征模型 |

来源：[S04][S05]。MSR+MNP 总耗时随候选数增加，不能只看 19 ms；识别还包含检测、裁剪、对齐、取帧和比对。Wi-Fi、AFE、UI 同时运行时又会竞争带宽和计算时间。

### 5.2 小呆应采用分频率、分触发条件的视觉路径

建议持续低频执行“有没有人/人在哪里”，仅在有效会话、目标变化或身份不确定时触发人脸识别。对稳定目标保留带有效期的身份判断，不必每帧重新提取特征。这是本文的调度建议，尚无本项目并发性能实测。

多脸检测与多脸身份识别也需要区分。官方识别便利接口中的路径会选择最大脸，不意味着调用一次就把画面所有人逐一登记或认出。多目标跟踪、目标选择与身份缓存仍由小呆的模块契约管理。[S05]

### 5.3 不把“脸朝向屏幕”写成准确眼神对视

脸框与五点关键点足以提供双眼中点和粗略朝向，可用于屏幕眼睛的共同目标；它们本身不能可靠回答真实眼球正在注视哪里。眼球注视估计与视线标定仍是独立任务。V1 没有已安装电机或 IMU，目标离开视场后不能声称小呆会物理转头重新找到对方。参见 [eye-contact.md](../eye-contact.md)。

## 6. 本地声音：可做的远多于录音，但并非完整开放式 ASR

### 6.1 官方语音组件

ESP-SR 已对 S31 提供 WakeNet、MultiNet 和 AFE。AFE 中的 VAD、噪声处理、AEC 等可作为持续采音路径的组成部分；MultiNet 的中英文命令识别与自由语音转文字应保持明确边界。[S07][S08]

官方 S31 基准中，三通道 WakeNet10 使用约 16 KB 内部 RAM、523 KB PSRAM，每 32 ms 音频块约需 6.4 ms 处理，约为单核时间的 19.9%。这只对应指定 WakeNet 配置，不能代替完整 AFE、Opus、网络、视觉的总资源预算；同页沿用的 Alexa 识别指标来自其他测试条件，不能作为本项目 S31 远场准确率。[S08]

板级 BSP 对 ES8389 的时钟接线有说明，建议采样率基线采用其支持配置；S31 示例中的 16 kHz 路径优先于自行组合未验证的采样率。GMF 的 S31 PIE 优化也有 Core 1 运行要求，移植时要查对应版本说明。[S13][S22]

### 6.2 “小呆小呆”有两条本地实现路线

产品规则保持不变：**完整四音节“小呆小呆”才是语音唤醒；单次“小呆”、人脸出现、眼神或普通聊天都不是唤醒事件。**

| 路线 | 当前证据 | 使用边界 |
| --- | --- | --- |
| 专用 WakeNet 唤醒模型 | 官方现有唤醒列表未找到“小呆小呆”；有模型定制/请求流程 | 不能通过更改显示名称或配置字符串凭空得到声学模型 |
| MultiNet 完整短语识别 | 小智 `custom_wake_word.cc` 已有持续运行 MultiNet 作为自定义唤醒的实现，可注册完整 `xiao dai xiao dai` | 可直接实现相应识别链路；它不是专用 WakeNet 模型，资源、误触发、漏触发须实板验收 |

官方模型列表、自定义模型讨论及小智实现见 [S09][S21]。这里所说的 MultiNet 路线是“识别完整四音节命令”，不是把两次独立“小呆”检测在应用层凑在一起。

无论采用哪条路线，都要让音频模块拥有唯一麦克风采集流，唤醒、VAD 和可选 DOA 共享样本。未唤醒的环境音不持久化、不上传。夜间待机应是屏幕和视觉降低活动、语音识别保持运行的应用状态，不能直接用切断采音的 deep sleep 替代。参见 [wake-word.md](../wake-word.md) 与 [behavior-lifecycle.md](../behavior-lifecycle.md)。

### 6.3 声纹已经有 S31 官方模型，但不是瞬时身份识别

`speaker_verification` 0.3.1 明确支持 S31，接收 16 kHz 单声道音频，提供 3 秒和 6 秒两种窗口。[S10]

| 模型窗口 | S31 前处理 | S31 推理 | 官方 Vox1-O-clean EER |
| --- | ---: | ---: | ---: |
| 3 秒 | 84.1 ms | 202.5 ms | 4.51% |
| 6 秒 | 159.1 ms | 388.6 ms | 2.19% |

同样采用 CPU 320 MHz、PSRAM 250 MHz、int8 和性能优化编译。以上推理时间**不包含等待收集足够语音的 3/6 秒**。短输入虽可填充，不能据此假设短唤醒词也有相同识别质量；EER 是该测试集的阈值交点指标，不是家里孩子、多人同时讲话或扬声器回声条件下的正确率。

建议把声纹作为身份的补充线索，结合有效人脸证据；身份不确定时保持“未知”，不把它作为是否响应唤醒的前置要求。

### 6.4 中文语音合成与语音播放

ESP-SR 的中文 TTS 有 S31 库及中文拼接式、16 kHz 流式合成文档。它可以支持本地中文提示和一些动态文本，但不能据此承诺自然多语种、富情感或特定人物音色。[S11]

对“小呆小呆”后的短应答，优先考虑本地音频资源：语气可控、响应路径短、断网可用。预置文本若只有字符串而没有本地音频资源、可用的本地 TTS 或已完成的其他合成路径，不能算作已实现“开口回答”。一般问答可接云端流式 TTS 或保留 Mac 本地 TTS 选项。

### 6.5 全双工、打断和声源方向需要整机验证

官方 AEC 有全双工模式，ESP-WebRTC 的发布记录明确增加了 S31 支持；这些是实现基础。OpenAI 等演示的默认板配置不一定是 S31，不能称为我们已经验证过的同板实时语音。[S16][S17]

小智 issue #2279 给出了同板证据：ESP-SR 2.4.7 的 AFE 出现噪声输出，升级至 2.5.5 后双向语音恢复。但“双向对话正常”仍不能替代高音量扬声器播放时的 AEC 效果、自然打断、误唤醒、远场和长稳验收。[S19]

DOA/GSC 官方接口也有 S31 支持。双麦方向是有限的声学线索，不是声纹或用户身份；不能把四麦阵列的官方指标直接套到 Korvo 双麦，更不能用声源方向独立触发会话。[S23]

## 7. 本地 Agent：复用 ESP-Claw，而不是把桌面 CLI 放进 MCU

### 7.1 Agent 的位置与模型的位置可以不同

ESP-Claw 的 `claw_core` 在 FreeRTOS 任务中组装上下文、发出 LLM 请求、解析工具调用、执行能力、追加结果并继续循环。它还管理会话、取消、记忆与事件。这证明“本地 Agent 调用远程模型”不需要 Linux、Node.js 或 Mac 常驻。[S14][S15]

需要保留的区别是：模型作出语义判断仍使用远程推理；板端 Agent 负责调用、流程与执行。确定性规则可以完全本地执行，例如音量、表情、停止播放、显示卡片，不必每次问模型。

### 7.2 可复用能力与尚需整合的部分

| ESP-Claw 已有基础 | 对小呆的用途 |
| --- | --- |
| OpenAI-compatible / Anthropic / custom endpoint | 模型后端适配，避免依赖某一桌面 harness |
| 工具循环、任务取消和事件路由 | 对话与设备动作的编排 |
| 本地结构化记忆或轻量 Markdown 记忆 | 小呆人设、偏好、有限历史 |
| HTTP、搜索工具、MCP client/server | 接入外部工具与其他设备 |
| Lua、LVGL、二维绘图、相机、音频录播模块 | 动态显示或受限设备能力 |
| `esp32_s31_korvo1` 板级目录 | 主板外设接入参考，需适配本项目 SC101IOT |

当前主要参考交互是 IM、Web 和串口；本次没有查到其完整“面对面唤醒 → 开放式 ASR → Agent → TTS”链路已经开箱集成。不能把上述能力表解释为刷入 ESP-Claw 就完成小呆所有交互。[S14][S15]

### 7.3 MCP 与浏览器的实际边界

ESP-Claw 的 MCP client 使用 HTTP transport，能发现和调用远程工具；其 MCP server 文档包含 SSE 和局域网 mDNS。小智设备则主要充当 MCP server，由后端 gateway 发起工具调用。这两种架构不要混淆。[S15][S18]

现有 Mac 上通过 stdio 启动的 Node/Python MCP 包、桌面 browser/Chromium、文件系统工具，不能因“支持 MCP”就直接在 S31 上运行。可以将相应服务留在可选 Mac 或远程节点，板端通过受限网络接口请求。普通搜索或天气 API 则更适合直接接入。

### 7.4 本地记忆不等于记忆内容从不上传

ESP-Claw 的记忆文件存于设备，但相关人设、历史、标签或召回结果会注入发送给云模型的上下文。`claw_memory_lightweight.c` 更直接将本地文本作为 system context。因此准确承诺应是“本地保存、按需选择发送的内容”，不能照抄“记忆永不离开设备”的宣传性说法。[S15]

## 8. 已知成功项目：值得复用哪些部分

### 8.1 小智 xiaozhi-esp32

确切 V1.1 板型支持已合入，包含 ES8389、双麦、双功放、800×480 LCD、触摸、按键、相机等。默认云端是 `xiaozhi.me`，使用官方托管服务时不需要个人电脑持续开机；但云端语音语义、模型和相关工具服务依然存在。[S18][S19]

适合复用音频流、唤醒、会话、断连恢复、设备 MCP 和专板处理。README 中的声纹功能不能单凭功能列表就判定其全部在板上运行；本地声纹的证据应来自专门 S31 模型组件。

### 8.2 ESP-Brookesia Chatbot

官方例程组合了 AFE/VAD/唤醒、表达动画、设置、Agent 切换和硬件工具，包含 S31-Korvo 的构建配置。默认交互为半双工，播放时可通过唤醒词发起打断；支持多后端不代表它已实现我们所有自定义身份、注视和本地 Agent 策略。[S20]

它适合借鉴语音与表情界面的组合，但当前 master 的 S31 SDK 要求与其他框架不同，应先锁定兼容版本。

### 8.3 ESP-Claw

它最有价值的部分是板端 Agent core 与 capability 机制，不是其默认产品界面。对于小呆，应保留既有 `bot_runtime` 行为主权与模块解耦，只把适合的 Agent 能力接入，避免第二套 runtime 同时控制相机、音频和表情。[S14][S15]

### 8.4 同板 Edge AI Vision Terminal

乐鑫社区收录的 xkk6663 项目在 `端侧人脸识别` 分支实现本地人脸识别、手势、云小智对话、图片/视频、录音播放和动态壁纸。作者提供同板源码与上机记录，证明此类组合具有现实可行性。[S24]

其重要取舍是 OV3660、按钮唤醒、模型按需装载、音频租约互斥以及单应用分区。它不能证明常驻唤醒、全帧识别、本地 Agent、全双工和全部媒体功能可同时满负载运行。

### 8.5 MimiClaw 的旁证

MimiClaw 在 ESP32-S3 上以 C 实现 Agent、工具与本地记忆，进一步说明 Agent 执行循环本身不需要桌面操作系统。其主要入口是 Telegram，模型仍在云端；没有核实到可直接替换本项目的 S31-Korvo 完整语音固件。[S25]

## 9. 图片、图表和“可视化回答”如何实现

建议把输出分成三种明确接口：

1. **本地表达**：表情、字幕、状态、倒计时、简单图形，直接由小呆 UI 渲染。
2. **结构化可视化**：模型返回有限的数据/卡片结构，由本地 LVGL 模板显示表格、柱图、天气或步骤。
3. **外部图像**：远程生成或检索图片，板端下载合适大小的资源，解码、缓存、显示。

ESP-Claw 的 LVGL/Lua 与显示模块已有图片、chart、canvas、table 和动画基础；官方板级 demo 也给出了显示和相机转换路径。[S12][S15]

下载一张图片与在本地生成图片是不同能力；提供供手机访问的 Web 管理页与板上运行网页浏览器也是不同能力。为任意 HTML/JS 建一个完整浏览器不适合作为这块板的默认可视化方案。优先使用受限场景数据和本地模板，避免把大段任意脚本引入日常对话路径。

## 10. 内存和并发：真正需要设计的约束

### 10.1 帧缓冲尺寸不能按 JPEG 案例估算 SC101IOT

以下是按像素格式计算的裸数据量，不是实测峰值：

| 数据 | 计算 | 大小 |
| --- | --- | ---: |
| 800×480 RGB565 一帧 | 800×480×2 | 768,000 B，约 0.73 MiB |
| 同屏双缓冲 | 上述×2 | 1,536,000 B，约 1.46 MiB |
| 1280×720 YUV422 一帧 | 1280×720×2 | 1,843,200 B，约 1.76 MiB |
| 720p YUV422 双缓冲 | 上述×2 | 3,686,400 B，约 3.52 MiB |
| 320×240 RGB565 一帧 | 320×240×2 | 153,600 B，约 0.15 MiB |

如果驱动另有备用 DMA 缓冲、颜色转换目标、缩放目标或图片解码缓冲，峰值还会增加；JPEG 帧大小随内容和质量变化，不能用固定像素字节数代表压缩帧。模型 arena、音频环形缓冲、TLS/JSON、字体和任务栈也必须计入。

### 10.2 建议的资源所有权与优先级

保持项目已有的单一硬件所有权：相机、音频、显示由平台和所属模块统一管理；Agent 调工具不能绕过 `bot_runtime` 自行抢占外设。

建议按以下顺序保护实时路径：

1. 麦克风采集、唤醒、播放和取消。
2. 必要 UI/表情与事件处理。
3. 低分辨率人脸位置更新。
4. 按需身份验证、图片上传下载和其他 Agent 工具。

优先级不是简单的“一个核音频、一个核视觉”固定分配：S31 的特定 SIMD/PIE 内核、DMA、共享 PSRAM 带宽可能有额外约束。应根据实际组件版本设置亲和性，避免音频 deadline 被视觉或网络处理拖延。[S02][S22]

身份模型按需执行、图库/媒体缓存设置上限、上下文有界、避免长时间保留大图，是合理的组合策略。最终要记录内部 RAM、PSRAM 的最小剩余量与最大连续块，不能仅看开机时 free heap。

## 11. 延迟：去掉 Mac 不会自动让所有回答变快

完整语音回答大致包含：等待说话结束、ASR、Agent 规划与工具回合、模型首段输出、TTS 首段音频、网络与播放缓冲。减少中转只影响其中一部分。

ESP-Claw 当前 OpenAI-compatible backend 的可见实现是收到完整 HTTP JSON 后解析回答；它不能直接作为已经具备低延迟流式语音的证据。默认工具回合上限也很宽松，不适合原样用于短问答。宣传中的本地毫秒事件响应，不是远程模型整轮完成时间。[S15]

建议将“用户叫我，我立即有反应”和“完成开放问题回答”分开：

- 唤醒成功后，本地同步执行睁眼、短应答和监听反馈。
- 固定设备命令、停止、音量、离线提示直接走本地规则。
- 开放问答采用流式 ASR/模型/TTS 或实时语音后端，并按语义完整短句播放。
- 工具回合有上限，取消要能停止在途模型请求和后续播放。
- 不为一句简单问候启动检索、复杂记忆提取或大量不相关工具。

在未取得这块板和真实网络的测量前，不承诺固定首音时间或给出“去掉 Mac 可以快多少”的百分比。

## 12. 时间、环境与内置应答：适合先落地的本地能力

以下为小呆的产品实现建议，不代表本研究已烧录验证。预置应答可以独立于开放式 Agent 工作，也不需要每次调用云模型。

### 12.1 只由明确互动触发

完整“小呆小呆”、有意触摸或其他明确人工动作可以触发应答；人脸出现、VAD 或“到了晚上”本身不触发主动说话。时间和环境用于选择说什么、用什么音量与表情，不用于绕过被动陪伴原则。

### 12.2 应答条目应包含统一语义

每条可包含：稳定 ID、中文文本、音频资源引用、适用时段、可选环境条件、表达/动作标签、权重、冷却时间及近期重复抑制。内容与表情渲染解耦，以便后续换眼睛主题。

| 场景 | 示例短句 | 表情意图 |
| --- | --- | --- |
| 早晨被叫醒 | “早呀，我在呢。” | 轻快睁眼、微笑 |
| 白天普通呼叫 | “来啦，你说。” | 抬眼、专注 |
| 晚间呼叫 | “嗯，我听着呢。” | 柔和睁眼、安静倾听 |
| 困倦状态被叫醒 | “唔……小呆上线。” | 先困倦、再清醒 |
| 网络或远端不可用 | “我在，不过现在连不上。先陪你一会儿。” | 温和、避免持续假装思考 |
| 停止请求 | “好，停下来啦。” | 收束动作、停止播放 |

这些是设计示例，实际内容数量和素材完成度以实现文件为准。时间未同步时选择中性应答，不伪造“早上/夜里”的事实；光线估计过期或摄像头关闭时使用“未知”，不从噪声图判断有人或没人。

### 12.3 离线语音与主题要能独立演进

离线应答采用真实可用的本地资源或已接通的本地 TTS。缺资源要有明确退化路径，不能把显示字幕当作扬声器已经发声。表达请求必须经过已有 runtime 仲裁，不能抢走正在进行的取消、设置或更高优先级提示；不得直接改动目标选择与瞳孔几何控制。

## 13. 可选部署架构与实施顺序

### 13.1 目标职责分配

| 位置 | 职责 |
| --- | --- |
| S31 实时交互层 | 本地唤醒、被动状态、音频采放、视觉位置、表情、字幕、预置应答 |
| S31 语义编排层 | 有界上下文、有限记忆、工具路由、请求取消、后端切换 |
| 云服务 | 开放 ASR/实时语音、通用 LLM、较强视觉理解、自然多语种 TTS、图片生成 |
| 可选 Mac | 现有 OpenCode、本地 ASR/TTS/身份模型、完整浏览器和桌面工具、调试及资源生成 |

“Mac 可选”是该部署目标，不是本文宣布当前代码已经切换。现有 Mac 路线的功能与配置继续保留，单板路线可作为另一个可选择后端。

### 13.2 优先实现顺序

1. **本地互动闭环**：完整四音节识别、有效唤醒事件、本地应答与表情、进入监听、明确取消。
2. **无 Mac 的语音基线**：复用同板官方/小智音频路径，完成云语音请求与板上播放，核对 SC101IOT 不干扰音频资源。
3. **板端 Agent 接入**：先少量设备工具与简短记忆，再加按需图片和外部查询。
4. **人脸与声纹增强**：登记、拒识、目标连续性、按需识别；保持未知状态可用。
5. **并发和长稳完善**：真实扬声器、网络故障、打断、夜间和高负载资源验证。

应选一个运行主体并抽取组件，不把小智、Brookesia、ESP-Claw、ESP-WHO 的整套应用同时堆入一个固件。相机与音频所有权、事件循环和 SDK 版本要在整合前对齐。

## 14. SDK、API 与套餐边界

### 14.1 版本必须组成经过核对的一组

- 小智的 S31 专板文档要求 ESP-IDF `release/v6.1` 或更新。[S18]
- Brookesia 当前 master/v0.8 对 S31 指向 IDF 6.2；其稳定 v0.7 对应的是另一 SDK 范围。[S20]
- ESP-Claw 的通用构建教程仍有 IDF 5.5.4 描述，而 S31 板配置中已有 IDF 6.1 的 workaround 注释。不能照搬通用入门命令作为 S31 完整构建依据。[S14]
- 小智同板 AFE 故障与修复说明 ESP-SR 版本会实质改变声音路径；后续 RISC-V 模型库还会改变固件体积。[S19]

应记录 IDF、BSP/Board Manager、ESP-SR、ESP-DL、GMF、LVGL 的实际锁定版本、编译选项与模型资源。模型可替换不代表精简后的固件一定包含其所有算子。

### 14.2 模型协议兼容与订阅额度可用性分别判断

ESP-Claw 支持 OpenAI-compatible/custom endpoint，并列出 DeepSeek。接 GLM 的通用兼容 API 有工程基础，但不能写成已经验证过当前 GLM 型号与本板的完整组合，也不能默认其已有与本项目相同的自动 fallback。

GLM Coding Plan 当前官方允许一组指定工具与产品，包括部分通用 Agent；因此“只允许写代码”并不准确。但其名单和使用规则不能自动扩展到 ESP-Claw 或自制机器人。**协议能接通不等于订阅权益可用于这个新固件。** 应使用明确允许自定义应用的 API/服务，或取得该使用方式的确认。[S26]

本文不因此改动现有 Mac OpenCode 默认配置；这里只说明未来单板接入需要独立核对的边界。API Key、endpoint、额度、语音/图像能力和超时策略都应属于可替换 provider 配置，而不是写死到表情或唤醒逻辑。

## 15. 仍需要在小呆上取得的证据

本研究没有执行固件构建、测试、烧录或模型调用；以下是后续实板验收项，不是本次交付声明：

- 单独“小呆”、普通对话、电视声与播放回声不被错误当成完整唤醒；完整“小呆小呆”在近场、远场、儿童声音下的漏识别与延迟。
- 夜间屏幕/视觉低活动时，麦克风与完整短语识别仍可用。
- 本地应答实际从扬声器发声，并与正确表情、监听和取消状态协调。
- SC101IOT 采集/转换、LCD、AFE、网络、Agent 的并发资源峰值及连续块余量。
- AEC、播放中唤醒/打断、高音量、噪声、多人的实际效果。
- 身份模型的登记/未知/拒识、错误身份纠正，以及人脸/声纹矛盾时的处理。
- 断网、云端超时、限额、重连和长时间运行的可理解退化行为。

官方组件、同板案例与本文架构判断共同支持继续实施；实际小呆的功能完成度应由对应代码、构建和设备记录分别说明。

## 16. 原始来源索引

以下均于 2026-11-13 调研。`master`、`main` 和 `latest` 页面会变化，实施时应锁定实际使用的提交和组件版本。

- **[S01] 开发板用户指南**：[ESP32-S31-Korvo-1 V1.1](https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32s31/esp32-s31-korvo-1/user_guide.html)。硬件、内存、音频、相机及 LCD 子板。
- **[S02] 芯片数据手册**：[ESP32-S31 datasheet](https://documentation.espressif.com/esp32-s31_datasheet_en.pdf)。CPU、片内内存、SIMD 与多媒体外设。
- **[S03] 官方 Board Manager 定义**：[esp32_s31_korvo_1](https://docs.espressif.com/projects/esp-board-manager/en/latest/references/boards/esp32_s31_korvo_1.html)。OV3660/SC101IOT 双相机配置。
- **[S04] 官方人脸检测模型**：[human_face_detect README](https://github.com/espressif/esp-dl/blob/master/models/human_face_detect/README.md)。S31 模型、分阶段时延、输入和条件。
- **[S05] 官方人脸识别模型与实现**：[human_face_recognition](https://github.com/espressif/esp-dl/tree/master/models/human_face_recognition)。MFN/MBF、模型时延、识别接口行为。
- **[S06] 官方视觉框架**：[ESP-DL](https://github.com/espressif/esp-dl)、[ESP-WHO](https://github.com/espressif/esp-who)。应分别核查模型 target 与上层示例板表。
- **[S07] 官方语音识别框架**：[ESP-SR](https://github.com/espressif/esp-sr)。WakeNet/MultiNet/AFE 的 S31 支持及版本历史。
- **[S08] 官方 S31 语音基准**：[ESP-SR benchmark](https://docs.espressif.com/projects/esp-sr/en/latest/esp32s31/benchmark/README.html)。WakeNet 资源与音频块处理时间；各准确率需查看其测试条件。
- **[S09] 官方唤醒词与模型请求**：[wakeword_list.md](https://github.com/espressif/esp-sr/blob/master/wakeword_list.md)、[自定义唤醒模型讨论 #88](https://github.com/espressif/esp-sr/issues/88)。专用“小呆小呆”模型不能由名称配置代替。
- **[S10] 官方声纹模型**：[speaker_verification 0.3.1](https://components.espressif.com/components/espressif/speaker_verification/versions/0.3.1/readme)。S31 支持、3/6 秒窗口、时延和 EER。
- **[S11] 官方中文 TTS**：[S31 speech synthesis](https://docs.espressif.com/projects/esp-sr/en/latest/esp32s31/speech_synthesis/readme.html)、[中文 TTS 库目录](https://github.com/espressif/esp-sr/tree/master/esp-tts/esp_tts_chinese/esp32s31)。
- **[S12] 官方相机与工厂示例**：[SC101IOT Kconfig](https://github.com/espressif/esp-video-components/blob/master/esp_cam_sensor/sensors/sc101iot/Kconfig.sc101iot)、[S31-Korvo factory_demo](https://github.com/espressif/esp-dev-kits/tree/master/examples/esp32-s31-korvo/examples/factory_demo)。
- **[S13] 官方板级音频支持**：[S31 Korvo BSP README](https://github.com/espressif/esp-bsp/blob/master/bsp/esp32_s31_korvo_1/README.md)。音频接线与采样配置注意事项。
- **[S14] ESP-Claw 框架与板级支持**：[主仓](https://github.com/espressif/esp-claw)、[S31-Korvo 专用目录](https://github.com/espressif/esp-claw/tree/master/application/edge_agent/boards/espressif/esp32_s31_korvo1)、[支持芯片与板](https://esp-claw.com/en/tutorial/supported-list/)。本次查阅主仓快照 `50072775cfa20d7ce25a9b27591d9d7c8b9dbab5`。
- **[S15] ESP-Claw 实际执行与资源能力**：[Agent Core](https://esp-claw.com/en/reference-core/claw-core/)、[Memory](https://esp-claw.com/en/reference-project/memory/)、[MCP](https://esp-claw.com/en/reference-cap/cap-mcp/)、[OpenAI-compatible backend 源码](https://github.com/espressif/esp-claw/blob/master/components/claw_modules/claw_core/src/llm/backends/claw_llm_backend_openai_compatible.c)、[Lua LVGL](https://github.com/espressif/esp-claw/blob/master/components/lua_modules/lua_module_lvgl/README.md)、[二维显示](https://github.com/espressif/esp-claw/blob/master/components/lua_modules/lua_module_display/README.md)。
- **[S16] 官方 AEC**：[S31 Acoustic Echo Cancellation](https://docs.espressif.com/projects/esp-sr/en/latest/esp32s31/acoustic_echo_cancellation/README.html)。
- **[S17] 官方实时语音示例**：[esp-webrtc-solution](https://github.com/espressif/esp-webrtc-solution)、[releases](https://github.com/espressif/esp-webrtc-solution/releases)。核查每个例程的实际 target 和板型。
- **[S18] 小智固件及设备协议**：[主仓](https://github.com/78/xiaozhi-esp32)、[S31-Korvo README](https://github.com/78/xiaozhi-esp32/blob/main/main/boards/espressif/esp32-s31-korvo-1/README.md)、[MCP 协议](https://github.com/78/xiaozhi-esp32/blob/main/docs/mcp-protocol.md)。
- **[S19] 小智同板支持及实板报告**：[专板支持 PR #2158](https://github.com/78/xiaozhi-esp32/pull/2158)、[S31 AFE 问题与修复 #2279](https://github.com/78/xiaozhi-esp32/issues/2279)。用户报告与维护者讨论，不是本项目实测。
- **[S20] 官方 ESP-Brookesia**：[主仓](https://github.com/espressif/esp-brookesia)、[AI Chatbot 例程](https://github.com/espressif/esp-brookesia/blob/master/examples/agent/chatbot/README.md)、[版本矩阵](https://docs.espressif.com/projects/esp-brookesia/en/latest/getting_started.html)。
- **[S21] 小智 MultiNet 自定义唤醒参考**：[custom_wake_word.cc](https://github.com/78/xiaozhi-esp32/blob/main/main/audio/wake_words/custom_wake_word.cc)。
- **[S22] 官方 GMF 音频**：[ESP-GMF](https://github.com/espressif/esp-gmf)、[gmf_audio README](https://github.com/espressif/esp-gmf/blob/main/elements/gmf_audio/README.md)。S31 优化及任务亲和性以对应版本说明为准。
- **[S23] 官方方向/波束形成**：[S31 DOA](https://docs.espressif.com/projects/esp-sr/en/latest/esp32s31/direction_of_arrival/README.html)、[S31 GSC](https://docs.espressif.com/projects/esp-sr/en/latest/esp32s31/gsc_beamforming/README.html)。
- **[S24] 同板综合实例**：[乐鑫社区介绍](https://esp32-s31.espressif.com/en/community/345)、[Harness_ESP32S31 人脸识别分支](https://github.com/xkk6663/Harness_ESP32S31/tree/%E7%AB%AF%E4%BE%A7%E4%BA%BA%E8%84%B8%E8%AF%86%E5%88%AB)。主分支用途已变化，应看所引用分支。
- **[S25] 其他 MCU Agent 实例**：[MimiClaw](https://github.com/memovai/mimiclaw)。S3 上的 C Agent/云模型架构旁证。
- **[S26] GLM 套餐与接入规则**：[BigModel 使用须知](https://docs.bigmodel.cn/cn/coding-plan/usage-notes)、[Z.ai Supported Tools](https://docs.z.ai/devpack/tool/others)、[Z.ai FAQ](https://docs.z.ai/devpack/faq)。工具名单、套餐权益与通用应用 API 分开核对。
