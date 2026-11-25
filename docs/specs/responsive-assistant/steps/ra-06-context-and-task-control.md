# RA-06：每轮上下文与任务控制

日期：2026-11-25。状态：**待实现**。验证：**not_run**。

依赖：[RA-04 任务与投递](ra-04-task-delivery.md)、[RA-05 能力目录](ra-05-capability-registry.md)。关联总议题：[#36](https://github.com/JasonGuoo/esp-bot/issues/36)。本规格针对 `46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`；本次只提交设计文档，没有新增运行时模块、数据库结构或测试结果。

## 1. 目标与边界

小呆每次回答时，都能使用当前权限范围内的身份线索、相关任务、最近完成结果、待送达内容和记忆。读取这些信息不得等待数据库、Graphiti、声纹、任务执行器或 Luna 返回。某个任务在等待参数，不应使这个家庭的所有闲聊都改走 OpenCode。

用户能说“刚才那件事怎么样了”“取消那个搜索”“时间改成下午三点”“别做猫了，改成狗”。这些请求必须落到可追踪的任务控制操作；模型说出“改好了”之前，必须获得相应提交或应用回执。当前轮的补一句、停止播放、取消后台任务是不同操作。

本步骤复用现有 `TaskService`，不更换任务引擎，也不把核心模型的会话锁变成任务锁。RA-07 定义身份登记与历史归属的业务规则；本步骤只规定这些事实如何进入当前轮上下文、如何失效。

## 2. 基线与具体改动位置

| 位置 | 基线已存在的行为 | 本步骤改变的边界 |
| --- | --- | --- |
| `server/bot-server/src-tauri/src/brain/orchestration.rs::execute` | core 和 OpenCode 都得到家庭记忆、近期对话、当前可用的说话人注记；Graphiti 有有限等待；收集同日 `AwaitingInput` 任务 | 用同一个版本化快照构造两条模型路径的上下文；删除“任何待补参任务都迫使本轮走慢路径”的条件 |
| `brain/fast_core.rs::{route,context,mark_seen}` | core 已有完整/增量上下文；`route(..., pending_tasks)` 在参数为真时返回 `pending_task_input` | 该修复已前移到 [RA-00](ra-00-quick-decoupling.md)；本步骤在其上改为按快照中的任务指向决定路由；增量去重必须包含快照和各事实版本 |
| `brain/tasks.rs::{get,list,events,resume,cancel,progress,set_state}` | 持久任务、取消、事件、补参已存在；`resume` **只接受 `AwaitingInput`**，不能修改 `Running` 任务 | 加入任务控制版本检查、运行中改要求协议及与操作账本的关联；保留既有打印副作用边界 |
| `memory/household.rs::{household_context_items,household_recent_context,journal_identity_samples}` | 现有来源版本、私域、过期/遗忘处理；迟到身份不得覆盖更新后的来源 | 作为快照投影的权威输入；保持删除和来源更改传播 |
| `brain/rpc.rs`、`brain/scene.rs`、RA-05 的 `brain/capability_dispatch.rs` | 管理接口可查/取消任务，结构化任务提案可补参；尚无统一的对话任务控制面 | 对话、管理 UI 通过同一个服务入口读取结果/提交操作，避免各写一套状态规则 |

本表中 `brain/`、`memory/` 均相对 `server/bot-server/src-tauri/src/`。拟新增 `brain/context_snapshot.rs`、`brain/task_control.rs`、`memory/assistant_read_model.rs`。它们是实现落点建议，当前仓库没有这些模块。

## 3. 数据与接口契约

### 3.1 一个不可变的每轮快照

快照由后台投影器构建，通过不可变值发布。主循环和回答 worker 只读取已经发布的引用；不能在 `snapshot()` 内暗藏 SQL、文件读写、模型调用、异步等待或会竞争长锁的回调。

```text
ContextSnapshot {
  snapshot_version,
  household_id, household_day, timezone,
  scope, destination, policy_epoch,
  built_at_utc_ms, valid_until_utc_ms, freshness,
  current_person_evidence,
  relevant_tasks[], recent_completed[], pending_delivery[],
  memory_items[], recent_dialogue[], ready_capabilities[],
  task_projection_revision, identity_revision, catalog_version,
  source_refs[]
}

CurrentPersonEvidence {
  input_ref, identity_revision,
  state: unknown | self_named | candidate | confirmed_binding | ambiguous,
  person_id?, anonymous_voice_group_id?, display_name?,
  evidence_refs[], observed_at_utc_ms, valid_for_input_revision
}

TaskSummary {
  task_id, task_revision, input_revision, state, stage, event_sequence,
  capability_id, short_request, missing_fields?, result_revision?,
  recipient_scope, source_refs[], updated_at_utc_ms
}
```

`input_ref` 使用 RA-01 的 `ForegroundRef` 中的输入归属，不另建第二套 turn token。`ForegroundRef.input_revision` 只表示 capture/input 的本地版本；`TaskRecord.input_revision` 表示任务要求版本，两者不能互代。`source_refs` 复用既有 `brain/contracts.rs::SourceRef`，来源版本另用 `source_revision`。快照中的 `confirmed_binding` 表示有可审计的身份绑定事实，**不表示已获得个人资料权限或操作授权**。称呼偏好、自报姓名、声学观察和管理确认分别保留，不能合并为一个 `verified=true`。

版本的职责必须明确：

| 版本/标识 | 所有者与用途 |
| --- | --- |
| `snapshot_version` | 投影器发布一次完整快照时递增，用于解释本轮看到了什么 |
| `policy_epoch` | 权限/遗忘协调器的撤销栅栏；旧快照即使内容新鲜也不可跨过此栅栏 |
| `task_revision` | `TaskService` 的状态/控制 CAS；不使用每次进度心跳制造无意义的冲突 |
| `TaskRecord.input_revision` | 某任务已经接受的不可变要求版本；每个 attempt 都绑定一个版本 |
| `event_sequence` | 既有任务事件顺序，用于增量同步进度；不充当控制版本 |
| `operation_revision` | RA-05 统一操作账本的状态版本，表示一次调用的接受/执行情况 |
| `result_revision`、`delivery_revision` | RA-04 的结果和送达版本；“任务完成”和“用户已收到”分别记录 |
| `source_revision` | 既有来源版本；更正、删除、权限变更后不得继续使用旧投影 |

任务摘要至少覆盖：与本轮明确相关的运行任务、用户当前可见的待补参任务、近期完成但未读的任务、已完成且本轮被引用的结果。跨午夜任务保留原 `daily_conversation_id` 作为出处，同时可在新一天的授权快照内查询。不能只查“今天创建的任务”。摘要受条数/字节预算限制，超出时给出可查询数量，不把完整任务输入无限塞进提示词。

### 3.2 非阻塞与失效规则

1. 背景存储 worker 读取 SQLite 并构造基础投影；Graphiti 只补充已经通过当前来源权限复核的派生事实。Graphiti 延迟、失败、不可达不阻塞快照发布。
2. 当前输入的相关性裁剪在回答 worker 中对有限候选集执行。若需要新检索，提交后台刷新，本轮使用有标记的有效快照或明确表示尚未查到；主循环不等待。
3. 删除/撤权请求被接纳时，先在控制域提升撤销栅栏、阻止旧来源输出；后台完成真实提交后才能说“已删除”。提交失败时保持该范围暂不可用，不能为恢复响应速度重新放出旧内容。
4. 发布前复核构建起点与当前 `policy_epoch`、来源有效期及身份版本。过期构建丢弃重做，不能把“新快照编号”套在旧权限内容上。
5. 读取时检查当前撤销栅栏和 `valid_until_utc_ms`。存储卡住时允许缺项/降级；不允许使用已知无效、私域不匹配、来源已遗忘的数据。没有能够安全证明可用的快照时，使用最小无个人内容快照。
6. 晚到声纹或姓名只触发后续快照更新。本轮已经冻结/送达的文本不被改写，已执行工具不重放；下一轮不知道当前说话人时，不沿用上一人的身份。
7. Luna 的线程重用仍需处理 day、privacy 和 context revision。仅发送增量不足以撤回线程中的旧事实：删除、私域切换、来源更正需要失效/重建相关线程，并拒收旧 generation 结果。

### 3.3 同一套任务控制服务

通过 RA-05 的能力协议暴露下列入口。只读 `query_snapshot` 返回 `read_result`，不分配操作账本记录；mutation/workflow 返回统一 `OperationRef` / 操作状态。host 根据真实输入来源生成变更的幂等键，不接受模型任意指定跨轮复用的身份、授权范围或幂等键。

```text
task.status(task_id? | scoped_query)                  -> read_result(TaskSummary[])
task.cancel(task_id, expected_task_revision)          -> OperationRef
task.provide_input(task_id, expected_task_revision,
                   patch, source_refs)               -> OperationRef
task.amend(task_id, expected_task_revision,
           expected_input_revision, patch, source_refs) -> OperationRef
```

所有变更附 host 生成的 `idempotency_key`、调用来源、scope、允许的 effect 范围。`task.status` 快速返回有效投影，其 `read_result` 包含 `request_id`、`snapshot_version`、`source_refs`、`as_of`、`stale`；不写 `assistant_operations`、不等待 DB、不发 `accepted_durable`。数据库被锁时可明确报告“截至某时的最近状态”；需要权威刷新时异步返回后续结果。撤权/来源失效的数据不能仅加 stale 标记后披露，未观测的状态保持 unknown。查不到某任务不能披露“另一个私域存在此任务”。

任务修改操作使用 RA-05/08 的单一 `assistant_operations` 账本。`task_events` 和拟新增的不可变 `task_input_revisions` 保存任务历史，不能再创建一份与操作账本竞争的“通用 operation”表。操作库与任务库不在同一 SQLite 连接时，采用 RA-09 的 outbox/reconcile；不能宣称跨库提交是原子事务。待补参的问题也可作为 RA-04 `kind=needs_input` 的 inbox 项，沿用其冻结版本和送达状态，不把父任务伪装为 Succeeded。

### 3.4 状态与应用结果

| 当前任务状态/边界 | 允许操作 | 持久结果与用户可见说法 |
| --- | --- | --- |
| `AwaitingInput` | `provide_input` | 校验缺参、scope 和版本；沿用原 `task_id` 接受新 `input_revision` 并排队；收到提交回执后才说“参数已补上” |
| `Queued` | `amend` | CAS 提交新要求版本；调度器只领取最新已接受版本 |
| `Running`，adapter 有安全更新点 | `amend` | 记录 `amendment_pending` 的操作事件；worker 在安全点确认新版本，才回报 `applied`；此前只能说“已收到修改要求” |
| `Running`，adapter 只能安全重启且尚无不可重放 effect | `amend` | 取消旧 attempt，等待退出/隔离确认，再以原 `task_id` 的新 attempt 执行新版本；旧结果无法写回当前结果槽 |
| `Running`，已跨不可撤销 effect 边界或 adapter 不支持修改 | `amend` | 返回 `cannot_amend` 或 `requires_new_intent`，保留真实已有 effect；不得偷偷新建并再次打印/发出外部动作 |
| 任意非终态 | `cancel` | 先撤销相关输出资格，再持久化 `CancelRequested`；worker 清理完成/外部 adapter 确认后才到 `Cancelled`。取消请求提交不等于 effect 已撤销 |
| `Unknown` | 查询/reconcile | 显示“执行状态待确认”；除明确安全且获授权的恢复策略外，不调用 `resume` 重放 |
| 终态 | 查询 | 重复取消返回当前事实；重复同键操作返回原回执；不同请求若需重做，创建新的明确意图 |

冲突返回 `task_revision_conflict` 及当前允许披露的简要状态，让本轮澄清或重新确认；不得默默把旧 patch 套在新任务上。进度更新不能清除取消标记，也不能让过期 attempt 将新输入版本标为成功。真实外部 effect 的晚到证据可以追加到原 attempt，不能因此重新获得输出租约或覆盖当前任务要求。

## 4. 详细实现顺序

1. **列出投影依赖与预算。** 将 household、private scope、cloud destination、来源 TTL、身份 generation、任务/送达版本的读取和失效入口逐一登记。对快照条数、字符数、更新队列、刷新并发配置有限上界。配置上界要有饱和行为，不能仅有默认值。
2. **抽出只读投影器。** 从 `orchestration::execute` 提取同步 SQL/来源检查/家庭记忆组合，放进受限存储 worker。已有 SQLite 仍是权威；不为快照引入新的记忆事实数据库。发布不可变 snapshot 并记录 freshness 和省略原因。
3. **统一两条回答路径。** core 的 full/delta 和 OpenCode prompt 读取同一快照结构；权限过滤在构造上下文前完成。用版本化事实键处理去重，防止名字更改或任务状态变化被“已发送过”吞掉。Luna 线程每轮只收**增量**快照事实（变化的版本化事实），并设单轮硬字符上限，延续 Phase 4 将每轮增量上下文压到约 73 字符的成果；完整快照只用于新线程、HTTP 路线与 OpenCode prompt。需有稳态大小测试，证明多轮后每轮注入不增长。
4. **替换全局 pending 路由。** 删除 `pending_tasks: bool` 的全局慢路由含义；保留本轮确实在补参、控制任务或请求能力调用的明确路由。用户说“你今天开心吗”时，后台有十个待补参任务也不影响普通闲聊路径。
5. **实现任务选择。** 显式 task ID、当前被讨论任务和唯一明确指代可直接匹配；两个可能对象时提一个短澄清问题。多个待补参任务收到“三点”不能随机续跑。别把“补一句”手势的即时对话关系当作后台任务指针。
6. **添加版本化任务控制。** 在任务数据库增加/迁移 `task_revision`、`input_revision` 和不可变输入版本记录；将 claim、cancel、resume/provide_input、amend、finish 的条件更新串成一套 CAS。保持现有 `active -> db` 顺序，禁止 await 持有数据库/全局活动表锁。
7. **添加 adapter 控制契约。** 每种能力声明 `amend_mode`（不支持、安全点更新、安全重启）及 effect 边界。不能仅因所有 worker 有取消 token 就宣称运行中修改都已支持。
8. **用实际回执驱动回答。** 根据操作 `accepted_durable`、`applied`、`cancel_requested`、`cancelled`、`unknown` 等真实阶段生成短回应；状态变化推送到快照和 RA-04 inbox。等待提交的自然语言 ACK 必须与“任务已接受”不同。
9. **在本步骤提交 formal 与回归。** 完成下节模型、配置及实现回归后，记录可运行环境的执行情况；缺环境时如实保留 `not_run`，不能把 RA-10 当作本步骤缺失验证的补票。

## 5. 异常、取消与清理

- 快照刷新队列满：合并相同 scope 的刷新需求，保留撤销事件；不阻塞控制事件，不无限增加刷新 worker。
- SQLite 被长事务占用：后台刷新/变更按有限 deadline 返回 pending 或明确失败；主循环仍能处理 Stop、新连接、下一轮接纳。只读查询可返回仍合法的快照及 as_of/stale，不因写账本等待数据库；尚未提交的变更不得报告 accepted_durable。
- worker 忽略取消：旧 generation 先失效，按 RA-03/适配器策略终止或隔离；后台维护清理，不由控制域 join。释放 slot 必须以资源已退出或已隔离为依据，避免容量账目与实际进程数脱节。
- 删除来源与任务修改竞争：删除栅栏优先；即使 amendment 的模型推理早已完成，提交仍须复核来源。保留必要的 effect tombstone，不保留被删除的任务正文。
- 一轮中收到两次控制请求：按 task CAS 和操作幂等处理；取消已经提交后，旧“改要求完成”不得重开任务。冲突要有结构化原因和可见状态。
- 本地 Stop 只撤销当前听说/呈现资格；后台任务是否取消由明确的 `task.cancel` 决定。设置“别再说了”不能暗中删除正在准备的资料。

## 6. Formal 与实现回归责任

既有模型均不足以证明完整的任务控制或快照隔离。本步骤拟新增以下模型目录；**此文档 PR 不创建这些目录，不声称 TLC 已运行**。

| 模型 | 本步骤需表达的状态/性质 | 对应实现回归 |
| --- | --- | --- |
| 拟新增 `formal/tla/macos/ContextSnapshot/` | 两个 scope、两个 privacy generation、构建/撤销/发布交错；`NoRevokedSourceInPublishedView`、`NoCrossScopeRead`、`LateIdentityDoesNotReplayOperation`；刷新永不完成时，已接纳 control 仍可推进 | 卡住投影/DB、撤权后迟到发布、私域切换、来源过期、Luna 增量失效；不以假快照函数替代真实发布路径 |
| 拟新增 `formal/tla/macos/TaskControl/` | 两个 task、两个输入版本、cancel/amend/finish 竞争；`NoOldAttemptCommit`、`OneAcceptedVersionPerCAS`、`CancelDoesNotReopen`、`NoImplicitEffectReplay` | 对真实 `TaskService` 做同键重试、冲突版本、两个并发修改、旧 attempt 晚到、打印 effect 边界及取消清理 |
| 既有 `formal/tla/macos/ReplyTiming/` | 保留 `IdentityOnlyLabels`、`ProgressWithoutIdentity`、`SilentAfterStop`；加入快照未就绪但无私域内容的降级分支 | 声纹/检索永不返回时普通回答与 Stop 仍可推进；冻结句子不被迟到身份重写 |
| 既有 `formal/tla/protocol/Supplement/` | 保留 `PreviousQuestion`、`OnlyConfirmedSpeech`、`LateReceiptsStayRetired`、`ReservationSpent`；即时补充与任务参数补充必须不串线 | 触摸补一句后更改 task、跳轮/换会话、“三点”存在多个任务；已播放前缀仍准确 |
| RA-01 `ResponsiveControl`、RA-04 `TaskDelivery` | 快照/任务控制 worker 完成只投递带版本事件；control 不等待提交，旧结果不获得新输出权 | 后台修改/查询被卡住仍可停止及开始新一轮；同一结果按 inbox 规则投递 |

新模型的 fairness 可以要求可运行的控制事件得到调度，不能要求声纹、DB、Luna 或外部 adapter 最终返回来“证明”控制可用。TLC 的有限状态不代替快照容量、锁竞争和实际 SQL 路径的实现回归。

验收场景对应 [acceptance.md](../acceptance.md) 的上下文/任务控制组，并同时覆盖 RA-ACC-01–RA-ACC-10 的故障隔离场景。

## 7. 迁移、开关与回滚

数据库版本和跨库操作一致性按 RA-09 执行。旧任务增设版本时从明确的基线版本开始，不能推断不存在的历史 amendment。旧 `AwaitingInput` 可继续补参，但运行中修改只对声明支持的新 adapter 开启。

建议分别提供快照投影和运行中任务修改开关；两者均依赖 RA-05 的已验证能力 readiness。关闭运行中修改时从目录撤下该操作，保留任务查询、取消和已有待补参恢复。关闭快照路径时须失效 Luna 已注入的敏感上下文；回退旧路由不等于本方案验收通过。撤销栅栏、幂等键和来源过滤不能随功能回滚被关闭。

## 8. 完成定义

- [ ] core 与 OpenCode 使用同一份可解释、受预算约束的版本化快照；同步/异步调用图证明控制域不会等待其生成。
- [ ] 新鲜度、私域、来源过期/遗忘、云许可与身份不确定性都有结构化表达和实现回归。
- [ ] 等待参数的后台任务不影响无关闲聊路由；明确补参保留原任务 ID。
- [ ] status/cancel/provide_input/amend 通过统一能力服务可达，并由实际提交/应用回执决定回答。
- [ ] 运行中修改有 CAS、input revision、安全点/effect 边界；旧结果无法覆盖新要求，冲突不会静默丢失。
- [ ] 本步骤相关 TLA、`.cfg`、`checks.sh` 与实现回归在同一行为 PR 中提交；执行结果、未运行原因和硬件边界分别记录。

本规格完成不意味着上述勾选项完成；当前全部待实现，运行验证为 `not_run`。
