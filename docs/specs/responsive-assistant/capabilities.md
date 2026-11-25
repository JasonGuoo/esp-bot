# 个人助理能力契约与现状审计

日期：2026-11-25。源码审计基线：`46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`。总跟踪：[issue #36](https://github.com/JasonGuoo/esp-bot/issues/36)。入口：[总规格](README.md) · [实施计划](implementation-plan.md)。

**本文件是拟议规格，尚未实现。** 下文明确区分现有模块、已有入口和本系列需要补齐的用户能力。新增文件、类型、状态和验收案例均属于后续实现要求；本次文档交付不表示源码、TLC、模型、Mac 或 S31 验收通过。

## 1. 什么才算小呆具有一项能力

一项能力应从用户的目的出发，贯通：感知证据、意图理解、当前可用性、受控执行、持续状态、缺信息时协商、失败后恢复、真实结果回执，以及后续查询、更正或撤销。一个函数、管理 RPC 或已安装模型只覆盖其中一部分。

例如“记住，我是 Jason”需要把当前说话来源、名字、已有匿名声音关系和留存政策结合起来；`identity.enroll` 单独存在并不证明小呆能够完成这句话。“半小时后提醒我”需要真实存储、到期调度、设备接纳和播放结果，不能在模型说“好”时就算设置成功。

沿用既有 Rust/Tauri、Xiaodai core/Luna、OpenCode、TaskService、SQLite、模型 worker 与设备 runtime；本系列不引入新 agent/runtime，不把所有内部函数转换为 MCP，也不让模型直接调用家长控制台的通用 dispatcher。MCP 可以是对外服务的接入协议，内部可信宿主应使用 typed adapter。

能力应分成三类：

| 类别 | 运行方式 | 本项目示例 | 主要约束 |
| --- | --- | --- | --- |
| 感知流 | 受授权的输入持续产生证据，宿主处理事件 | ASR、说话人候选、设备播放结果、当前能力健康状态 | 不要求 LLM 每次调用；迟到结果不得重放动作或提升权限 |
| 动作 | 由明确意图触发、有界执行并返回实际结果 | 保存称呼、保存偏好、查询任务、设置音量 | 宿主校验来源/范围；数据库写入或硬件应用成功后才报告对应成功 |
| 持续工作流 | 等待计算、工具或用户补充，跨回合推进 | 声纹补样、历史归属核对、资料搜索与媒体准备 | 使用已有 TaskService 的持久生命周期；不占用前台回答槽等待完成 |

设备侧完整唤醒、停止、音量等已有固定本地命令继续保留。已由本地处理的同一操作不能再次作为模型动作执行；本地结果进入宿主事件和上下文即可。

## 2. 现有能力成熟度审计

审计等级不是功能完成百分比：M1 表示底层模块存在；M2 表示至少一个可信入口可调用；M3 表示自然语言到结果的完整场景成立；M4 表示该场景在指定配置/设备上实测。已有历史验收只能证明其记录的版本和场景，本次只审查基线源码，不能新增 M4 结论。

| 用户能力 | 基线已有证据 | 当前入口/限制 | 本系列补齐 |
| --- | --- | --- | --- |
| 快速聊天、短响应 | [`fast_core.rs`](../../../server/bot-server/src-tauri/src/brain/fast_core.rs) 有 Luna/HTTP 路线、句级冻结和 TTS 接线 | 普通回答存在；工具词预路由/`〔转交〕` 把部分请求交 OpenCode。core prompt 明确禁止工具与读写文件 | RA-03 接结构化意图提案；前台不等待慢能力，保持同一人格与上下文 |
| 能力发现 | [`contracts.rs`](../../../server/bot-server/src-tauri/src/brain/contracts.rs) 有协议能力；[`capabilities.rs`](../../../server/bot-server/src-tauri/src/brain/capabilities.rs) 有媒体/打印依赖检查 | 协议版本、文件存在和可执行路径不等于当前可执行；缺统一模型可见能力目录 | RA-05 建有范围、版本、就绪依据的目录，执行时再次校验 |
| 记住当前人 | [`memory/identity.rs`](../../../server/bot-server/src-tauri/src/memory/identity.rs) 有登记/匹配；[`voice_album.rs`](../../../server/bot-server/src-tauri/src/memory/voice_album.rs) 有匿名组、后命名、未来候选 | M1/M2：Mac 控制台可登记/改标；尚无“记住，我是 Jason”的受控对话工作流 | RA-07 将名字、声音绑定、历史关联分开推进，复用现有声音相册 |
| 查询和更正记忆 | [`household.rs`](../../../server/bot-server/src-tauri/src/memory/household.rs)、[`provenance.rs`](../../../server/bot-server/src-tauri/src/memory/provenance.rs) 和记忆 worker 有来源、修订、派生清理 | 召回已有；显式保存/纠错/遗忘主要是管理入口，不能证明口语指令会执行 | RA-08 增加有限、可解释的记忆动作；RA-06 提供来源有效的上下文快照 |
| 长任务委托 | [`orchestration.rs`](../../../server/bot-server/src-tauri/src/brain/orchestration.rs) 注册 `agent.work`；[`tasks.rs`](../../../server/bot-server/src-tauri/src/brain/tasks.rs) 持久任务/状态/取消/恢复 | 独立工作会话已存在；不等于所有自然语言请求都会进入后台，也不等于完成结果可持续投递 | RA-04/06/09 负责任务与结果投递；本页定义适配接口 |
| “做到哪了/别查了/继续昨天那个” | `tasks.get/list/events/cancel/resume` 已在 [`rpc.rs`](../../../server/bot-server/src-tauri/src/brain/rpc.rs) 暴露给控制台 | M1/M2：缺通用语义目标解析；前台停止、任务取消不能混同 | RA-06 解析任务目标，RA-05 dispatch 校验 scope 和 revision |
| 自然语言提醒 | [`memory/proactive.rs`](../../../server/bot-server/src-tauri/src/memory/proactive.rs) 有 add/cancel/list、持久到期状态与幂等播放结算；[`brain/proactive.rs`](../../../server/bot-server/src-tauri/src/brain/proactive.rs) 有安静时段等门控 | M1/M2：Mac API 基础存在；没有可靠的 model→typed reminder 桥。关键词“提醒我”转交不证明提醒已创建 | RA-08 实现明确时区、去重、查询/取消、真实保存回执；播放沿用现有门控 |
| 当前时间与时区 | `memory/household.rs` 有 IANA 家庭时区及来源，`brain/proactive.rs` 使用宿主时间计算本地时刻 | M1/M2：内部有时钟与时区逻辑；缺统一模型可用的当前时间查询契约 | RA-08 提供只读 `clock.now/timezone`，不使用旧 prompt 中的时间冒充现在 |
| 调整设备 | [`device_volume.rs`](../../../server/bot-server/src-tauri/src/brain/device_volume.rs) 有在线/连接、pending、`hardware_applied`、超时 | 本地固定命令和 Mac 控制已有；不能把发送命令当应用成功，也不能离线排队后意外执行 | RA-08 复用既有控制路径，补有限语义入口，保留本地优先 |
| 图文/媒体/打印 | `presentation.render/media.prepare/print.submit`、不可变 artifact、呈现与效果回执已有 | 有部分结构化任务提案链路；来源、媒体支持、真实显示/播放/打印仍须逐项判断 | RA-08 复用适配器；不能伪称具备任意网站视频流导出或物理打印成功 |
| 在做事时继续对话 | 有前台/任务分离的设计与部分实现 | WebSocket 接收、core 工作槽、状态获取仍需本系列的非阻塞控制契约验证 | RA-01～04/06，不能只以 `async` 函数存在打勾 |
| 自知与故障恢复 | status、模型 health、任务事件和错误已有 | 多个状态来源尚未形成统一的可供对话解释的能力快照 | RA-05/06/09：可用性、缺依赖、未知效果和恢复动作都有结构化事实 |
| 主动跟进 | 明确设置的提醒可以在未激活对话时播报；被动问候已限制 | 人脸/时间/回家并不授权问候或开麦，普通后台任务完成也不自动授权播报 | RA-04 待投递结果与 RA-08 提醒分开；遵守原有被动陪伴约定 |

## 3. 能力目录：发现的是此刻可用的产品动作

拟议 `CapabilityDescriptor` 由宿主组装，不由模型或远端 MCP 元数据赋权。每项至少包含：

| 字段 | 含义 |
| --- | --- |
| `capability_id`、`schema_version` | 稳定的业务动作和参数契约版本；不采用任意 Rust 方法名 |
| `kind` | `action` 或 `workflow`；感知流由独立事件契约声明 |
| `description`、`parameters_schema` | 用户目的、适用条件、模型可填写的业务参数；禁止隐含任意代码/SQL/路径执行 |
| `allowed_scope` | 当前家庭/私密范围、可用目标引用与调用者授权摘要 |
| `availability` | `ready`、`degraded`、`unavailable`，含有限 `reason_code` 与允许的降级结果 |
| `checked_at`、`health_generation`、`valid_until` | 就绪证据的新鲜度；版本变化使旧提案需要再校验 |
| `execution_class` | `query_snapshot` 只读合法快照、`query_local` 有界本地观察、有界本地动作、需设备回执动作、现有持久 task/workflow |
| `supports` | 是否可查询、取消、补充、恢复、补偿/撤销；不支持时明确返回 |
| `receipt_contract` | 哪个阶段能够承诺什么结果，需何种 effect/display/playback 证据 |

`installed/configured` 只可作为健康证据的一部分。模型文件存在、环境变量存在、Python 可执行、Graphiti 开关打开、打印机名字已配置，均不能单独给出 `ready`。就绪应考虑 worker/服务的近期健康、模型是否成功初始化、设备当前协商、数据库写入能力、来源权限与本次操作输入。耗时探测、模型加载和能力目录 warmup 在受管后台通道完成，不能每次提问串行执行。

`degraded` 必须指明仍能可靠做到的范围。例如声音模型不可用但本地数据库可写时，“保存称呼”可以执行，“以后靠声音认出你”不可宣称。旧健康结果过期后先降级或拒绝能力调用；普通聊天继续。目录中的就绪不是执行授权，执行前仍检查同一代健康与政策。

## 4. 模型提案、可信上下文和执行器的边界

外层动作/控制 envelope 以 [RA-01](steps/ra-01-control-contracts.md) 为准，Luna 生产方式以 [RA-03](steps/ra-03-luna-worker.md) 为准。本页只定义其中能力提案的业务 payload，不建立第二套 agent 消息协议。

模型可以提出类似以下的业务意图；这是拟议示例，不是基线已支持的 wire 格式：

```json
{
  "capability_id": "identity.remember_self",
  "parameters": {"preferred_name": "Jason"},
  "target_handle": "current_speaker"
}
```

`ActionProposal` 的字段固定为 `capability_id`、`parameters`、可选 `target_handle`；示例 handle 只表达当前输入中的说话者，不能自行指向任意音频。宿主构建 RA-01 的 `TrustedActionContext`：`request_id`、`service_epoch`、现有 `source: SourceRef`、`actor_scope`、`policy_epoch`、`snapshot_version`、`catalog_version`、可选 `foreground: ForegroundRef`、`operation?: OperationRef` 与可选 `output_lease_id`。mutation/workflow 必须绑定持久 operation；只读 query 可不分配。说话段与 `audio_ref` 另由宿主从有效来源解析后交给身份适配器。**模型不能自行提供这些权威值。** 模型复述快照中的任务或人物 handle 也只是选择提案，宿主需验证 handle 仍存在且在本次允许范围内。

复用当前 `SourceRef` 的家庭/日期会话/episode/turn/utterance/source_revision 字段，不另建同名类型。`ForegroundRef.input_revision` 是采集输入版本，`SourceRef.source_revision` 是持久来源修订；`OperationRef.operation_revision` 是操作版本，三者不互相替代。`EventEnvelope.event_id` 用于事件去重，可选 `request_id` 关联请求，二者也不混同。

三个边界必须同时成立：

1. **提案校验：**业务 schema、长度/数量边界、动作名白名单和来源有效性。模型文本“已记住”不能写数据库或构造成功事件。
2. **宿主接纳：**依据用户请求、当前授权和政策构造 typed command；写操作/工作流在短事务内分配/找到操作 ID，持久提交可恢复操作后才能发 `accepted_durable`。只读快照查询无需账本写入，直接返回 `read_result`。
3. **适配器执行：**直接调用指定业务服务，不经过 `BrainHost::dispatch(method, Value)`，不让模型选择 RPC method、任意文件或通用 shell。结果转为 typed receipt，再由前台组织自然语言。

现有 `rpc.rs` 中多种 `household.*` mutation 会 `cancel_scoped(All)`、取得 `turn_gate.write()`，再删除/刷新 harness 上下文。这是管理入口的保守协调方式，**不能原样包装为前台对话工具**。RA-05/07/08 必须先抽出短事务与可版本化失效接口；慢清理走 durable outbox，旧内容即刻 fence，不在持有长期 turn gate 时等待模型、网络或用户。

## 5. 回执不是一个 success 布尔值

操作执行状态与用户交付状态分开。以下是逻辑含义，具体 envelope 字段沿用 RA-01：

| 证据 | 已经证明 | 允许说的话 | 尚未证明 |
| --- | --- | --- | --- |
| `received` | 网关/宿主收到了请求 | “听到了。” | 已保存任务、已执行 |
| `read_result` | 查询读取了仍合法的快照，附 source refs、snapshot/version、as_of 与 stale | “截至刚才，图片已经准备好。” | 写入已接纳、比观察时间更新的事实 |
| `accepted_durable` | 操作或 task 的持久记录和去重键已提交 | “这件事记下了，我继续做。” | 动作完成、用户已看/已听 |
| `applied` | 具体业务事务已提交，或设备报告应用成功 | “称呼已保存。”“音量已调好。” | 任务结果已经播完 |
| `prepared` | 不可变产物已保存并可验证 | “图片已经准备好。” | 图片已显示或音频已播放 |
| `displayed` / `played` | 当前输出尝试对应的真实显示/播放证据到达 | “刚才已展示/播完这部分。” | 用户确实理解；其他未交付部分也完成 |
| `unknown`（含 `reason_code=effect_unknown`） | 动作可能发生，但无法确定最终效果 | “结果还无法确认，我正在核对。” | 可安全盲重试或可直接标失败 |

短本地动作可以在一次数据库事务中从接纳走到应用，但仍需持久幂等记录。慢工作立即返回 task/operation handle，完成事件不直接抢占播音；交付由 RA-04 的结果收件箱和当前输出许可决定。短 ACK 播完不推进业务任务到 `succeeded`。

适配器结果至少区分：`needs_input`、`running`、`applied`、`partial`、`unavailable`、`failed`、`cancel_requested`、`cancelled`、`unknown`。`prepared` 是产物领域结果，`displayed/played` 是交付证据；不能将它们混成业务操作的同一终态。任务生命周期以现有 TaskService 与 RA-04 为权威；此处的 receipt 阶段不能复制成另一个任务调度器。

## 6. 幂等、取消、查询、补充和恢复

- 幂等键由宿主从明确用户意图及接纳记录生成，不能让模型随机键决定去重。相同来源/意图重传与相同 payload 返回同一操作；同键不同 payload 返回冲突。用户在另一轮明确重新提出一次操作，可以获得新意图 ID。
- 能力操作有独立 `operation_id` 和持久 `operation_revision`。设备激活/前台轮次/生成版本与它们不同；新聊天、静音或前台输出取消不能自动取消已接纳任务，也不能使旧输出重新有效。
- 查询只读当前操作/任务快照，包含来源、snapshot/version、as_of、stale、实际阶段、可恢复性和未决问题。它不先写 `assistant_operations`，不等待数据库提交、不要求 accepted_durable，也不可再次执行工具。合法但不新鲜的事实如实标注；已撤权/删除的事实不能仅标 stale 后继续披露。
- 取消是带目标及期望版本的请求。返回“正在取消”直到相应执行器或持久状态确认；已经发生的物理效果不能通过修改状态抹掉。只想停止说话时保留任务。
- 补充应绑定明确的 `needs_input`/任务目标；新输入是新来源，不能冒充原话。无明确目标时用 RA-06 的有界消歧，不按“最近一个任务”静默选择。
- 可恢复操作必须在源有效、政策未撤销、依赖恢复后继续。执行结果未知时先 reconcile；数据库失败、进程崩溃或断线后的重复回包不得重复外部效果。
- 无相关持久化权限时，只提供明确的本次临时能力，不把内存中的记录称为可跨重启任务。自动 history 关闭与用户 explicit remember 分开：既有政策允许时可以最小保存明确指定的事实，不因而保存全量对话；不能为了完成能力偷偷开启 history 或 voice review。

## 7. 首批适配器范围

| 拟议业务能力 | 复用服务 | 首批结果与边界 |
| --- | --- | --- |
| `identity.remember_self/status/correct/forget_voice` | `memory/voice_album.rs`、`identity.rs`、`household.rs` | [身份连续性规格](identity-continuity.md)；只把名字绑定到可信当前来源，名字/声音/历史分别回执 |
| `memory.remember/recall/correct/forget` | SQLite 来源、现有 memory/household、Graphiti 派生索引 | 说话者和事实主体分开；明确范围；遗忘先 fence 派生物，清理可恢复 |
| `reminder.create/list/cancel` | `memory/proactive.rs` 与现有 gateway/设备提醒路径 | 首版一次性提醒；时区、日期和文本可核查。`reminder.update` 与循环提醒首批 unsupported，不假称已修改 |
| `clock.now/timezone` | 宿主当前时钟、已发布的家庭 IANA 时区与来源 | 纯查询，不写操作账本；当前时刻与时区来源可解释，时钟异常不伪装精确可靠 |
| `device.status/volume` | `DeviceVolumes` 与设备固定本地控制 | 在线、当前连接、范围和 hardware-applied 校验；不建立离线设备命令队列 |
| `presentation.show/replay` | 不可变 plan/artifact、既有输出 lease/回执 | 复用已有准备好的版本，不能用重播触发新的模型/工具副作用 |
| `presentation.render/media.prepare/print.submit` | 现有 `capabilities.rs`、TaskService、打印 adapter | 保留来源许可、幂等、效果核对；仅在实际支持时进入目录 |
| `task.status/cancel/provide_input/amend` | 现有 TaskService | 语义目标解析、运行中修改边界与语音入口由 RA-06 定义；`resume` 是已有 AwaitingInput 底层接口，不表示任意运行任务可恢复/改要求 |

提醒及结果投递沿用现有被动陪伴规则：未激活时只有家人明确设置的定时提醒可按已有门控播报；任务完成、识别人脸、日期、回家或习惯推断不会自动开启麦克风或问候。请求“做好后告诉我”可记录待投递意图；交付按 [RA-04 的结果交付体验](steps/ra-04-task-delivery.md#发言仲裁和通知政策)执行：默认只有静默表情提示和下次唤醒先交付，主动说出结果须家庭 opt-in 且走 `proactive.v1` 的 `task_result` moment，不能隐式获得无限播报权。

## 8. 错误与权限原则

能力要能说清“还缺哪一步”，但不向模型暴露模板向量、原始音频、密钥、内部绝对路径或任意数据库方法。`reason_code` 是结构化原因，不让 LLM从日志猜测是否完成。权限由既有宿主策略和明确授权决定；声纹候选、姓名、face track 或能力目录元数据均不赋予管理权限。

读取状态超时不等于服务失败；操作响应丢失不等于操作没有发生；模型不可用不等于设备本地停止不可用。对话控制和本地命令保留独立通路。耗时模型、Graphiti、ffmpeg、打印和历史回查共享受限资源时，不能抢占前台控制接收及首句 TTS 的必要资源。

## 9. 必须证明的场景

| 场景 ID | 场景与期望 | 后续实现责任 |
| --- | --- | --- |
| CAP-01 | 模型文件存在但 worker 初始化失败：目录不是 ready，聊天仍能继续 | RA-05 |
| CAP-02 | 同一提案重传、重启后重传：只有一个持久操作/副作用；同键不同参数冲突 | RA-05/09 |
| CAP-03 | 模型伪造家庭、音频或人物 ID：拒绝权威字段，不产生效果 | RA-05/07 |
| CAP-04 | 提案后来源删除/政策变更：执行前被 fence，不能恢复旧来源 | RA-05/06/09 |
| CAP-05 | 保存称呼期间另一个任务进行：不调用全局取消，也不长期持有 turn gate | RA-05/07 |
| CAP-06 | 提醒重复、设置失败、时间歧义、取消与到期竞争：有一次真实效果和准确回执 | RA-08 |
| CAP-07 | 音量命令发送后断线：不能说已调好，重连不重放过期命令 | RA-08 |
| CAP-08 | 图片准备好但没有输出 lease：只入待投递，不声称已展示 | RA-04/08 |
| CAP-09 | 用户说“停一下”后后台结果到达：保持任务状态，旧播音不复活 | RA-01/04/06 |
| CAP-10 | 故事里出现“妈妈提醒我喝水”：不能创建提醒或修改记忆 | RA-03/08 |
| CAP-11 | DB 写入挂住时问“做到哪了”：直接返回合法查询快照与 freshness，不因记录 query 而阻塞 | RA-05/06/08 |

TLA+ 证明边界、有限状态和公平性由各步骤说明；真实自然语言理解、家庭声学准确率、首音延迟、模型质量需要对应模型/设备实测。阈值和延迟预算属于待校准参数，不填写未经测量的达标值。项目保持 No CI。
