# RA-07：晚命名、声音绑定与可纠正的身份连续性

日期：2026-11-25。状态：**待实现**。验证：**not_run**。

依赖（按 PR #37 review 拆分）：**RA-07a** 依赖 [RA-05 能力目录](ra-05-capability-registry.md) 与 [RA-08](ra-08-assistant-adapters.md) 共用的 bot-server 名字校验器；**RA-07b** 依赖 RA-07a 与 [RA-06 上下文与任务控制](ra-06-context-and-task-control.md)。总议题：[#36](https://github.com/JasonGuoo/esp-bot/issues/36)。领域依据：[identity-continuity.md](../identity-continuity.md)。源码基线为 `46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`；以下新增接口、字段、表与验证均为拟议，当前未实现。

## 1. 目标与非目标

本步骤分两部分交付、分别验收：

### RA-07a：称呼与当前声音

进入 M2：bot-server 名字校验、保存称呼（§4.1）、绑定当前可靠声音（§4.2）、`identity.status/correct/forget_voice`（§4.4）。主要复用 `voice_album.rs` 的晚命名和 2026-11-18 的声音匹配称呼路径，不做历史回认。

### RA-07b：历史关联

之后单独验收：历史回查与重新关联（§4.3），包括对留存音频重新分段、跨模型特征比对的限制、代表性样本询问，以及向 Graphiti 的传播。RA-07b 未完成不阻塞 M2。


用户可能先聊过很多次，后来才说“记住，我是 Jason”。本步骤让小呆能保存当前自报称呼、使用已有合格声音证据建立人物关系、在证据允许的范围内关联历史，并继续聊天。用户可以查询“你记住我的声音了吗”、补充样本、说明“刚才不是我”、改称呼或忘记声音。

称呼保存、声音绑定、历史归属分别回执。没有旧声音/特征时不声称可以声学回认；声音模型尚未就绪时不阻塞保存称呼。声纹不等于认证、角色或家长权限，不能解锁私密资料，不能自动开麦。

本步骤复用 profiles、声音相册、模板登记和来源修订。它补对话入口与跨回合业务闭环，不另建家庭成员数据库，不替换现有声纹模型，不承诺真实儿童/成人或远场准确率。

## 2. 现有机制与具体文件

本表内路径相对 `server/bot-server/src-tauri/src/`。

| 文件 | 基线事实 | 本步骤需要的改变 |
| --- | --- | --- |
| `memory/voice_album.rs`、`voice_schema.sql` | 匿名声音组、合格片段收集、后来命名、group revision、单条人工标注保护已有；含命名前后历史/未来归属测试源码 | 抽取来源级候选核验与窄范围绑定，不直接把整个组的管理命名当成自然语言 self-introduction |
| `memory/identity.rs`、`types.rs` | 显式 enrollment、样本/质量/模型一致性、`guardian_confirmed`、重复模板与替换约束 | 复用既有授权；将 unavailable/insufficient/conflict 与实际 bound 结果映射到能力回执 |
| `memory/household.rs`、`household_types.rs`、`provenance.rs` | 来源 journal、segments/annotations、source revision、派生物清理 | 增加自报称呼与来源级身份依据的 typed 事务；保留原有删除/人工修订优先级 |
| `memory/profile_merge.rs` | profile 合并/aliases 与模板归属规则 | 不把显示昵称等同 profile merge；忘记声音需覆盖别名来源模板，合并仍需原授权 |
| `brain/orchestration.rs`、`memory_worker.rs`、`mod.rs` | ASR/identity 分支、晚到身份、worker 和 source preflight | 将证据变更投递到 RA-06 快照；前台不等待识别，迟到结果仍绑定原来源 |
| `brain/rpc.rs` | 控制台 `household.voice.*` 与 `identity.*`；部分 mutation 全局取消/长 gate | 管理 UI 和新 adapter 共享窄领域服务；对话入口不经通用 dispatcher |
| `brain/tasks.rs` | 持久 workflow 载体、补参/取消/恢复 | 注册身份补样/历史关联 handler，复用 RA-06 的 task/attempt/input revision |

