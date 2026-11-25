# RA-09：迁移、重启恢复与回滚

日期：2026-11-25。状态：**待实现**。验证：**not_run**。

依赖：[RA-04](ra-04-task-delivery.md)、[RA-06](ra-06-context-and-task-control.md)、[RA-07a](ra-07-identity-continuity.md)、[RA-08](ra-08-assistant-adapters.md)。关联总议题：[同一总议题 #36](https://github.com/JasonGuoo/esp-bot/issues/36)。基线为 `46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`。以下全部是待实现规格；本次没有执行 SQL 迁移、启动模型、改变用户数据或运行 TLC。

## 1. 目标

服务重启后，小呆必须能够准确区分“已经保存的请求”“还在运行或等待确认的工作”“工作已完成但结果尚未送达”“曾经开始了外部动作但结果未知”。数据库提交、设备听见、外部动作完成是三个事实，不能互相代替。

恢复不得重新打开麦克风、重用过期输出租约、再次执行未知的打印任务，也不得从快照或身份分组中恢复已遗忘的信息。重启期间，设备本地停止和控制连接仍可用；存储尚未恢复的能力显示为不可用/恢复中，不拖住整个交互循环。

## 2. 已有行为与缺口

| 基线代码 | 已有保证 | 本步骤要补齐的保证 |
| --- | --- | --- |
| `server/bot-server/src-tauri/src/brain/tasks.rs::open` | SQLite `synchronous=FULL`；重启撤销旧 output leases；未结束 attempt 记为 interrupted；运行中的 render/media 可重新排队，其余运行任务转 `Unknown` | 对 operation、task input revision、workflow checkpoint、结果 inbox 和跨库 outbox 做一致恢复；不扩大“可重跑”范围 |
| `brain/tasks.rs::{submit,resume,cancel,begin_effect,reconcile}` | 接受任务在事务后发事件；幂等 intent key；打印前记录 effect 开始；打印有 reconcile；`resume` 不能恢复 Unknown | 为一般 adapter 声明安全重试/reconcile 策略；补上 operation 与 domain receipt 的关联 |
| `brain/tasks.rs::{forget_sources,invalidate_sources,purge_expired}` | 来源失效传播至任务、产物、呈现；effect tombstone 保留必要事实 | 将新 inbox、快照、工作流 checkpoint、操作日志的敏感载荷纳入相同传播 |
| `memory/household.rs`、`memory/voice_album.rs` | 来源版本、过期/遗忘、云权限、声音组 generation、手工归属和命名已存在 | 对迟到登记工作流及已确认绑定建立可恢复关系；不能靠重启猜测历史身份 |
| `brain/gateway/segments.rs`、`brain/gateway.rs` | 当前 Delivery 保存前台呈现与待跟进任务；有租约、实际播放回执 | RA-04 独立 inbox 在断线/重启后继续存在；重新投递需要当前可用的输出许可 |

特别说明：基线中运行的 `agent.work` 重启后可能成为 **`Unknown`**，并没有通用的可恢复 agent checkpoint。既有后台执行能力不等于运行中的 OpenCode 工作能从任意位置安全恢复。

## 3. 数据归属与版本

### 3.1 一个操作账本，若干领域事实

采用 RA-05/08 的统一 `assistant_operations`。它回答“哪个请求被接受、执行到了哪里、依据何种回执结束”。任务、身份、提醒等领域仍保留自己的权威状态，不为每个 adapter 再建一份通用操作表。

| 数据 | 权威存储/所有者 | 最小持久关联 |
| --- | --- | --- |
| operation | RA-05/08 的 `brain/assistant_operations.rs`（位于 `tasks.sqlite3`） | `operation_id`、`operation_revision`、host 幂等键、能力版本、scope、来源、状态、领域对象/回执引用 |
| task、attempt、input revisions | 既有 `tasks.sqlite3` / `TaskService` | `task_id`、`task_revision`、`input_revision`、attempt、`operation_id`、来源和 effect 证据 |
| result/needs_input inbox | RA-04，位于 `tasks.sqlite3` | `delivery_id`、`delivery_revision`、kind、task/result revision、recipient scope、result/source refs、送达状态与尝试过的 lease |
| identity 实体/绑定 | 既有 memory SQLite，复用 RA-07 的来源与声音组 | person/group/source 关系、身份版本、领域提交回执；绑定不是新的权限主体 |
| 长 workflow/checkpoint | RA-07 复用 `TaskService`，位于 `tasks.sqlite3` | task/operation 关联、阶段、workflow revision、input revision 与 attempt；checkpoint 只引用仍有效证据，不复制一份不可删除的原始音频 |
| reminder | 既有 `memory/proactive.rs` | 提醒记录、operation 引用、明确 UTC 到期时间及家庭时区语义、播报回执 |
| outbox/reconcile cursor | 与其事件来源同一个 SQLite 事务 | event ID、来源对象/版本、目标 consumer、最小载荷、ack/重试状态 |
| context snapshot、Graphiti、read model | RA-06 的派生投影 | 权限/来源/identity/registry 版本；可重建，不能作为事实的唯一副本 |

`SourceRef` 复用 `brain/contracts.rs` 的 household/day/episode/turn/utterance/source_revision 关系。`ForegroundRef`、`OperationRef` 和 `EventEnvelope` 复用 RA-01；本地 deadline、socket、runtime token、Luna thread handle 都不能当作可跨重启的持久运行凭证。

同库的 task 状态变化、冻结 result/needs_input 的 `result_revision` 与 inbox 插入必须在一个短事务中提交。需要补参的 inbox 不把任务变成终态。若 operation 在 memory 数据库，task 在 `tasks.sqlite3`，**不存在隐含的跨库原子提交**。采用本地事务 outbox、幂等消费、领域回执和恢复 reconciliation，不把两个顺序 `.commit()` 描述为原子。

### 3.2 接受和完成的语义

```text
控制层收到输入
  → 可以给出“收到，正在安排”的临时回应
  → 权威 operation + durable dispatch intent 提交
  → 请求已保存/可找回
  → 领域任务提交并收到 task.accepted 事实
  → 可以说“任务已接下”
  → adapter 真实工作及 effect 回执
  → result + inbox 提交
  → 当前输出许可 + 实际设备回执
  → 可以记录 delivered
```

以上阶段适用于 mutation/workflow，可在一个事务/快速调用内合并，但不能通过语气合并。`query_snapshot` 无需这条写入链：直接返回合法快照的 `read_result` 及版本、as_of/stale，DB 不可用时仍可报告合法的最近状态，不能披露已撤销范围。若 RA-05 的具体 adapter 在领域事务内直接写操作回执，以该事务为接受线性化点。若仅内存排队或将 JSON 写入 channel，不能承诺重启后可找回。`accepted_durable` 响应必须包含真实 `operation_id` 和已提交版本；任务 ID 只在真实任务存在后返回。进程崩溃前来不及给用户回执时，重试同键返回原事实，而不是重新执行。

## 4. 迁移设计与实施清单

### 4.1 准备与版本化

1. 在各 SQLite 库内引入显式迁移版本记录及最小 reader/writer 版本。先核对现有 open/schema 创建路径，避免依赖“`CREATE TABLE IF NOT EXISTS` 没报错”推断结构兼容。
2. 迁移在后台存储初始化阶段运行；在完成来源撤销/权限加载以前，不开放敏感上下文、身份修改或外部 effect 能力。控制服务可以给出“正在恢复”，不能等迁移结束才处理 Stop。
3. 以添加字段/表和可重建索引为主，避免覆盖旧 payload。为旧 task 设置明确的初始 `task_revision/input_revision`，不伪造过去的修改版本或播放回执。
4. 明确每一步迁移的开始、提交和重入条件。失败/磁盘满/进程中断后，旧版本可读部分或已提交新版本必须可识别；不得“迁移失败后仍以 ready 继续运行”。
5. 临时备份如需要，只用于同次受控迁移恢复，并遵循现有保留/删除策略。不能用回滚旧备份复活迁移后已经删除的身份、记忆或隐私设置。

### 4.2 各类数据的具体处理

- **task input revisions：** 将旧 `input_json` 作为初始接受版本；已有 attempts 与当时已知版本关联，缺少信息时标记 legacy，不编造 lineage。新的 amendment 保留 source refs，不能复制已删除的内容进历史版本。
- **result/needs_input inbox：** 有可验证的结果或待补参问题、仍有效来源、明确 recipient scope 的任务可以生成唯一 `(task_id, result_revision, recipient_scope, kind)` inbox。没有完整旧送达证据时标为未确认送达；不得默认 delivered，也不得在升级后批量主动朗读所有历史结果。旧结果的首次展示遵守当前会话/用户请求与 RA-04 通知策略。
- **identity/workflow：** 现有 person/group ID 保留，姓名不是主键。同名不能 merge；已有手工片段归属优先。恢复的登记 checkpoint 只有仍保留、仍有授权的 source refs 才可继续；没有音频/embedding 证据时进入需要新采样阶段。
- **operation：** 不给过去所有 UI 变更伪造“从语音调用成功”的历史。仅为新的可恢复操作和必要 legacy 关联建立明确来源。幂等键及最小 effect tombstone 可以保留，敏感输入必须服从删除规则。
- **source deletion：** 将既有 memory cleanup 与新 operation/workflow/inbox/outbox 的来源引用接起来。旧 outbox 重放时也检查当前 tombstone/generation；不能先过滤任务表，随后又从 outbox 重新播出被删内容。
- **read model：** 缓存失效后从权威库重建。私域隔离、destination cloud 许可、过期和 source forgotten 过滤在发布前及消费前都成立；Graphiti 命中不是恢复已删除事实的授权。

## 5. 重启恢复状态机

### 5.1 启动顺序

1. 创建新的 `service_epoch`，撤销所有旧连接/前台 generation 的输出资格。设备保持自身唯一音频所有者；服务启动不触发录音。
2. 加载迁移状态、权限 generation、来源 tombstone、当前有效家庭设置。无法加载时敏感能力保持 unavailable，控制面可以继续响应基础状态。
3. 恢复并重放最小 outbox/撤销事件；幂等地对齐 operation 与领域事实。收敛完成前，不把有冲突的 operation 宣告 completed。
4. 对 task/attempt/workflow 按下表分类；只有声明可安全恢复且通过当前版本/权限复核的记录允许排队。
5. 发布首份有效快照和 readiness；恢复 task scheduler。inbox 可以恢复成 pending/deferred，但等待新的输出许可。
6. 后台逐步 reconcile 未知的外部状态。恢复完成不是必须等待所有外部服务恢复；Unknown 要能继续被查询。

### 5.2 恢复决策表

| 崩溃前持久事实 | 恢复后动作 | 禁止的推断 |
| --- | --- | --- |
| 仅内存接纳，无 committed operation/task | 不生成成功任务；客户端可用原请求身份重试 | 不说“已经帮你保存” |
| operation + dispatch intent 已提交，domain task 尚未提交 | 幂等派发并对齐领域回执；排队容量/readiness 不够时保留真实等待状态 | 不在每次重启生成不同 task |
| task 已提交，operation 投影未更新 | 按 operation/intent key 对齐原任务，补发回执事件 | 不重复 submit |
| Queued/AwaitingInput | 来源/权限仍有效时保留状态和原 task ID；补参需当前有效输入 | 不要求原 activation 仍存在 |
| `media.prepare`/`presentation.render` 运行中 | 继续保留既有安全重排策略，并校验产物完整性、当前 source/input revision | 不把这条策略推广到所有能力 |
| `agent.work` 运行中，没有 adapter checkpoint/reconcile 证据 | `Unknown/recovery_required`；先查询上游 session/已记录 effect；无法确认则报告不确定并提供重新发起的明确选择 | 不自动把 Unknown 改回 Queued；不凭最后一句模型文本认定完成 |
| print 等 effect 已开始但无可靠 terminal receipt | 查询外部 job/operation ID；保留 Unknown/CancelRequested；外部确认后补终态 | 不自动重发打印，不把本地取消当作外部取消完成 |
| operation 正在取消 | 保持取消意图，重发幂等控制/查询，不重新执行原任务 | 不因 worker 不存在就认定 effect 未发生 |
| result + inbox 已提交，未实际送达 | 保留 pending/deferred，用户再次询问或合法送达时提供结果 | 不依赖旧 socket/旧 token 主动插话 |
| offered/partial，回执缺失 | 保留已确认的片段，尝试状态明确；根据用户意图和设备状态重试/继续 | 不把 offered 当 delivered，不把没收到回执说成“你已经听过” |
| 已有合法完整 delivery receipt | 维持 delivered；重复 receipt 幂等 | 不因重连重复播报完整结果 |
| 身份 checkpoint 的 source 已删除/过期或 generation 不符 | 失效 checkpoint，保留最小原因/操作回执，必要时请求新的有意采样 | 不从影子缓存恢复 embedding 或猜回旧人物 |

## 6. 断线、时间与输出许可

`service_epoch` 和 `connection_epoch` 变化后，旧 worker completion 可以用于核实其所属持久 task/attempt 的事实，但不能直接更新当前前台槽、发送音频或获得 lease。只有通过 task/input/source 版本核验的结果才可进入 inbox。旧回执不能驱动新的 followup；若回执属于仍可审计的旧投递，只可结算那次尝试，不能为新连接授予输出权。

单调时钟 deadline 仅用于当前进程的接纳、队列和局部超时。重启不能恢复旧单调时间戳，也不能用重启后的同一个数值延长旧租约。持久 TTL 和提醒使用明确 UTC 时间；家庭日界、展示和自然语言日期按家庭时区计算。午夜不取消尚在进行的任务，也不把昨天已结束的会话变成今天的录音授权。

断线期间结果可以完成并落入 inbox。重连协议先协商版本和能力，再查询当前 scope 的待送达结果。设备没有音频/画面能力或没有相应 receipt 协议时，选择已协商的降级呈现方式，不能伪造硬件播放完成。

## 7. 功能开关、协议协商与回滚

1. 为 control/gateway 新边界、独立 task delivery、能力目录、快照、身份工作流分别设有限且可观测的开关；具体配置名由实现 PR 定稿并记录默认值。不能允许“新快照读取旧隐私缓存”之类不安全组合。
2. 协议协商至少区分 input/foreground 生命周期版本、task notification/inbox 支持、结构化操作回执、分段与硬件 receipt 支持。未知字段可忽略的前提是语义兼容；未知关键能力必须拒绝/降级。
3. 旧固件可以留在兼容模式，但界面应显示限制。若旧协议仍把 task 生命周期绑在当前呈现，它不算本方案“长任务期间继续聊天”的通过对象。
4. 关闭新能力入口只影响新请求。已持久化 task/operation 仍可查询、取消或 reconcile；不要因隐藏菜单而丢掉未完成的外部 effect。
5. 若新数据库格式不被旧二进制理解，旧二进制必须拒写并说明升级/兼容需求。支持回滚的实现必须先有桥接 reader/writer，不能直接用旧二进制打开新增语义的队列继续运行。
6. 回滚不撤销既有 effect、不恢复已删数据、不重新启用旧 output lease；操作和 tombstone 的格式兼容优先于功能开关。禁止把“恢复备份”作为普通撤回版本的默认操作。

## 8. 异常与资源清理

- outbox 消费失败保留 retry/错误状态，有限退避与容量上限；接近容量时拒绝新的耐久操作并给出准确原因。不得删掉尚未消费的 effect/撤销事件腾空间。
- SQLite busy、磁盘满、损坏、读写权限错误均在存储 worker 中处理；控制域不会同步等待 2 秒 busy timeout。无法持久化时不承诺接受。
- 身份、媒体或模型子进程卡死由其 worker deadline/终止策略负责；重启恢复不等待无法结束的旧 join。必须核对进程组和临时产物是否真实回收或隔离。
- 服务 Stop 撤销输出并保存可恢复状态；永久停止后台工作需要显式取消。设备局部 Stop 不等于删除持久队列。
- 清理工作只能删除确认属于该 operation/attempt 的临时资源，不能误删当前新 generation 正在使用的同名产物。

## 9. Formal 与实现回归

| 模型 | 本步骤需增加或复用的性质 | 对应实现回归 |
| --- | --- | --- |
| 拟新增 `formal/tla/macos/Recovery/` | 两库、outbox、重复消费、崩溃点、未知 effect；`AcceptedHasCommittedIntent`、`NoUnreconciledEffectReplay`、`NoLeaseSurvivesRestart`、`TombstoneDominatesReplay`。前述 replay 性质约束宿主可控制的提交；仅对有外部幂等保证的 adapter，在明示该假设时另查 `AtMostOneEffect`，不能推出打印/网络的物理 exactly-once | 在每个事务边界前后终止进程/关闭存储，重启重投同请求；真实临时 SQLite 文件与 adapter stub/打印证据，不仅测试纯状态函数 |
| RA-04 `formal/tla/macos/TaskDelivery/`（拟新增） | result/inbox 同事务、offered/partial/delivered 区分；重连重复 receipt 和旧 lease | task 完成与断线竞态、结果提交后响应丢失、partial 重启、旧回执重排 |
| RA-06 `ContextSnapshot`、`TaskControl`（拟新增） | 重启失效 snapshot；输入版本与 attempt 不串线；Unknown 不隐式恢复执行 | 跨午夜、source forgotten、改要求后崩溃、取消后旧 attempt 回写 |
| RA-07 `formal/tla/macos/IdentityContinuity/`（拟新增） | 绑定只来自仍有效证据；同名不自动 merge；恢复的采样不扩大原授权范围 | 已聊 30 轮后登记、登记中崩溃/删除、关闭声音收集后恢复、缺原始声学证据 |
| RA-05/08 `formal/tla/macos/CapabilityExecution/`（拟新增，在 RA-08 扩展） | 提醒 claim/取消/播放结算、设备当前连接的回执、记忆删除 fence 与恢复的一致性 | 提醒到点崩溃、旧设备音量回执、删除后 outbox 恢复；硬件 effect 仍需实机 |
| 既有 `formal/tla/protocol/TurnPlayback/` | 保留 `OldReceiptsAreInert`、`StoppedHasNoPlayback`、`FollowupHasEvidence`；显式建模新的 epoch 边界 | 断线前的 audio/end 回执晚到新连接，确保不打开新录音 |
| 既有 `formal/tla/firmware/ListeningGate/` | 保留 `OnlyIntentAuthorizes`、`OnlyFreshInput`、`RoomDoesNotWake` | 服务重启、任务完成、到点提醒不触发麦克风；仍需 S31 实板验证 |

新模型目录在本文件中只是计划。实现此步骤时，模型、TLC 配置和实际实现回归必须同 PR 更新；本次文档变更的 formal 影响判断就是以上映射。仅有内存 fake 的 crash 测试不足以证明 SQLite 的接受语义；仅有 TLC 也不证明操作系统 fsync/外部打印行为。

## 10. 完成定义

- [ ] 迁移版本、失败重入、旧数据初始化和最小 reader/writer 规则清晰；无破坏性自动降级。
- [ ] operation、task、result inbox、workflow 和来源的权威归属唯一；跨库 outbox/reconcile 有幂等键和实际回归。
- [ ] 每一个“请求已保存/任务已接受”的回执都能追溯真实提交；磁盘/DB 故障不会得到假成功。
- [ ] `agent.work` Unknown 和外部 effect 未知态有查询/reconcile 路径；无自动盲重放。
- [ ] 已遗忘/过期/私域不可见的内容不会从 read model、outbox、inbox、身份 checkpoint 或 Graphiti 重新出现。
- [ ] 断线、午夜、服务重启不授予录音或输出权限，送达状态遵循真实回执。
- [ ] 功能开关和协议兼容矩阵可操作；回滚不会破坏删除、取消、幂等或未完成 effect 的核对。
- [ ] 本步骤 formal、实现回归、Mac 与 S31 证据分别记录；当前记录仍是 `not_run`。
