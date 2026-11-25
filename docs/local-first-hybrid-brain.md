# Mac 为主的家庭后端：部署与复用决策

**2026-11-25 增量设计，待实现：** [随时回应规格](specs/responsive-assistant/README.md)细化同一 Mac 后端内的控制/工作边界：Luna、OpenCode、模型、存储和发送均在交互控制循环之外；保留现有部署与模型栈。下文日期对应其原始决策，本次不将方案写成已安装能力。

**当前选择：2026-11-16。** 用户已明确当前依赖 Mac，Bot 只连接配对的本地后端，不在本阶段开发独立联网 Agent。本页保留 Local-First 的设备离线互动和本机数据边界，更新原 2026-10-30 路线。总体权威见 [architecture.md](architecture.md)，详细设计和步骤见 [家庭陪伴规格](specs/household-companion/README.md)。

本次是设计修订。当前源码及真实运行范围见 [11 月 15 日验收](review-acceptance-2026-11-15.md)；[独立 ESP32 Agent 可行性研究](research/esp32-s31-standalone-agent-feasibility-2026-11-13.md)保留为研究，不再列为当前产品配置或开发门槛。

## 1. 当前部署分层

| 层 | 职责与采用方式 |
| --- | --- |
| ESP32-S31 Bot | ESP-IDF/BSP、单音频 owner、完整词本地唤醒、runtime、Face/attention、轻量媒体播放器及连接适配 |
| Mac Rust/Tauri core | 配对网关、输入来源、家庭日记录、权限、任务调度、呈现编排、状态与控制台 |
| OpenCode harness | 沿用官方 V2 的进程/API/会话/工具适配；当前活跃路线 GLM Coding Plan → DeepSeek Flash，Qwen 默认停用 |
| 本地模型通道 | 复用当前 ASR、ECAPA、YuNet/SFace、系统 TTS 适配；拆分 ASR 与身份执行槽，控制热模型与内存预算 |
| 数据与记忆 | SQLite 权威记录；后台增量整理；Graphiti v0.31.2 作为待评估的可重建图/检索侧进程 |
| 媒体和动作 | FFmpeg、确定性排版/绘图、既有授权搜索/媒体来源；AppKit/CUPS 等真实能力适配器 |

只有 Mac 负责互联网搜索、云模型调用和长任务。ASR/TTS/身份与存储优先本机，但“本地图索引”并不证明其 LLM/embedding 也离线，必须显式配置模型与外发范围。当前不用 HF Speech-to-Speech 或 Letta 替换已选择的 OpenCode 主引擎；它们的适配/后台整理经验可以复用。

## 2. 单一行为权威与离线能力

`bot_runtime` 控制激活、采集、输出许可、停止和行为优先级；`bot_bus` 运输事件/快照；`attention_manager` 选视觉焦点；`attention` 做空间规划；`face` 渲染；`head_motion` 只报告真实已安装能力。Mac 不能绕过 runtime 逐帧驱动眼睛或电机。

无 Mac 时，完整“小呆小呆”唤醒、已有本地应答、表情与触摸控制继续工作。默认使用 MultiNet7 完整四音节命令，专用 WakeNet 模型仍是后续可选工作。缺少相应模型须明确报告，不用字符串配置冒充识别器。未唤醒的环境音不保存或上传；已唤醒也只在有效输入许可内采集。

复杂人脸/家庭身份与声纹在 Mac 处理。设备原有轻量视觉位置/眼睛跟随在预算内保留，音频和显示响应优先；无视觉证据时保持 unknown，不猜测固定头部已经转向某人。

## 3. 局域网与能力选择

1. Bot 发现之前配对的 Mac，验证已有信任材料后连接；mDNS 名称/UUID 不作为认证。
2. 当前 WSS v3 负责控制、文字与凭据；用户选择的逐轮 LAN PCM TCP 带完整性校验但不保密。新设计不改变既有音频传输选择。
3. Mac 根据已配置 provider、能力与权限选择模型/工具；保持现有 GLM → DeepSeek 路线及已输出/已执行工具后禁止重放规则，不把任意本地模型当作已集成替代。
4. 每个能力分别声明可用、预热、版本、超时和并发预算。ASR/身份并行，前台输入优先；后台任务可以继续，但未持有当前输出许可时只归档结果。
5. Mac 不可用或断线时及时退回本地交互；不把旧音频自动上传给另一个端点，不重放打印等物理动作。

只有 MAC 侧已配置的云策略参与模型路由。旧文档的设备 **Cloud Direct profile 从当前路线移除**；以后重新引入需要单独产品设计，不是本计划的隐含依赖。

## 4. BLE 与硬件是独立的后续工作

