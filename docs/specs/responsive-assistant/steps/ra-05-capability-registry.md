# RA-05：能力目录、可信接纳与真实回执

日期：2026-11-25。状态：**待实现**。验证：**not_run**。

依赖：[RA-01 控制契约](ra-01-control-contracts.md)。不依赖 RA-03：目录、提案校验与操作账本是宿主侧组件，可像 `scene.rs` 校验四种任务提案那样，先对接现有 `fast_core` 输出路径实现与测试；RA-03 完成后再接入其 worker 边界。总议题：[#36](https://github.com/JasonGuoo/esp-bot/issues/36)。领域原则见 [capabilities.md](../capabilities.md)。审计基线为 `46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`；本次只写规格，不新增运行代码、schema 或形式模型。

## 1. 目标与边界

把“代码中有一个函数”变成“用户能表达目的、小呆知道此刻能否做、受控执行并说明真实结果”。本步骤提供统一目录、提案校验、短操作账本与 typed dispatch，供身份、记忆、提醒、设备及任务控制复用。业务实现分别在 RA-06/07/08；本步骤不引入新的 agent、工具运行时、任务调度器或任意 RPC 转发器。

目录既是模型的动作选择范围，也是对话自知的依据。正常闲聊、停止、补一句、设备固定本地命令不能依赖目录健康探测或动作提交完成。数据库被占用时，小呆可以立即说收到，但只有持久提交成功才可说已经接下任务。

## 2. 基线与文件落点

本表内 `brain/`、`memory/` 相对 `server/bot-server/src-tauri/src/`。

| 现有文件 | 已有机制 | 需要调整的责任 |
| --- | --- | --- |
| `brain/contracts.rs` | 现有 `SourceRef`、`RuntimeBinding` 和协议能力列表 | 复用 RA-01 新类型；不将协议协商能力当作产品动作目录 |
| `brain/capabilities.rs`、`capabilities_print.rs` | render/media/print 的依赖检测、执行、取消及效果证据 | 提供 typed 健康与 adapter 接口；复用现有有界资源与来源限制 |
| `brain/fast_core.rs` | 关键词和 `〔转交〕` 路由；core prompt 禁止工具 | 由 RA-03 产出结构化 `ActionProposal`；仅注入本 scope 的能力快照 |
| `brain/orchestration.rs`、`scene.rs` | 前台执行和部分 task proposal 接纳 | 统一经过可信 dispatch；保留既有 OpenCode 与 `agent.work` 执行路径 |
| `brain/rpc.rs` | 控制台管理 dispatcher、全局取消、`turn_gate.write()` 与来源清理 | 提取可复用的领域服务；对话入口不调用通用 dispatcher，不继承其全局取消路径 |
| `brain/tasks.rs` | 持久 task、事件、cancel/resume、effect 边界 | 长工作流仍由其拥有；操作账本只引用 `task_id` 与相关 revision |
| `memory/store.rs`、`mod.rs`、`additive_schema.sql` | SQLite 与增量表、事务入口 | 增加单一短操作账本与 outbox 接口；迁移方式需满足 RA-09 |

拟新增文件：

- `brain/capability_registry.rs`：能力描述、版本化健康投影、scope 过滤。
- `brain/capability_dispatch.rs`：提案校验、宿主上下文、接纳/执行/查询/取消结果映射。
- `brain/assistant_adapters/mod.rs`：typed adapter trait 和注册清单；具体业务模块由 RA-07/08 增加。
- `memory/assistant_operations.rs`：`assistant_operations` 的持久幂等与结果记录；不创建另一个任务 executor。

以上路径是本系列确定的拟议落点，当前基线不存在。新模块导出在已有 `brain/mod.rs`、`memory/mod.rs` 中接线；不是要求本次文档 PR 创建这些 Rust 文件。

## 3. 统一数据契约

### 3.1 模型只提出业务意图

```text
ActionProposal {
  capability_id,
  parameters,
  target_handle?
}

TrustedActionContext {
  request_id, service_epoch,
  source: SourceRef,
  actor_scope, policy_epoch,
  snapshot_version, catalog_version,
  foreground?: ForegroundRef,
  operation?: OperationRef,
  output_lease_id?
}

OperationRef { operation_id, operation_revision, task_id? }
```

完整定义归 RA-01；这里只说明能力消费方式。mutation/workflow 的 operation 必需；`query_snapshot/query_local` 的只读请求可省略，不能为了创建查询账本而阻塞查询。现有 `SourceRef` 字段保持兼容。宿主从真实输入、快照、权限与已接纳记录构造上下文。音频引用、硬件连接、scope、授权声明、管理确认、任意路径与幂等键不属于模型业务参数。

`target_handle` 是快照中受范围限制的引用，不能凭字符串解锁人物、任务或文件。执行器重新解析 handle 并验证版本。模型填入未知额外权威字段、伪造来源或旧 handle 时返回结构化拒绝；不能忽略这些字段后执行一个可能已改变含义的操作。

`ForegroundRef` 失效后，未接纳的旧提案不能执行。已经 `accepted_durable` 的工作由持久 `OperationRef`、来源/政策、任务版本决定是否继续；新聊天不自动作废它，旧前台输出许可也不会随之恢复。

### 3.2 能力描述与健康快照

`CapabilityDescriptor` 的字段与解释见 [能力契约](../capabilities.md)。本步骤还需固定以下规则：

| 状态 | 可执行行为 | 例子 |
| --- | --- | --- |
| `ready` | 当前 scope、依赖健康和输入前提满足；接纳时仍校验 | SQLite 可写、身份当前源有效，允许保存称呼 |
| `degraded` | 仅执行 descriptor 明示的有限子结果 | 可保存称呼，但声纹 worker 未就绪，声音绑定不可用 |
| `unavailable` | 拒绝新执行并解释缺项；保留状态/取消/恢复查询入口 | 打印机不可达、源已撤销、依赖初始化失败 |

`catalog_version` 标识发布的目录版本；单项 `health_generation` 标识依赖健康代次。探测在有容量限制的后台 worker 中执行，主循环只读已发布快照。必须设置队列、并发、探测截止时间、结果有效期和饱和降级行为，具体数值经实现测量后确定。

`execution_class=query_snapshot` 用于任务、身份、设备、提醒和能力状态等无副作用查询。它读取已发布且仍合法的有界快照，返回 `read_result` 与 `source_refs/snapshot_version/catalog_version/as_of/stale`，不分配持久 operation、不写幂等记录，也不等待数据库或远端刷新。缺项可触发受限后台刷新；现有合法旧事实标 stale，已撤权/删除的事实直接拒绝披露。查询自身只能证明观察到的事实，不能报告 accepted_durable。

`query_local` 用于 `clock.now` 这类有界、无数据库/网络 I/O 的当前宿主观察，同样只返回 read_result。当前时间从系统时钟读取，时区从合法配置快照读取；不能把旧快照的构建时间当作现在。此类查询不准夹带修改、加载模型或等待远端健康检查。

安装路径、配置开关、model 文件、Python 进程存在均不能独立证明 `ready`。对昂贵或无法无副作用验证的依赖，应明确 `checked_on_use` 的部分以及失败回执，不能把“尚未检查”显示为已验证。模型选能力与实际执行之间健康可能变化；重新检查失败返回 unavailable/降级，不让旧目录绕过条件。

### 3.3 短操作账本与长工作流

拟议 `assistant_operations` 至少记录：`operation_id`、`operation_revision`、宿主 `intent_id/idempotency_key`、capability/schema、规范化参数摘要、scope、来源与政策版本、接纳/更新时间、状态、结果引用、可选 `task_id`、重试策略及未知效果的核对依据。去重摘要不能包含原始音频或密钥；清理时来源正文可删除，必要的无内容 effect tombstone 按原政策保留。

必须有唯一约束保证同一意图只接纳一次。同键同参数返回原操作；同键不同参数返回 `idempotency_conflict`。模型重试、更换生成版本或网络重连不能制造新意图；用户明确再次提出同样动作可以产生新意图。去重键由 host 绑定已确认的请求来源/意图和能力，不由模型填随机值，也不能简单以文本哈希永久合并所有“提醒我喝水”。

账本是短动作的权威结果记录及跨库接纳桥梁，不承接长 workflow 调度。多轮补样、历史回查、OpenCode 工作、媒体处理继续进入 `TaskService`；其状态、attempt、输入版本和 checkpoint 在任务库。`assistant_operations.task_id` 是引用，不复制另一套长任务状态。memory 与 tasks 在不同数据库时采用 outbox/reconcile，禁止宣称跨库原子提交。

短动作如“保存称呼”应在业务事务内同时写业务结果和操作结果；提醒创建也需同样的原子关联。外部效果无法与本地 SQL 同事务时，先写可恢复意图，使用 adapter 提供的幂等或查询协议；未知结果进入核对，不许盲重试。

### 3.4 回执、生命周期与交付

写操作的 `OperationReceipt` 至少含 `OperationRef`、阶段、`reason_code`、可披露的领域结果、证据引用和后续允许动作。只读结果使用同一 RA-01 envelope 的 `read_result` payload，不要求 OperationRef；request_id 仍用于关联。所有业务事实由执行器/事务或合法快照产生，不采信模型自述完成。

| 回执阶段 | 触发条件 | 用户可见承诺 |
| --- | --- | --- |
| `received` | 请求已进入接收/校验路径 | 仅已听到/收到 |
| `read_result` | 查询读到仍合法的快照或有界本地观察，版本和观察时间可查 | 说明截至观察时刻的事实；不声称执行/提交 |
| `accepted_durable` | 可恢复意图、去重和相关 outbox 已提交 | 这件事已记下；若只提交转交意图，仍需说明实际执行待接纳 |
| `applied` | 具体业务提交或真实硬件应用已确认 | 已保存/已设置该项 |
| `cancel_requested` | 取消请求已持久接纳 | 正在取消；不代表外部效果已撤回 |
| `cancelled` | 执行器清理/终止或持久边界确认 | 该待执行工作已取消，并保留此前已发生效果 |
| `unknown` | 可能已执行但结果不明，需 reconcile | 尚不能确认结果 |
| `failed` | 已知失败且不会继续隐式执行 | 未完成，给出可执行恢复方式 |

`needs_input`、`running`、`partial` 是领域/工作流结果；`prepared` 表示产物准备；`displayed/played` 由输出尝试的设备回执证明。不能将收到请求、保存操作、产物完成和用户收到折叠成一个 success 布尔值。若只接纳了 memory outbox 而 TaskService 还未收录，不可声称 task 已处于 running。

派发完成仅投递 RA-01 事件与 RA-04 inbox/RA-06 投影，不直接调用 TTS。播放需要当前有效输出资格。即使 `applied` 回执在前台被打断后抵达，也不会自动恢复旧回答。

## 4. 详细实现清单

1. **冻结 v1 业务清单。** 对每项列出动作/工作流、输入/结果 schema、scope、前提、效果边界、query/cancel/supplement/resume/reconcile 支持、幂等模式；没有完整契约的模块不加入 ready 目录。感知流独立更新证据，不把每次声纹推理做成 LLM tool call。
2. **实现后台健康投影。** 复用现有媒体/打印检查与 worker 健康，合并为目录事件；只发布受当前 scope 和版本约束的不可变值。慢探测不占控制接收或 Luna 会话锁。
3. **创建单一操作账本。** 以事务/唯一约束落地幂等、参数冲突、receipt 查询、取消 CAS；明确来源删除后 tombstone 最少保留字段。新增迁移有故障注入与重跑验证。
4. **建立 host 绑定。** `request_id` 与真实输入来源关联；参数只按 schema 转换。audio、person 解析、任务版本与授权由宿主解析，拒绝模型伪造。非语音入口也必须有可信来源或版本化受限 trigger，不能用空家庭/假 turn 绕过检查。
5. **实现受限 dispatch。** 通过 enum/trait 显式注册 adapter，不接受任意 method string。接纳/执行 workers 与主循环分离，按能力设置有限队列、deadline 和资源配额；队列满返回可重试原因，不能悄悄丢请求或无限 spawn。
6. **移出管理 RPC 耦合。** 从现有 `rpc.rs` 抽取短事务领域入口和版本失效接口；控制台可复用它们。前台 adapter 不调用 `cancel_scoped(All)`，不持有 `turn_gate.write()` 等待 outbox、Graphiti、harness、模型或用户输入。原有删除源先提交、清理 outbox、fail-closed 的保障须保留。
7. **建立真回执到自然语言映射。** 提案阶段只允许非承诺 ACK；对真实 applied/unknown 等生成有限事实。原样复用同意图回执，不能为让对话顺畅而提升状态。
8. **接入状态、取消和恢复。** 只读状态走 query_snapshot，无需账本写入；通用变更入口负责操作级语义和 scope，任务控制委托 RA-06，业务续接委托 RA-07/08。已发生物理效果的撤销需要专门补偿，不以 cancelled 虚构回滚。
9. **提交对应形式模型和实现回归。** 此步骤未来合入运行代码时必须同时提交下节验证；不能等最后验收阶段再补控制边界。

## 5. 失败、取消与冲突处理

- 接纳 SQL 尚未提交就崩溃：无 accepted_durable；重启同意图可重新接纳。已提交但回包丢失：查询原操作，不能重复业务写入。
- 业务已执行、回执前断线：保留真实效果与 unknown/reconcile 状态；外部 adapter 若无法证明可重试，禁止自动重做。
- scope、来源或政策在校验后变化：在实际提交/外部效果授权点再次检查；撤销 fence 阻止新的效果。已经发生的效果可追加证据，但不能因此恢复旧输出权或被删正文。
- 取消与执行竞争：以 operation revision/CAS 与 adapter 效果边界裁决。取消早于效果可阻止执行；效果先发生则如实返回已应用/不能撤销，不能两边都报成功。
- 设备 Stop 或新一轮激活：只撤销旧 foreground/输出；不全局取消已接纳后台工作。明确 task cancel 才转发任务取消。
- 状态查询/撤销不依赖创建能力的当前 ready；即使声纹模型离线，用户仍能查看称呼或删除已有声音记录。
- 模型返回不存在的能力、超大参数、引用内容里的假指令或历史 snapshot：拒绝/澄清；本地 Stop 和普通对话继续。

## 6. TLA+ 与实现回归

拟新增 `formal/tla/macos/CapabilityExecution/{CapabilityExecution.tla,CapabilityExecution.cfg,checks.sh}`。至少使用两意图、两 scope、两政策/操作版本，允许依赖永久挂起、重复消息、崩溃恢复和取消/提交交错。以下名称是待实现性质，不是当前已经证明的结果。

| 待证明性质 | 对应实现回归 |
| --- | --- |
| `HostAuthorityOnly`、`NoCrossScopeEffect` | 模型伪造 household/source/audio/guardian、越范围 target、旧 handle，真实 dispatch 无效果 |
| `ReceiptRequiresEvidence` | DB 锁/提交失败/回包丢失，不能输出 accepted_durable 或 applied；硬件未确认不能说已调好 |
| `OneCommitPerIntent`、`NoBlindEffectRetry` | 同键重试与重启；同键异参冲突；外部结果未知时只核对、不盲重试。形式模型只在 adapter 明确幂等假设内证明效果去重 |
| `NoRevokedNewEffect`、`OldForegroundCannotAdmit` | 来源更正/删除、政策变化、前台替换后迟到提案，分别验证接纳与效果提交 fence |
| `CancelDoesNotEraseAppliedEffect` | 取消与事务/设备应用并发，原结果保留，cancel_requested 不被说成 cancelled |
| `DurableOperationSurvivesForeground` | 下一轮开始、Stop、断连只结束旧输出，真实 task/操作仍可查 |
| `ReadQueryRequiresNoDurableWrite` | DB 写入永久挂住时，“做到哪了”通过真实快照路径返回 read_result；撤权后的 stale 快照被拒绝 |
| 与 RA-01 `ResponsiveControl` 组合的控制进展 | 健康探测/DB/adapter 永不返回，控制事件仍被处理；饱和不无限扩容 |

公平性可以要求可运行控制事件被调度，不可以假设每个外部服务最终成功。实现测试必须经过真实 ledger/dispatch/队列入口，不能仅测试 enum 映射。保留现有媒体/打印来源与 unknown-effect 回归；不新增 CI。

## 7. 迁移与回滚

新目录和 dispatch 初期以开关接入，旧控制台 RPC 保持兼容；没有通过业务回归的 adapter 显示 unavailable。切换后管理入口与对话入口应共享领域事务和结果，不同时执行两条 mutation 路径。

关闭能力提案入口时，先从模型目录撤下新动作，停止接纳；仍保留账本查询、取消、来源清理、已接受工作流恢复和结果核对。不得删除操作表或复用幂等键，也不得因回滚重新运行旧动作。旧版本不能安全读取新 schema 时应停止写入并使用 RA-09 的版本检查，不能以降级启动绕开来源过滤。

## 8. 完成定义

- [ ] 当前能力目录有 schema、scope、ready/degraded/unavailable 依据和版本；文件存在与 ready 已明确区分。
- [ ] 只读状态使用 query_snapshot/read_result，不要求持久操作；freshness 可见，撤权事实不会用 stale 掩盖。
- [ ] Luna/OpenCode 只产出结构化业务提案，host 注入权威上下文；不存在任意 RPC/路径/命令透传。
- [ ] 短动作共用一个幂等账本，长工作流共用现有 TaskService；跨库 outbox/reconcile 可恢复。
- [ ] received、accepted_durable、applied、unknown、cancel 与交付回执不能互相替代，重启/重复事件后仍保持。
- [ ] 管理 mutation 已抽离为短事务/有范围失效；前台无全局取消、无长期 turn gate、无主循环等待外部能力。
- [ ] 查询、补充、取消、恢复在权限范围内可达，未支持的动作明确可见；设备固定本地命令保留且不双执行。
- [ ] 同一行为 PR 提交所需 TLA/配置/检查脚本和实现回归，记录实际执行与未运行原因；阈值/性能未经测量不宣称达标。

本页所有勾选项当前均待实现；文档审查不替代运行验收。
