# RA-08：记忆、提醒、设备与结果交付适配器

日期：2026-11-25。状态：**待实现**。验证：**not_run**。

依赖：[RA-05 能力目录](ra-05-capability-registry.md)、[RA-06 上下文与任务控制](ra-06-context-and-task-control.md)。总议题：[#36](https://github.com/JasonGuoo/esp-bot/issues/36)。原则：[capabilities.md](../capabilities.md)。基线 `46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`；所有新增 adapter、参数和验证属于后续实现，本次无运行时变更。

## 1. 目标与首批范围

让现有模块形成用户能直接使用的个人助理能力：“记住我喜欢紫色”“你还记得什么”“现在几点”“明天下午三点提醒我”“取消刚才那个提醒”“声音小一点”“把刚才的图片再给我看看”。每项都要能说明当前可用性、执行状态和真实结果；缺信息时短问一句，执行期间能继续聊天。

本步骤复用现有 memory/proactive/device/presentation/media/print 与 TaskService，增加 typed 业务入口。身份能力归 RA-07，通用任务状态/取消/改要求归 RA-06。第一版提醒只含 create/list/cancel，`reminder.update` 与循环提醒明确 unsupported；也不因此承诺任意互联网视频下载、任意系统设置、任意第三方通信或新的硬件动作。

## 2. 现状与具体文件

本表内 `brain/`、`memory/` 路径相对 `server/bot-server/src-tauri/src/`。

| 现有文件 | 可以复用的机制 | 必须补齐/保持的边界 |
| --- | --- | --- |
| `memory/store.rs`、`household.rs`、`provenance.rs`、`household_jobs.rs` | 显式记忆、来源与人工确认、派生物、清理 outbox | 意图到限定领域事务；说话者与事实主体分离；撤销传播不依赖长 turn gate |
| `memory/proactive.rs`、`additive_schema.sql` | 一次性 reminder 的 add/list/cancel/due、claim、播放结算和重试 | 当前 add 使用新 ID，不能保证同意图重试去重；新增原子操作关联与查询投影 |
| `memory/household.rs`、`brain/proactive.rs` | 家庭 IANA timezone/timezone_source、宿主当前时间与本地时刻计算 | 形成不依赖写入的 clock 查询；不得每次查时间重读文件/SQL |
| `brain/proactive.rs`、`gateway.rs` | 只有明确提醒可未激活播报；quiet/busy/cap、admit 再检查、实际设备 outcome 结算 | 不扩大主动范围；settings、TTS、offer 接纳与真实 played 分开 |
| `brain/device_volume.rs` | 当前连接/状态新鲜度、settings/pending、hardware-applied、失联失效 | 模型入口复用路径；不离线排队，不以命令发送当应用成功 |
| `brain/presentation.rs`、`display.rs`、`media.rs`、`artifacts.rs` | 不可变 plan/artifact、revision/digest、output lease 与播放/显示 receipts | 展示/重播必须引用合法既有版本；不重做准备工具或放宽输出权限 |
| `brain/capabilities.rs`、`capabilities_print.rs`、`tasks.rs` | render/media/print handler、来源证据、effect 边界与核对 | 通过 RA-05 描述当前 ready，保留 TaskService 幂等/取消/未知效果规则 |
| `brain/rpc.rs`、`scene.rs`、`orchestration.rs` | 管理入口、部分结构化任务提案和工具委托 | 抽出 typed 服务；core 当前禁工具、提醒目前只有 Mac API，不能把已有关键词转交视为完整能力 |

拟新增 `brain/assistant_adapters/{memory,clock,reminders,device,presentation}.rs`；由 RA-05 的 `assistant_adapters/mod.rs` 注册。复用 `memory/assistant_operations.rs`，必要的 reminder intent 唯一键/来源关系增量加入现有 schema。具体迁移归 RA-09，不新增另一套 reminder scheduler 或通用 operation 表。

## 3. 通用契约

### 3.1 能力与效果边界

| 能力 | 输入由模型提供的业务部分 | host 绑定/校验 | 成功证据 |
| --- | --- | --- | --- |
| `memory.remember` | 事实、明确主体/范围意图 | 有效来源、允许主体 handle、policy、明确保存意图 | 事实与 provenance、操作结果同事务提交 |
| `memory.recall` | 有界问题或允许的目标 handle | 当前 scope、合法快照/来源、查询预算 | `read_result`，带 snapshot/version/as_of/stale |
| `memory.correct/forget` | 目标 handle、修正内容或删除范围 | 目标 revision、授权范围、source deps | 权威修订提交与立即 fence；清理进度另报 |
| `clock.now/timezone` | 当前或明确指定的合法时区意图 | 宿主当前时钟、家庭 IANA 时区/来源快照 | `read_result`，current UTC/local time、as_of、timezone/source 与可用健康信息 |
| `reminder.create` | 文本、日期/时间或相对时长 | 家庭时区、host 当前时间、允许收件范围与创建授权 | 唯一 reminder 持久提交；不代表到时播出 |
| `reminder.list` | 时间范围/目标 | 当前 scope 与快照 freshness | `read_result`；不写操作账本 |
| `reminder.cancel` | reminder handle | 目标状态与授权、当前 offer/输出资格及操作版本 | 取消状态提交；已播内容不可撤回 |
| `device.status` | 当前允许设备 handle | 当前连接/观察新鲜度 | `read_result`；标明实际观察时刻 |
| `device.volume` | 有限目标音量或相对调整意图 | 当前连接、上限、真实当前音量、pending/本地已处理去重 | 当前 connection 上匹配请求的 hardware-applied 回执 |
| `presentation.show/replay` | 已准备结果 handle、明确重播范围 | artifact/source revision、输出许可、范围 | prepared 与 displayed/played 分层，后者须设备证据 |
| `presentation.render/media.prepare/print.submit` | 既有 schema 支持的参数 | 来源、权限、依赖、artifact/effect 引用 | TaskService/adapter 的真实准备或效果回执 |

所有模型参数通过 RA-05 的统一提案与可信上下文，不额外建立一条工具协议。读快照是无副作用路径：不创建持久 operation，不等待 SQL 写入，也不把 `received→read_result` 描述成 accepted_durable。需要新检索而快照不足时，在后台刷新；当下返回已有合法事实与限制。

`clock.now` 使用 `query_local`，读取有界的本地系统时钟与已发布时区配置；`clock.timezone` 可读配置快照。不得等待网络校时、文件/SQL 读取，不得从旧 prompt 推算“现在”。返回时区和其 configured/system/fallback 来源；可观察到时钟跳变或时区未知时说明限制，不把 fallback UTC 说成用户所在时区。提醒接纳仍由 host 在实际提交时确认时间语义，不能把先前 read_result 当永久有效的时间授权。

写操作和 workflow 必须有持久操作和幂等；宿主生成唯一意图，模型不能伪造。`accepted_durable`、`applied`、`prepared`、`displayed/played` 不互相代替。操作结果投递到 inbox/快照，是否发声由当前输出策略决定。

### 3.2 ready 不等于可以立即交付

提醒创建服务 ready，不表示设备此刻允许发声；图片准备 ready，不表示有 output lease；音量 adapter 已注册，不表示当前设备在线。目录应分别呈现创建/准备能力和交付条件，用户不需要理解内部锁或实现脚本。

操作 deadline 到期、取消、设备断线或来源失效之后，回执可以补充发生事实，不能恢复旧 output lease。新请求需要新的当前授权；重连不会把历史音量命令或旧图片自动播放给设备。

## 4. 详细实现清单

### 4.1 显式记忆：可保存、可查、可更正、可忘记

1. 定义有限的 remember/recall/correct/forget schema。区分“记住我喜欢紫色”的明确请求、一般聊天提到偏好、故事引用和否定；显式记忆不能仅靠关键词生成。
2. 使用 host 当前来源与已允许的主体 handle。Jason 说“妈妈喜欢紫色”，source speaker、事实主体与当前授权是不同字段；不能把所有事实自动写到说话者 profile，也不能因声纹候选解锁私密范围。
3. 从管理 RPC 提取 typed 领域事务，复用现有 `save_memory`/household confirm/forget 语义和来源检查。业务提交与 `assistant_operations` 幂等结果同事务，冲突按目标 revision 返回。
4. 区分自动 history 留存与用户明确要求保存的最小事实。history 关闭不当然禁止所有 explicit remember；按现有授权允许时，仅保存该明确事实及必需的最小来源证明，不开启全量 history/voice_review。现有政策不允许持久写入时返回限制，不能偷偷留存原录音/全文。
5. recall 优先使用合法快照，标明不确定与过期；Graphiti 只作为经权威来源复核的派生检索，不能因图中有结果而恢复已删事实。
6. correct/forget 先使受影响上下文与派生读取失效，再提交权威更正与清理 outbox；事务未成功不报告已删除。慢 Graphiti/任务 artifact/harness 清理可恢复，不能在全局 turn gate 内等待它们。
7. 将记忆操作结果投影给两条回答路径；Luna 已有线程中的被删事实需要受范围约束的失效/重建，不能仅在新 prompt 附一句“忽略旧内容”。工具与已完成任务不因事实纠正重放。

### 4.2 提醒：从自然语言到真实持久计划

8. 明确一次性提醒 v1 的参数：文本、相对时长或本地日期时间、可选允许的接收范围。宿主提供当前时间、家庭时区，并将结果规范化为 UTC due + 原本地表示。简单明确时间直接执行；缺时区、日期歧义、夏令时无效/重复时刻或超出既有范围才询问具体缺项。
9. 复用当前创建授权。可信上下文已经允许创建时不重复请求确认；未满足现有权限时返回具体 needs_authorization。模型不能把自报姓名、声纹或 `guardian=true` 参数当授权。此规格不暗中扩大现有家长控制政策。
10. 抽取 `create_reminder_for_intent` 式 typed 事务，使 reminder 行、来源依赖与 operation result 有唯一关联。当前 `add_reminder` 每次产生新 ID，不能只在它外面先查再插；必须用事务/唯一约束关闭并发和崩溃窗口。
11. 提醒提交后回应规范化后的时间与内容；自然语言 ACK 先发出时只能说明收到。数据库写入失败、超出已有文本/日期范围或需要补参时，不能说已设置。
12. list/status 从 RA-06 快照读取。v1 cancel 复用现有 `state='pending'` 的事务条件更新，由统一 operation revision/幂等保护取消请求，并使未播放 offer 的输出资格失效；取消与 claim/播放开始交错时，发送前还须复核当前 reminder 状态和输出 fence。正在播放时按输出控制停止剩余部分，记录已发生的 played portion，不能假称从未提醒。既有 list 会执行 stale 过期清理，不能直接拿它当 query_snapshot 的同步读取函数；该清理留在后台投影/维护路径。
13. 复用已有 due、claim、过期/重试与 `settle_reminder_offer` 结算，以 reminder/offer/device/connection 等有效证据去重。TTS 完成、offer 发送或设备 started 不等于整条 played；gateway 的真实终态回执仍是播放事实来源。
14. 保留 quiet、每日次数、设备状态、音量与忙碌门控。后台任务完成不自动变成 reminder；人脸、回家、时间和习惯不授权问候或开麦。若未来改变 busy 中“后台任务忙”与“前台输出忙”的区分，需在同一行为 PR 中单独建模，不在本步骤顺手删除现有门控。

请求“改成三点”时，若明确指向 reminder，返回当前不支持直接修改，不能把 RA-06 的通用 task.amend 冒用为已支持提醒更新。禁止静默 cancel+create 伪装原子 update；只有用户另行明确选择取消旧项并新建时，才执行两个独立操作并分别给出真实结果，部分失败如实保留。未来实现 update 必须增加 reminder revision/CAS，以及旧 offer/played 回执不能结算新版计划的规则，另行进入能力目录与验收。

### 4.3 设备动作：保留本地路径与真实应用结果

15. status 读取设备已发布的观察；失联/过时明确报告，不能通过写 operation 等待设备上线。volume 的模型路径复用 `DeviceVolumes` 的范围、freshness、settings/pending 和 connection 校验。
16. 相对音量调整以 host 当前有效硬件状态计算；状态不新鲜时不能按模型猜测的 50% 做减法。具体上限继续用现有设备/家长策略，不把“再大一点”解释为绕过限制。
17. 固定本地命令继续由设备即时执行，不要求 RA-05 数据库或网络可用。本地已处理事件作为事实进入上下文，并带可去重关联；同一用户意图不能再从模型路径二次调整。Stop 是控制事件，不能被包装成需持久账本才能执行的慢动作。
18. 模型路径写入操作意图后发送受当前连接约束的命令；只有匹配 request/connection 的 hardware-applied 回执能判 applied。断线、过期或未知结果走 status/reconcile；不离线排队，不在重连时重发历史命令。

### 4.4 准备、展示、重播与外部效果

19. 将现有 render/media/print handler 描述接入目录，保留来源白名单、实际 webfetch/browser/search 工具证据、资源上限和媒体处理/打印取消。不能因通用 OpenCode 能访问网页，就声称所有网站的媒体都可获取。
20. show/replay 只接受合法快照中的不可变 plan/artifact handle；核对 revision/digest、来源权限和当前 output lease。必要的本地格式转换仍走已有受限能力，不能重执行原工具链或重新打印。
21. 结果已经 prepared 但设备无资格接收时，写 RA-04 inbox 并可被查询；不说“已经给你看了”。真实 displayed/played 回执指向当前 attempt 和 segment，部分播放不能结算为整个结果已送达。
22. 打印遵守既有 effect 边界。调度器接收 job 可以证明已提交，不能自动证明纸张已经打印出来；外部状态 unknown 先核对，不盲重发。更改要求与取消交给 RA-06 adapter 控制协议，不以新任务复制旧副作用。

## 5. 失败、取消与并发裁决

| 故障/竞争 | 所需结果 |
| --- | --- |
| 同一“提醒我”在模型/网络重试两次 | 同一 reminder 和操作结果；用户另一轮明确再设一次可以是新意图 |
| reminder 提交后回包丢失或进程退出 | 查询原操作/计划；不能重复提醒行 |
| cancel 与 due/claim/started/played 竞争 | 一个可审计状态序列；已播事实保留，未播部分不继续，重复回执不多结算 |
| Graphiti/SQLite 慢或断开 | status/Stop/闲聊仍可继续；读到的合法旧快照标 stale；写入只 received，不能虚报 applied |
| 询问时间时 SQLite/网络卡住或系统时钟跳变 | clock 查询不写账本、不等远端；返回当前宿主观察与已知限制，不使用旧对话时间 |
| 记忆删除与旧检索/旧线程响应到达 | fence 拒绝旧来源；不因到达较晚而重新发布内容 |
| 音量命令发出后设备重连 | 旧连接回执不证明新连接应用；不会重放过期命令 |
| 本地音量命令和模型提案重复 | 宿主确认已处理，报告实际结果，不再执行第二次相对调整 |
| 图片准备好但当前在听用户/已 Stop | prepared 保留，旧输出不启动；合规的下一次交付需要新许可 |
| print 发送成功但状态查询失败 | unknown + 可查询核对依据；不可自动再次提交以“保证完成” |

如果操作不可补偿，就说明已发生的效果；不要把数据库状态改为 cancelled 来伪造物理回滚。补充输入和恢复按 RA-06 的明确任务/operation 目标处理，不把任意后续一句话当作上次操作的补参。

## 6. TLA+ 与实现回归

本步骤扩展 RA-05 拟新增的 `formal/tla/macos/CapabilityExecution/{CapabilityExecution.tla,CapabilityExecution.cfg,checks.sh}`，使用有限 reminder、两设备连接、两来源/政策版本及重复回执；不另建一个 AssistantAdapters 模型。新增领域副作用状态与配置，通用调度/播放规则引用 RA-04/01 与既有模型的分工，不复制成第二个任务系统。

| 待证明性质/模型分工 | 对应实现回归 |
| --- | --- |
| `ReminderOneCreationPerIntent` | 真实 SQLite 事务并发、提交前/后崩溃、重试与同键异参；不能仅 mock add 返回值 |
| `ReadQueryRequiresNoDurableWrite` | clock/status 在 DB 挂起时仍经真实只读入口返回；时区变化/未知与旧 prompt 时间不串线 |
| `ReminderSettledOnce`、`CancelledOfferCannotStart` | cancel/due/claim/started/played 交错及重复/错设备回执，走现有 gateway settlement 路径 |
| `NoUnsolicitedNonReminder` | 未激活下人脸/回家/任务完成不播报；明确 reminder 仍经 quiet/cap/device 门控 |
| `AppliedVolumeRequiresCurrentReceipt` | 断线重连、旧 connection、hardware_applied=false、pending 超时、本地命令重复 |
| `NoRevokedMemoryInResult`、RA-06 `ContextSnapshot` | 记忆删除/主体纠错与旧检索/旧 prompt 响应竞争，清理失败时保持 fence |
| `PreparedIsNotDelivered`、RA-04 `TaskDelivery`、既有 `TurnPlayback` | prepared 无 lease、部分播放、Stop 后迟到 receipt、重播同 artifact 新 attempt |
| `NoImplicitEffectReplay`、RA-05 `CapabilityExecution`、RA-06 `TaskControl` | print unknown、取消/改要求、show/replay 不重跑准备工具或打印 |
| RA-01 `ResponsiveControl` | reminder SQL、检索、device ACK、media worker 永久挂起时，新输入与停止仍可推进 |

语言 fixture 至少包括明确提醒、过去叙述“妈妈提醒我喝水”、故事台词、否定、含糊时间和两个可取消对象。自然语言 fixture 只能证明所测模型/版本的案例，不证明任何措辞都正确。实机播放、音量、打印和时延需单独记录；本次未运行。

提醒修改 fixture 必须预期 unsupported，验证没有创建/取消副作用和假“改好了”；若用户明确选择分开取消/新建，再测试两事务之间的失败与真实回执。

## 7. 迁移与回滚

旧 reminder 保持原 ID、due 与状态；迁移只为其标记 legacy 来源，不伪造自然语言意图键或用户授权。新 adapter 创建的记录增加可去重关系，重复迁移不得再创建计划。旧记忆与 artifact 使用原来源过滤，缺证据条目不能补写为已确认。

按能力逐项开启：查询与自知、记忆写/删、一次性提醒、音量、展示/重播、已有媒体/打印包装。某 adapter 出问题时从目录撤下新执行，保留状态、取消、遗忘清理、已接纳提醒调度与 unknown-effect 核对；不要停掉整条对话或重新运行已提交效果。

回滚不重置 reminder played/cancelled、不删除操作 tombstone、不恢复旧音量命令、不重新构建已删声音/记忆。跨库迁移与启动一致性按 RA-09；schema 不兼容时采用显式停止写入，不能降级绕过权限。

## 8. 完成定义

- [ ] 本页首批能力从自然语言可达，并经统一 registry/host context/typed adapter；没有通用 RPC 或任意 shell 透传。
- [ ] 无副作用查询可从合法快照立即返回，DB 写入卡住不会阻塞“做到哪了”；stale 与撤销分别处理。
- [ ] 显式 remember 与自动 history 留存分离；来源、主体、私域、纠错/遗忘传播完整且不重放工具。
- [ ] 提醒具有时区、去重、真实提交、查询/取消和真实播放结算；无凭模型“好”就算设好的路径。
- [ ] 设备固定本地命令保留，不依赖新账本，不重复执行；模型音量动作有真实当前连接回执且不离线排队。
- [ ] 展示/重播复用不可变版本，prepared 与 played/displayed 分离；打印未知效果不盲重试。
- [ ] 所有慢路径有容量、取消、超时、冲突和恢复策略，控制域不等待它们；主动范围未被扩大。
- [ ] 同一行为 PR 提交领域 TLA/配置/检查脚本和真实路径回归，设备/外部效果及性能验证按实情记录。

当前全部待实现。文档完整不等于 adapter 已可用，更不等于硬件与模型已经通过上述验收。