BLE 仍用于 Mac 配对、Wi-Fi 安全配置与低带宽控制的后续目标。优先复用 Espressif `network_provisioning` / protocomm 与 Mac CoreBluetooth，验证 proof-of-possession、凭据轮换和实际角色支持。它与已有局域网会话协议分层，不把配网安全机制误当业务 RPC。

压缩语音经 BLE 是测量后的可选研究，视频不作为 BLE 默认能力。芯片支持 Bluetooth 不证明 Mac 和固定 ESP-IDF 版本已经实现目标 HFP/A2DP/LE Audio profile。不要用未测的全双工蓝牙链路阻止当前 Wi-Fi 家庭主线。

硬件仍是 S31-Korvo-1 V1.1、实际 SC101IOT 相机、16 MB Flash / PSRAM，没有已安装的电机或 IMU。现成双轴机构、外壳、扬声器与电源走 [硬件采购/复用](off-the-shelf-hardware.md) 的独立流程；不得用软件生成假反馈。

## 5. 协议与媒体复用

继续使用现有 [WSS v3](../shared/mac-brain-v3.md) 和 [语音任务协议](specs/server-voice-tasks/protocol.md)，新增家庭、任务、素材和呈现契约先版本协商与旧端降级，避免每个能力新增一套通信栈。会话、激活、任务、播放和输出许可的独立标识见 [模块契约](module-contracts.md)。

音频使用有界帧、来源/序号、明确的采样范围和取消；不能把处理完成时间冒充硬件采样时间。媒体由 Mac 排版/缩放/转码，设备协商实际格式、尺寸、队列和同步能力。优先评估 Espressif ESP-GMF `esp_player` / `esp_video_render`，复用当前 LVGL 与 codec owner；上游可播放不等于本板在同时唤醒/相机运行时已达到某个 FPS。

## 6. 研发次序

新主线为：共同契约与家庭原始记录 → 并行理解和连续上下文 → 独立任务与真实应答进度 → 分段图文语音 → 后台记忆/家庭控制台 → 打印等动作。多人重叠、AEC 和高级视频单独验收，接口稳定后可并行开发。[HC-01～HC-15](specs/household-companion/implementation-plan.md)给出具体步骤与依赖。

当前原语音链路、104 本地语音、完整词模型和设备 WSS 均已有源码及不同范围的验收记录；新日会话/任务/呈现/图索引仍未实现。家长/成员模板代码存在也不表示当前环境已装齐识别依赖或通过家庭声纹测试。

## 7. 官方与成熟实现入口

- [XiaoZhi 的同款 S31-Korvo-1 板型](https://github.com/78/xiaozhi-esp32/blob/main/main/boards/espressif/esp32-s31-korvo-1/README.md)、[官方 S31 板说明](https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32s31/esp32-s31-korvo-1/user_guide.html)。
- [ESP-SR](https://github.com/espressif/esp-sr)、[ESP-GMF](https://github.com/espressif/esp-gmf)、[官方 esp_player](https://github.com/espressif/esp-gmf/blob/main/packages/esp_player/README.md)。
- [S31 BLE 文档](https://docs.espressif.com/projects/esp-idf/en/v6.1/esp32s31/api-guides/ble/overview.html)、[官方 provisioning](https://github.com/espressif/esp-idf/blob/master/docs/en/api-reference/provisioning/provisioning.rst)。
- [OpenCode compact](https://opencode.ai/docs/compaction/)；项目仍须按实际 V2 API 验证兼容。
- [开源记忆比较与固定版本](research/household-memory-open-source-2026-11-16.md)、[项目复用目录](upstream-reuse-catalog.md)。

复用之前核查对应版本、许可证与模型/资产条款。**不新增 CI，不把设计、模型下载或编译成功写成实板功能验收。**

## 8. Mac 内部隔离与能力入口 — 2026-11-25 目标

Mac 交互控制器采用事件与状态决策，外部工作通过非阻塞有界命令提交、带版本事件返回。保留独立模型 lane；同步 CPU/文件/数据库工作不进入控制执行环境，Luna 内部会话顺序仍由其服务管理。故障、过载或取消清理不能占住本地停止和状态反馈。

现有 TaskService、SQLite、voice_album、提醒和设备接口优先复用；统一的[能力服务](specs/responsive-assistant/capabilities.md)把模型结构化提案接到真实适配器，公布当前就绪状态与效果回执。MCP 可作为适配边界，但不会把所有内部函数直接开放给模型。

当前输出与后台任务分离，结果先进入持久收件箱，再按激活和输出许可交付。恢复未知效果时先核对，不能用重新运行工具代替恢复。十步变更与各项回滚约束见[实施计划](specs/responsive-assistant/implementation-plan.md)。