拟新增 `brain/assistant_adapters/identity.rs`、`memory/identity_continuity.rs`。在既有 `memory/additive_schema.sql` 增加需要的身份断言/依据记录，使用 RA-05 的 `assistant_operations`；不再创建身份专用 operation 账本。长工作流 checkpoint 存于 TaskService 所属任务数据，不能在 memory 中复制一份权威 workflow state。

## 3. 数据与状态契约

### 3.1 稳定人物与可修订证据

`profiles.id` 继续是人物稳定 ID。名字只是带出处的断言。显示昵称、姓名别名与 `profile_aliases` 的历史 profile 合并不是同一个概念；同名不能自动合并，改称呼不能覆盖其他人的 enrollment。

拟议身份记录应至少保存：`assertion_id`、`person_id`、断言类型、显示称呼或绑定对象、可信 `SourceRef`、片段范围、依据类型、相关 model/preprocessing 标识、group/annotation/source revision、policy/voice-review generation、创建操作 ID、有效/撤销状态与修订。它们记录 provenance 与当前关系的依据；当前段标注仍通过既有 household annotation 体系更新，不创建相互竞争的 speaker 主表。

依据类型至少区分：当前自我介绍、受授权的逐段声学匹配、已有 album group、显式 enrollment、用户对具体旧来源的指认、控制台人工更正。用户说“昨天那段是我”是人工归属证据，不能被标为声学验证。

无音频的文本自报可保存称呼和本次来源关系，但不能获得未来语音自动识别能力。人物记录的创建或名字保存不授予任何角色；无法将两个自报名可靠解析到同一实体时，保留冲突或短消歧，不按名字字符串直接合并。

### 3.2 三条领域状态轴

| 字段 | 状态 | 提交依据 |
| --- | --- | --- |
| `name_state` | unknown / claimed / saved / conflict / revoked | saved 需称呼与来源关系的持久事务 |
| `voice_state` | unavailable / insufficient / candidate / bound / conflict / revoked | bound 需合格、兼容、未撤销的声音证据和已提交关系；必须包含 basis |
| `history_state` | no_evidence / candidates / linking / partial / linked / revoked | linked 只对应明确来源集合及 revision，不表示“所有历史” |

领域结果含有限的 `linked_count/rejected_count/pending_count` 与可解释原因。`name=saved, voice=unavailable` 是有效的部分成功；不能为了单一成功标志等待所有分支完成。

`identity.status` 是只读能力，直接从合法快照给出 `read_result`，包含版本、as-of 和 stale 标记；不先写操作账本。快照被撤权或来源失效时不能仅加 stale 后继续披露，应返回该范围不可用。

### 3.3 工作流归属

`identity.remember_self` 的称呼保存是短动作；需要补样或历史回查时创建/关联现有 `TaskService` 工作流。拟议任务种类为 `identity.continuity`，由同一 typed adapter 驱动，不启动第二个 agent。权威状态在任务库：`task_id`、`task_revision`、任务 `input_revision`、attempt ID 和 `checkpoint_revision`；memory 保存领域事实和短操作结果。

checkpoint 至少包含原 self-introduction SourceRef、目标 person、已接受的候选集版本、已处理的来源游标、待问问题与相应输入、声音/政策 generation、已提交的领域操作 ID。游标只能用于恢复，不是免检通行证；每个批次提交前重验来源与版本。跨库写入通过 RA-09 的 outbox/reconcile 关联，不能声称 checkpoint 与 identity SQL 在不同库里原子提交。

下一次补样经 RA-06 目标解析绑定该 task 和新的来源；不能因用户说了另一句话就暗中把所有后续声音当作同一个人的补样。task 输入版本与 `ForegroundRef.input_revision` 不同，不能混用。

## 4. 详细实现顺序

### 4.1 先提交当前自我介绍的可确定部分

