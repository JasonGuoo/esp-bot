# 交互解耦与能力体系：研究和源码依据

查阅日期：2026-11-25。关联：[issue #36](https://github.com/JasonGuoo/esp-bot/issues/36)、[设计总览](README.md)。本文件区分上游事实、esp-bot 源码事实和本项目设计推论；没有执行框架对照 benchmark、模型推理或新的实板测试。

## 1. 选择标准与决定

用户要求主循环不等待任何后端，包括 Luna。比较标准是：决策是否可独立执行、慢工作是否可隔离、取消是否先影响输出、旧结果是否可识别、过载是否有边界、是否适合现有 Rust/Tauri/OpenCode 架构。

**决定：以 AIRI 的纯 Policy / Runtime 分层作为主循环设计参考；从 TEN 参考执行隔离，从 Pipecat 参考控制帧优先级。** LiveKit 作为完整语音会话和中断实现的对照。这里没有“速度排名”，也不建议整体导入另一套 agent runtime。

| 上游与核查来源 | 可确认的设计 | 对小呆的适用范围与限制 |
| --- | --- | --- |
| [AIRI voice-input ADR](https://github.com/moeru-ai/airi/blob/68b35a7b4e7f90fb5e3d6bcbe98c98b17c1cfa9f/docs/ai/adr/2026-10-16-voice-input-layers.md) | Capture、Signal、Policy、Runtime、Projection 分工；规则只处理事件与状态；慢结果携带读取版本；设备立即执行打断 | 采用决策契约及版本检查；ADR 自述迁移步骤 1～3 完成，不能把后续步骤当作现有产品保证 |
| [AIRI core-agent](https://github.com/moeru-ai/airi/blob/68b35a7b4e7f90fb5e3d6bcbe98c98b17c1cfa9f/packages/core-agent/README.md) | VoiceController、有序规则事件、命名响应中断，以及输入版本的有效性检查 | 借鉴输入/输出/规则边界；插件与最终处理仍可等待，整套 controller 不自动满足本项目控制循环要求 |
| [TEN 概念](https://theten.ai/docs/ten_framework/concept_overview) 与 [Extension](https://theten.ai/docs/ten_framework/software_packages/extension) | Extension Group 指定执行线程；扩展可把工作交给其他线程/进程，稍后回送消息 | 参考进程/线程隔离和消息返回；同组扩展共享线程，慢回调仍会阻塞该执行环境；不假设默认队列满足容量与截止时间 |
| [Pipecat frame processor](https://github.com/pipecat-ai/pipecat/blob/b1256409cd5e3d670465540f5d5c469b48d0e73b/src/pipecat/processors/frame_processor.py) · [官方源码文档](https://reference-server.pipecat.ai/en/latest/_modules/pipecat/processors/frame_processor.html) | 优先处理 Start/System 帧，再处理普通数据/控制帧；部分中断流程等待处理任务取消 | 参考优先控制消息；仍需自行约束有界队列、同步计算和取消清理。async 不等于独立线程 |
| [LiveKit voice API](https://docs.livekit.io/reference/python/livekit/agents/voice/index.html) | AgentSession 的 interrupt 返回代表中断完成的 Future | 参考发起中断与完成回执的分离；不能据此宣称所有 handler 都无等待，也不能断言框架的 LLM 会阻塞全部音频处理 |
| [Sherpaw speaker diarization](https://github.com/moeru-ai/sherpaw/blob/2b12da09ab0cf45894e9565c386bf003cf44a106/packages/speaker-diarization/README.md) | 单次分段标签不等于全局身份；流式重聚类可能修订旧标签；持久声纹需记录模型/版本 | 用于支持“证据可修订、姓名晚绑定”的数据原则；不是替换已有 ECAPA/voice_album 的决定，也不引用上游准确率为本项目准确率 |

2026-11-25 核查的 AIRI main 为 `68b35a7b4e7f90fb5e3d6bcbe98c98b17c1cfa9f`，Pipecat main 为 `b1256409cd5e3d670465540f5d5c469b48d0e73b`，Sherpaw main 为 `2b12da09ab0cf45894e9565c386bf003cf44a106`。LiveKit API 文档为当日查阅的在线版本，仓库 main 为 `606fb779e20c045229411d4ec4f7b4852b4a3461`；不假定在线文档与该 commit 完全同步。TEN 网站没有在本次获取可绑定的仓库修订，因此仅记录文档 URL 和查阅日期。

本次只引用设计与公开契约，没有复制上游实现或引入依赖。将来实际复用代码/模型时需锁定版本，并按项目复用目录记录对应许可证和适配边界。

## 2. esp-bot 当前源码观察

固定基线：[`46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`](https://github.com/JasonGuoo/esp-bot/tree/46ecdfbd99eba8e9158f8953b9ca251a45bc1d92)。下面的路径为工作树导航；发生后续变更时，用固定提交核对这次观察。

| 观察 | 源码入口 | 设计影响 |
| --- | --- | --- |
| 推理已在后台 task 执行 | [`brain/gateway.rs`](../../../server/bot-server/src-tauri/src/brain/gateway.rs)，接纳轮次后的 `tokio::spawn` | 问题不能概括成“Luna 直接跑在 select 内”；改控制等待和状态耦合 |
| 控制分支内仍等待准备/尾包/发送 | 同一 gateway 的 `prepare_failure`、`prepare_proactive`、`finish_plain_input`、`send`、`turn.task.stop` | RA-02 做逐项迁出；同步 SQLite/媒体处理也需审计，不能只搜索 await |
| 准备/发送配置允许秒级等待 | [`gateway/segments.rs`](../../../server/bot-server/src-tauri/src/brain/gateway/segments.rs) 的失败准备 4 秒、提醒准备 8 秒；gateway PCM/WS helper 为 3 秒，部分输出 channel 为 2 秒 | 这些是配置上限，不是测得延迟，也不能相加冒充端到端耗时 |
| 后台任务参与当前交付结束判断 | `gateway/segments.rs` 的 pending/tasks 条件；gateway 以 delivery/active 状态拒绝普通输入 | RA-04 必须同步建立独立结果收件箱，不能只删一个条件 |
| 快核心禁工具、通过标记转交 | [`brain/fast_core.rs`](../../../server/bot-server/src-tauri/src/brain/fast_core.rs) 的 `CODEX_EXTRA`、`ESCALATE_MARK` 与关键词路径 | RA-05 增加受控结构化提案；Luna 仍不获得任意 shell/存储权限 |
| 自然任务提案当前限四种 | [`brain/scene.rs`](../../../server/bot-server/src-tauri/src/brain/scene.rs) 的 schema 与校验 | presentation.render / media.prepare / print.submit / agent.work 之外的内部功能不能凭关键词视为已可调用 |
| 提醒有内部管理 API 与实际回执路径 | [`memory/proactive.rs`](../../../server/bot-server/src-tauri/src/memory/proactive.rs)、[`brain/proactive.rs`](../../../server/bot-server/src-tauri/src/brain/proactive.rs)、gateway | 提醒能力缺的是模型语义入口和统一契约，不是从零实现调度 |
| TaskService 已有持久化、状态与独立执行 | [`brain/tasks.rs`](../../../server/bot-server/src-tauri/src/brain/tasks.rs) | 复用任务表和执行器；不以有表推断所有重启任务可自动续跑 |
| resume 只接受 AwaitingInput；部分恢复为 unknown | `tasks.rs` 的 resume 校验与启动恢复 | RA-06 定义运行中改要求语义；RA-09 核对不确定结果，避免重放外部效果 |
| 登记和管理写入仍有宽泛对话门控 | [`brain/mod.rs`](../../../server/bot-server/src-tauri/src/brain/mod.rs) 的 enroll/turn gate；[`brain/rpc.rs`](../../../server/bot-server/src-tauri/src/brain/rpc.rs) 的 memory_changed | 对话 adapter 不直接套用全局取消；准备在后台，短事务提交，按依赖更新上下文 |
| 已有匿名声音组和后命名 | [`memory/voice_album.rs`](../../../server/bot-server/src-tauri/src/memory/voice_album.rs) | 保留 opt-in、generation、retention、单条人工标注优先，补工作流与自然入口 |

现有显式 supplement 会清理当前交付并进入补话；因此不能声称所有新输入完全不可用。现有任务结果/进度的部分准备路径也已经 spawn，ACK 预热入口主要为短锁与后台启动，不能将它们误判为同一类 inline TTS 阻塞。

Luna 工作进程内的会话顺序锁用于维护 thread、steer 和上下文重建关系。RA-03 将等待隔离在服务内部，并补独立取消/期限处理，不能粗暴去锁换取表面并行。

## 3. 从事实得到的项目设计

以下是本项目提出的后续实现要求，不声称上游已经提供全部机制：

1. 控制 reducer 不调用任何模型、存储或网络；有界 dispatch 不等待空位；关键停止使用独立失效路径。
2. 回答结果按请求/输入版本和输出许可筛选；持久任务结果按任务/结果版本保存，不能因前台 token 过期而整体丢弃。
3. 能力目录由真实 adapter/依赖状态生成；模型只能提出动作，可信宿主绑定来源并决定适用权限。
4. 身份是可修订关系；名字、声音确认和历史关联分别提交，原始来源与实际执行事实不因新名字重写。
5. 读模型为前台提供简短的新鲜状态，后台做索引和整理；来源撤回立即限制读取，后续完成清理传播。

## 4. 证据边界

- 本次验证的是源码和设计关系，没有复测现有 11 月 16～20 日验收结果。
- 框架官网/README 的可用功能不等于本机已配置、模型已加载或 S31 声学效果达标。
- 性能目标和资源上限在[验收规范](acceptance.md)中标为拟议目标；实现后分别测量本地反馈、控制尾延迟、首个有用内容和真实音频停止。
- 本次文档不复制实际会话、声音样本或人物私有数据；示例 Jason 是用户给定的设计场景。