0. 先经 bot-server 共享名字校验器（[identity-continuity §3.1](../identity-continuity.md#31-名字校验由-bot-server-统一负责owner-决定)）：称谓/亲属词与冒犯性词（中英文及变体）返回 rejected，小呆礼貌重问，不保存任何内容。同一校验器也用于声音匹配得到的名字、App/控制台登记与声音组命名、以及创建/重命名人物的记忆写入；任何路径不能绕过。称谓只可另存为 `relationship` 字段，不作身份主键，也不单独解析到成员。
1. `identity.remember_self` 仅接受称呼、用户明确表达的绑定意图与 `target_handle=current_speaker`。模型不填写 person 权限、音频 URL/path、guardian 或 source revision。提案来源必须是当前有效用户输入；故事、引用、否定和演练不构成自动登记请求。
2. 宿主用已有 SourceRef 解析该句话对应的音频/分段。`current_speaker` 不能解析为最近任意音频；ASR 与 diarization 无法可靠对齐、多说话者或重叠时返回声音待核对。先保留当前自报称呼，不将混合音频登记到某个人。
3. 解析已有当前人物关系与同名冲突。所称成员已有确认声音而当前声音不匹配时返回 conflict：不绑定、不合并、不读取其私密范围或私密 OpenCode profile，只作本次称呼并提示家长。同名多实体、相反人工标注、模板冲突进入 needs_input。保存新名字须具备可靠的单人声音匹配，或 owner 在 App 中确认；两者都没有时只作本次 activation 的称呼并提交复核。
4. 在短事务中写称呼断言、人物关系与同意图的 `assistant_operations` 结果，提交后才发 applied。事务失败不能说“记住了”。这一步不等待声纹、历史回查、Graphiti、harness 清理或输出播放。
5. 发布身份投影更新到 RA-06；名字进入后续合法上下文。已播放语句不回写，前一用户的名字不能成为后一名未知说话者的默认身份。

### 4.2 再用合格证据建立声音关系

6. 检查当前 voice-review 开关/generation、留存、source revision、私密范围与 model health。现有相册默认关闭时不能自动打开；原始音频不能为了 LLM 理解而发送给聊天模型。
7. 若当前有效相册已有合格片段，复用原 model_id、质量、single-speaker/overlap、组 revision 和匹配依据。若只有兼容模板，沿用 enrollment 的质量与授权；若两条路径冲突，保持 unknown/conflict。
8. 新增一个窄的“从本次来源关联人物”领域接口，输入由 host 验证的候选来源集及期望 revision。不能直接调用现有 `edit_voice_group` 将整个匿名组全部变成人工确认。当前来源的明确自报与其他成员的自动声学候选分开记账。
9. 缺样但收集政策允许时，任务进入 needs_input；请求一个短、自然的有效样本，仍需正常激活/授权输入。无法或不允许收集时返回 partial/unavailable，保留已保存称呼。模型不得设置 `guardian_confirmed=true`；受既有 enrollment 政策限制的动作返回 needs_authorization，授权方式变更另行评审。
10. 声音关系提交后报告其实际依据；bound 不代表无误识别。所有新匹配结果仍是可修订证据，不成为家庭/私密授权 token。

### 4.3 后台有条件关联历史（RA-07b）

11. 从当前合法匿名组和其片段生成有上限的候选集合。逐段验证来源存在、留存允许、模型/前处理兼容、声音质量、未被独立人工纠正；不按设备、日期、话题或文风补认。
12. 有合法原音频但无特征时，可在后台按既有授权重新提取；只有同维度的不同模型 embedding 不可直接比较。原音频/特征均已删除时记录 no_evidence；文本只能按用户明确指认建立人工关系。
13. 将可靠匹配和需用户确认的候选分开。自报“我是 Jason”不等于人工确认整个聚类；需要询问时只选授权可见的具体候选，不泄露别的私密内容。用户回答只扩展到其明确范围。
14. 分批提交关联，记录 source/group/annotation/checkpoint revision 与拒绝原因。每批检查取消、撤权和新的人工修正；新 round 不能拖住前台。累计 partial/linked 结果经 inbox/快照回流，不擅自发声。

### 4.4 更正和删除必须与创建一起可用

15. `identity.correct` 解析目标为称呼、人物关系或具体说话来源，不能含糊地修改整个家庭。单条人工更正优先于组级确认，组级确认优先于自动候选；CAS 冲突时重读，不最后写入者覆盖。
16. `identity.forget_voice` 明确 person/scope 后撤销相册声音关系、模板、profile alias 来源 enrollment 与相关可再识别特征/缓存；保留名字/昵称、profile 合并关系、未要求删除的文字记忆和合法任务。授权来自现有宿主规则，不能只凭自报名字或声纹候选删除别人的资料。
17. 更正/删除先提升适当 fence 并提交权威修订和清理 outbox；派生摘要、索引、Graphiti、上下文、artifact 来源与缓存按依赖传播。若 SQL 未提交只能说已收到删除要求；实际删除结果按 applied/partial 回执。
18. 慢清理可恢复，读取立即拒绝被 fence 的旧内容。source-first/outbox 保障不可因移除管理 RPC 全局 gate 而丢失；不在 held turn gate 下等待清理/模型/用户。工具不重放、权限不升级、已发生 effect 不抹除。

“忘记现有声音”删除已有证据并保留最小撤销 fence；“以后不要识别我的声音”另保存持续偏好。两者之后都不允许旧/新自动样本悄悄重建同一 person 绑定，重新登记须新的明确意图和宿主授权，单纯又说了一次名字不算。撤销记录不得保存已要求删除的声纹作为 opt-out 识别器。无声纹后无法可靠知道全新匿名声音是否来自该 person：以当前明确会话身份、可信管理选择或家庭收集设置覆盖可确定范围，诚实说明其余未知，不承诺无法实现的个人全场景自动排除。

## 5. 失败、取消和竞争

| 情况 | 必须行为 |
| --- | --- |
| 自报后声纹晚到，此时另一人正在说话 | 只尝试绑定原 SourceRef 和期望版本；不取当前最新音频补名 |
| voice-review 关闭又重开 | generation 变化使旧推理失效；不能把关闭前未提交样本补收进新一代 |
| 原来源过期、删除或转为不可用 scope | 不再用它推进声音/历史绑定；既有清理传播和任务状态按依赖更新 |
| 名字已保存、声音模型失败 | name saved / voice unavailable；正常对话继续，可查询实际部分结果 |
| 同名、相反模板、误聚类 | 不覆盖、不合并；请求最小消歧或保留 unknown，可按来源拆分 |
| 单条人工更正与 group/history writer 并发 | annotation/group/source revision 条件更新；人工更正不能被旧批次覆盖 |
| 补样取消 | 停止后续收样/回查，保留已经提交的称呼事实；若用户明确撤销，再执行独立更正/遗忘 |
| 历史关联批次部分完成后崩溃 | checkpoint 与领域操作去重恢复；已关联片段不重复写，未处理部分重验来源 |
| 删除/清理中进程退出 | 重启先应用撤销 fence、继续 outbox；缓存/旧 worker 结果不得复活声音 |
| 忘记声音后出现自动新样本 | 不自动重建同一 person 绑定；显式重新登记前保留撤销偏好与名字/文字记忆的独立状态 |

模型理解自我介绍不应声称百分之百可靠；语言回归应包含引用、否定、多人和近似措辞。更复杂的含糊指代进入有界协商，不将不确定性隐藏为错误确定身份。

## 6. TLA+ 与实现回归

拟新增 `formal/tla/macos/IdentityContinuity/{IdentityContinuity.tla,IdentityContinuity.cfg,checks.sh}`。模型至少包含两个 person、两个来源、一个可误聚类组、两个模型/voice-review generation、人工更正和晚到推理；可对有限候选集建模，不将真实相似度计算假设为必然正确。

| 待证明性质 | 对应实现回归 |
| --- | --- |
| `CurrentSourceBindingOnly` | 真实 adapter 接纳 self-intro，插入另一人的下一轮，再送旧 identity 结果；只更新原来源 |
| `NameDoesNotImplyVoiceOrHistory` | 无模型、无原音频、文本自报、收集关闭，逐项检查三状态回执与 prompt |
| `NoCrossModelMatch`、`NoMixedSpeakerEnrollment` | 同维度异 model_id、重叠音频、同轮多 label、质量不合格；不可被登记 |
| `ManualAnnotationWins` | 单条人工更正/拆分与整组后命名/历史回查竞争；复用现有 voice album 测试并增加 adapter 入口回归 |
| `NoRevokedEvidenceResurrection` | 删除/关闭/重启/旧 generation 及新自动样本交错；检查 SQLite、缓存、投影、outbox 与显式重新登记门槛 |
| `HistoryHasBoundedEvidence` | 逐段来源集、仅平均向量、纯文本、留存过期；不能全库归名 |
| `SpeakerDoesNotRewriteSemanticSubject`、`NoAuthorityPromotion` | Jason 说妈妈偏好，后来补齐身份；事实主体/任务授权不被改写 |
| `IdentityDoesNotReplayOperations` | 更正前后记录任务/effect 调用次数；不重放媒体、打印、提醒或旧工具 |
| `NameValidatedEverywhere` | 称谓/亲属词与冒犯性名字（含变体）经语音、声音匹配命名、App 登记、记忆写入四个入口都被拒绝且不落库 |
| `SelfClaimNeverUnlocksPrivate` | 无可靠声音匹配的“我是妈妈/我是<名字>”不读取私密记忆、不使用私密 OpenCode profile；不匹配已有成员声音时为 conflict |
| `CancelledWorkflowDoesNotContinueBinding` | cancel 与批次提交竞争，CAS 阻止未授权后续关联；已提交称呼仍可查 |

复用 RA-05 `CapabilityExecution`、RA-06 `ContextSnapshot` / `TaskControl` 和既有 `ReplyTiming` 的约束：身份永久挂起时首轮回答与 Stop 仍可推进；晚到名字只进入后续快照，不改冻结句子。

实现回归落在 `memory/identity_continuity.rs`、`voice_album.rs`、`identity.rs` 及 `brain/assistant_adapters/identity.rs` 的真实领域/adapter 测试，覆盖 [ID-01–ID-20](../identity-continuity.md)。TLA 不能证明声学准确率；真实说话者、成人/儿童、距离、噪声、交叠、误认/漏认与阈值需独立实测。未测保持 not_run，不填虚构成功率。

## 7. 迁移与回滚

既有匿名组、人工标注、profile/enrollment 和 voice-review 设置保持原语义。对旧数据只记录可证明的来源；没有 assertion 来源的旧显示名字不得回填为“当前用户亲口确认”，也不能把旧组自动全部升级为人工标注。

新增身份语义入口开关关闭后，模型目录撤下 remember_self/补样；已有人员、控制台管理、状态查询和撤销清理仍可用。正在进行的身份 workflow 暂停或按明确策略取消，记录已提交部分；不能在回退时重新开始全库识别或丢失 tombstone。回滚不自动启用 voice review，不降低 guardian 条件，不恢复被删除样本。

数据迁移、operation/task 跨库关联和启动恢复顺序归 RA-09。新增 schema 若使旧二进制不兼容，应由版本检查停止写入；不能直接运行旧代码绕过新的证据 fence。

## 8. 完成定义

- [ ] 已经聊过多次后才说名字的端到端流程成立，复用有效匿名证据；无证据路径同样自然、准确。
- [ ] bot-server 名字校验器在全部入口生效；称谓只存为 relationship；冒犯性名字礼貌重问且不保存；ID-17～ID-20 有真实回归。
- [ ] 无可靠声音匹配的自报不解锁私密内容；保存新名字需可靠声音匹配或 App 确认。
- [ ] RA-07a 与 RA-07b 分别记录完成与验收状态。
- [ ] 名字、声音、历史三状态独立，applied 及 read_result 真实；不因保存称呼虚报认声。
- [ ] 当前 self-intro 来源由宿主绑定，多人/文本/迟到/引用/否定等边界有真实 adapter 回归。
- [ ] 名字别名、同名、误聚类、逐条人工更正、跨模型不兼容与留存关闭均有明确行为。
- [ ] 长 workflow 只由 TaskService 拥有，能查询、补样、取消、重启恢复；前台不等待身份工作。
- [ ] 遗忘覆盖实际声音存储和派生缓存/索引，源先提交与 outbox 保持；更正不升级权限、不重放工具。
- [ ] 自然语言路径不调用会全局取消/持长期 gate 的管理 dispatcher；原控制台路径保持兼容。
- [ ] 对应 TLA/配置/检查脚本和实现回归随行为代码提交，声学/设备验证状态独立报告。

以上是后续实现的验收门槛，本次文档变更不表示任何一项已经完成。
