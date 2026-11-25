# 随时回应与助理能力：验收契约

日期：2026-11-25。设计基线：`46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`。总议题：[#36](https://github.com/JasonGuoo/esp-bot/issues/36)。规格状态：**待实现**。本矩阵所有新场景的执行状态：**not_run**。

本文件定义未来行为 PR 的验收方式。本次只写设计文档：没有运行 TLC、Rust/固件回归、macOS App、真实模型或 S31 板测。仓库历史验收记录保留其原日期和修订范围，不构成本方案已经通过的证据。

## 1. 验收对象与证据规则

“随时回应”首先意味着设备本地 Stop 和主控制循环不依赖任何后端工作完成。ASR、声纹、Luna、OpenCode、MCP、TTS、图片、数据库、Graphiti、打印、提醒准备和后台清理均可能永不返回；这时仍能接收有效控制、撤销旧输出、处理连接状态，并及时给出接纳或拒绝状态。生成有内容的回答仍取决于可用的回答路径；不能将预生成 ACK 计作任务完成或内容首答。

验收使用四类独立证据：

| 证据层 | 可以证明的内容 | 不能代替的证据 |
| --- | --- | --- |
| TLA+/TLC | 声明的有限状态、并发交错、安全性和明确 fairness 条件下的活性 | 实际线程/SQL/队列/网络/回调没有阻塞；真实时间、音质和识别准确率 |
| 实现回归 | 真实控制/worker/存储边界、有限队列、取消清理、版本检查、loopback 网络行为 | 外部模型效果、操作系统真实音频播放、S31 的麦克风/扬声器 |
| 配置好的 Mac 集成 | App、sidecar、OpenCode、已配置 adapter 的真实可用性和回执 | 固件录音许可、硬件播放结束、声学混音和人机感受 |
| S31 与真人场景 | 本地停止、半双工、唤醒/回声、设备回执、多人归属与实际体验 | 对所有并发交错的穷尽证明 |

每次执行记录实际 commit、构建配置、协议协商、能力目录版本、设备/模型版本、场景 ID、事件 trace、结果、限制和复现步骤。源代码存在、模型目录存在、配置 enabled、HTTP 可达、工具已返回 JSON 都不是能力已完成的证明。

## 2. 统一观察点与拟议性能目标

至少记录：`request_id/event_id`、`service_epoch`、RA-01 `ForegroundRef`、`OperationRef`、task/input/result/delivery revision、source revision、输出 lease/playback/segment、单调时间及必要 UTC 时间。日志默认写事件与时长、数量和标识；不要为测延迟复制私密语音或完整个人记忆。

每个慢步骤记录 queued/started/finished/cancel_requested/cancel_observed/cleaned 或 quarantined；同时采集各队列占用、拒绝/合并数量、live worker/子进程数。成功取消不能仅靠 UI 变化认定。控制 loop 的调用图需覆盖同步函数、事件回调、锁、序列化、大块拷贝、文件 I/O、析构、join、进程读取和 socket writer，不能只搜索 `.await`。

以下是需要在配置好的 Mac/S31 上评估的 **proposal，非实测结果，也不是现有保证**：

| 指标 | 拟议 p95 / p99 | 定义与边界 |
| --- | --- | --- |
| host control dispatch | ≤ 50 ms / ≤ 100 ms | 控制事件被接纳至本地状态裁决及非阻塞 enqueue/reject；不包含 socket 尚未读到的网络排队，入队不等于已经网络发送；测试时后端可永久 pending |
| S31 本地 Stop | ≤ 100 ms / ≤ 200 ms | 有意触摸事件被 runtime 识别至撤销当前 token 的音频所有权/禁止其后续播放及采集；下一次合法 activation 仍可打开输入；硬件实际余响另测 |
| 可用短 ACK | ≤ 300 ms / ≤ 500 ms | 完整输入被接纳至本地/缓存 ACK 开始送出；要求素材已有、许可有效且传输健康；另量 commit→输入完整及硬件听见时间，不隐藏 PendingCommit 等待 |
| 下一轮可接纳 | 不依赖后台 task 完成 | 当前前台片段实际结束/本地中断并取得合法下一轮 token 后即可接纳；以事件顺序验收，另测实际时长 |
| 有效快照读取 | 进入 control 时不等待 | 读取已发布引用；生成/刷新时间单列。找不到安全快照时降级，不能等待 SQL |

不将上述指标求平均后掩盖长尾。每项注明样本数、负载、冷/热启动、成功/拒绝率和时间戳来源；ACK、内容首字、TTS 首包、扬声器首声、task 完成、结果 delivered 分开统计。若调整目标，需在对应行为 PR 中说明设备/环境依据。

## 3. Formal 责任索引

下列六个模型**已在基线仓库存在**；具体命令与历史范围见 [formal/tla/README.md](../../../formal/tla/README.md)。

| 简写 | 现有模型 | 本方案必须保留或扩展的实际性质 |
| --- | --- | --- |
| LG | `firmware/ListeningGate` | `OnlyIntentAuthorizes`、`OnlyFreshInput`、`RoomDoesNotWake`；提醒/被动事件不授权录音 |
| UC | `protocol/UtteranceCommit` | `FreshPrefix`、`CompleteUtterance`、`CommitBeforeAnswer`、`DrainsAfterCommit`；补足 pending commit/取消/连接代际 |
| RT | `macos/ReplyTiming` | `OrderedSpeech`、`IdentityOnlyLabels`、`SilentAfterStop`、`ProgressWithoutIdentity`；现有进展假设包含健康生成/TTS，不能拿来证明模型永久挂起时的 control 活性 |
| SP | `protocol/Supplement` | `PreviousQuestion`、`OnlyConfirmedSpeech`、`LateReceiptsStayRetired`、`ReservationSpent` |
| TP | `protocol/TurnPlayback` | `FollowupHasEvidence`、`StoppedHasNoPlayback`、`WaitForRegisteredPlayback`、`OldReceiptsAreInert` |
| CC | `macos/ConversationClose` | `OnlyFarewellsClose`、`GoodbyeIsHonored`、`TurnResolves` |

以下为各步骤**拟新增**的模型，仅在此列计划路径，不代表文件已经存在：

| 简写 | 计划路径 | 主责步骤 |
| --- | --- | --- |
| RC | `formal/tla/macos/ResponsiveControl/` | RA-01：控制接纳、撤销、有限容量、后端永不完成时的控制进展 |
| GD | `formal/tla/protocol/GatewayDispatch/` | RA-02：reader/writer、pending commit、socket 压力与连接代际 |
| LW | `formal/tla/macos/LunaWorker/` | RA-03：会话所有权、取消、隔离、补充预留与旧 generation |
| TD | `formal/tla/macos/TaskDelivery/` | RA-04：后台任务与前台输入分离、inbox/lease/receipt |
| CE | `formal/tla/macos/CapabilityExecution/` | RA-05 创建、RA-08 扩展：能力 readiness、授权、幂等、真实提交、提醒/设备/记忆 adapter 状态与回执 |
| CS | `formal/tla/macos/ContextSnapshot/` | RA-06：scope、policy epoch、来源有效性、非阻塞发布 |
| TC | `formal/tla/macos/TaskControl/` | RA-06：补参、改要求、cancel/finish/attempt 版本竞争 |
| IC | `formal/tla/macos/IdentityContinuity/` | RA-07：晚登记、匿名组、证据归属、撤销、不提升权限 |
| RE | `formal/tla/macos/Recovery/` | RA-09：崩溃/迁移/outbox/未知 effect/禁止无证据重放 |

fairness 只要求有资格的控制事件得到调度；RC/GD 的控制活性不得假设数据库、Luna、TTS 或任何外部后端最终返回。finite model 需要包含真实的满队列、重复/迟到/乱序事件、至少两个 epoch；单个任务的顺序 happy path 不足以覆盖设计。

## 4. 故障隔离与控制循环

下表及后续各表的状态统一为 **not_run**。硬件列的“主机”表示该项主要可在主机证明，但不免除后续 S31 联调；“S31 必测”表示没有实板证据就不能宣称这一层通过。

| ID / 归属 | 场景与动作 | 可观察的预期 | TLA | 实现回归与硬件边界 |
| --- | --- | --- | --- | --- |
| RA-ACC-01 / 01–03 | 分别让 ASR、声纹、Luna、OpenCode、MCP、TTS、图像、Graphiti、存储、打印 adapter、proactive/failure preparation 永不返回；再同时挂起全部 | Stop、连接状态、拒绝/接纳下一轮仍有有界处理；没有可用回答时给准确等待/不可用状态，不假装内容已生成 | RC/GD/LW/RT | 使用真实 gateway/worker 边界注入 pending future、无输出子进程或受控服务；不能只 mock 外层结果；主机+S31 控制复验 |
| RA-ACC-02 / 01,03 | 占满前台/后台 worker、Luna 会话槽；继续发送新输入及 Stop | 有限接纳，满时明确拒绝或替换允许替换的工作；Stop 不排在普通工作后等待；不无界 spawn | RC/LW | 观察实际 slot、进程、拒绝计数；主机 |
| RA-ACC-03 / 01,02 | 真实填满 ingress、command、completion、普通 writer 队列；在饱和时发 Stop/断线/新 epoch | 控制通道按契约保留/合并关键事件；数据有明示丢弃/拒绝策略；取消不能因满队列永久失效 | RC/GD | 填满实际有限 channel，控制事件接纳与队列容量都有断言；主机 |
| RA-ACC-04 / 02 | peer 停止读 socket，填满发送缓冲；并行发 stop、音频尾帧和回执 | reader/control 不被 writer send/flush 阻塞；按 writer deadline 断连或降级；不承诺坏网络上用户必定听到 ACK | GD/UC/TP | loopback 真 socket/真实 writer 队列，不能只模拟 `send` 返回慢；主机，S31 网络复验 |
| RA-ACC-05 / 01,06,09 | 持有 memory/tasks 的真实 SQLite 写锁及连接 mutex，超过 busy timeout；同时查询状态、提交任务/删除/闲聊/Stop | control 仍推进；只读 `read_result` 可给合法快照及 as_of/stale，不写 ledger/等 DB；未提交变更无 `accepted_durable/applied`；撤销来源立即 fence | RC/CS/CE/RE | 两个实际 SQLite 连接、受控 mutex 和后台任务；主机 |
| RA-ACC-06 / 03,08 | worker 忽略 cancel、子进程 stdout 不关闭、TTS 读取不结束；连续取消和新建 | 立即撤销旧输出资格；后台有限时间终止/隔离；未清理者仍计入容量，资源不无限增长 | RC/LW/CE | 检查进程组、JoinHandle、semaphore、临时文件和读端；主机，S31 播放权复验 |
| RA-ACC-07 / 01,02 | 在候选 helper、事件回调、同步 DB/文件/大序列化/析构路径中注入阻塞或慢 CPU | 调用图不存在 control 到此工作的同步执行边；即便 helper 没有 `.await` 也不得卡主循环 | RC/GD | 真实 callback 边界和线程探针；记录最大 loop turn 耗时，源码搜索仅辅助；主机 |
| RA-ACC-08 / 01–04 | 取消后启动新一轮，再放回旧 Luna/ASR/TTS/任务结果/旧播放回执 | 旧结果至多结算所属持久 attempt，不能改当前前台、重新播放、重开录音或覆盖新要求 | RC/LW/TC/TD/TP | 注入跨 generation/connection/service epoch 的迟到事件；主机+S31 必测 |
| RA-ACC-09 / 02 | `audio.commit` 比尾帧早到，随后补齐、超时、Stop、换连接；覆盖 WSS/现有 plain 路径 | pending commit 不阻塞收帧；只提交同 token 完整音频；新说话使推测 ASR 失效；旧 PCM 不被重新标记 | GD/UC/LG | 真实 gateway loopback 与 token-bound media store；S31 必测停顿续说 |
| RA-ACC-10 / 01–03 | 服务 Stop/断线与 cancel cleanup、warmup、writer failure 同时发生，随后重新启动 | Stop 不在控制循环等待 join/清理；退出阶段有明确 deadline；旧 warmup/worker 不在新服务自发恢复 | RC/GD/LW | 资源清理和服务 epoch 回归；S31 本地 Stop 必测，不能等 Mac 才停 |

## 5. 长任务、对话与呈现

| ID / 归属 | 场景与动作 | 可观察的预期 | TLA | 实现回归与硬件边界 |
| --- | --- | --- | --- | --- |
| RA-ACC-11 / 04 | 提交耗时任务后，连续进行至少 10 轮无关闲聊；任务仍未完成 | task ID/状态保持；每轮前台正常释放和接纳；不存在因 `delivery.tasks` 非空而持续 `session_busy` | TD/TP | 从真实提交、短回应、receipt 到下一轮的 gateway 回归；S31 必测 |
| RA-ACC-12 / 04 | 仅委派任务的短 ACK 或当前答案播完，但后台任务仍 Running | 仅委派以真实 `purpose=final` 委派说明段（M1 主路径；若日后协商 `turn.response_closed`，则为其 `outcome=delegated`）、durable task receipt、producer 封口及该段播放回执结束前台；`task_ack` 本身不是 final。任务仍存在，后续结果走 inbox/新输出裁决 | TD/TP | 不能只删 `tasks.is_empty()`；覆盖无内容仅ACK、旧端不支持关闭协议、独立 inbox；S31 必测 |
| RA-ACC-13 / 06 | 同 scope 有两个 `AwaitingInput`，用户问“你今天开心吗” | 仍为普通闲聊；不因 pending task 的存在把全部输入强送 OpenCode；保留 task 查询能力 | CS/TC | 检查实际路由和两条模型上下文；主机 |
| RA-ACC-14 / 03,04,06 | 播放中触摸补一句；分别继续原问题、重新说、Stop、跳轮，再补任务参数 | 即时补充只承接允许的原问题与已完整听见的片段；不串到其他人/旧轮；后台 task 不因普通补一句消失 | SP/LW/TD/TP | 复用/扩展 codex_fake 与 gateway 补充回归；S31 必测 |
| RA-ACC-15 / 03 | 声纹永不返回，TTS 第二句先准备好，第一句稍后；内容/ACK 竞争 | 普通回复不等身份；片段按顺序；实际内容开始后不插过期 ACK；停止后无新语音 | RT/LW/TP | 真实有序前缀及 late identity 回归；Mac 真 TTS/S31 检查断句 |
| RA-ACC-16 / 02,03 | 多次 idle/foreground/Stop 切换，ACK warmup 被挂起 | warmup 只在允许的空闲 lane 工作，不能占 first-sentence TTS lane；Stop 后不自行继续；已有 warm helper 仅 spawn 的部分不被误判为长 await | RC/RT/LW | 真实 worker lane/锁与 warmup 取消回归；主机 |
| RA-ACC-17 / 02,08 | 提醒到点或失败提示开始准备 TTS，准备过程永不完成；用户同时输入/停止 | `prepare_proactive/prepare_failure` 不能占 gateway 分支；提醒不打开录音；失败提示仍有无音频可见降级 | RC/GD/LG/CE | 真实 timer/错误分支注入挂起；S31 必测提醒与新输入竞争 |
| RA-ACC-18 / 01,04 | 半双工播放中触摸 Stop、再有意开始新输入；播放原内容带有唤醒词 | 本地 runtime 停音/撤权后才开始合法采集；不会把自己的扬声器当新请求；不声称已支持可靠声学打断 | LG/TP/UC | 固件 owner 与 token 回归；S31 声学必测，AEC 默认关闭 |
| RA-ACC-19 / 01,08 | 被动人脸/视线/噪音、提醒结束、任务完成、单次“小呆”和完整“小呆小呆”分别触发 | 只有完整唤醒或有意触摸授予对话；明确提醒可播报但不录音；当前 MultiNet7 路径如实标记 | LG/TP | 固件回归+S31 必测；源审不能给出唤醒准确率 |
| RA-ACC-20 / 03,04 | “晚安”“先这样”“讲个晚安故事”“别结束，继续说” | 真正道别短回应后安静；故事/否定/继续指令不误结束；不因未完后台任务保留录音 | CC/TD/LG | flow 与 gateway 回归；S31 真人体验 |

## 6. 上下文、任务控制与身份连续性

| ID / 归属 | 场景与动作 | 可观察的预期 | TLA | 实现回归与硬件边界 |
| --- | --- | --- | --- | --- |
| RA-ACC-21 / 06 | 询问一个运行任务、一个最近完成任务、一个未送达结果，并切换合法 scope | core/OpenCode 得到相同版本化摘要；`query_snapshot` 返回 `read_result`、无新 operation/DB 写；状态、完成、送达三者清楚；不泄露其他私域 | CS/TC/TD | 真投影+两条 prompt 路径，不只测字符串模板；主机 |
| RA-ACC-22 / 06 | 对 AwaitingInput 说缺失参数，重复同请求，再发送过期版本 | 沿用同 task ID；只产生一个接受版本；过期 CAS 返回明确冲突；没有新建替代任务 | TC/CE | 真实 TaskService/operation 账本与重试；主机 |
| RA-ACC-23 / 06 | 运行中把“猫”改成“狗”，旧 attempt 同时完成；覆盖安全更新点与安全重启 adapter | 新要求先提交再确认应用；旧结果不写当前 result；不支持修改/已跨 effect 边界的任务明确拒绝或请求新意图 | TC/CE/TD | 两个 attempt + 不同 input revision 的实际竞争；主机，若涉及打印需 Mac adapter |
| RA-ACC-24 / 06 | 并发 cancel/amend/finish；再重复取消、收到晚到 effect 证据 | 取消与版本结果可追溯；不会复活任务；真实旧 effect 可记账但不能重新获得播放权 | TC/CE/TD | 强制各事务交错和 worker cleanup；主机 |
| RA-ACC-25 / 06 | 两个任务都缺时间，用户仅说“三点”；另一轮明确“刚才那个搜索取消” | 多义时短澄清，不随机选任务；明确且唯一时可直接操作；普通闲聊不被劫持成补参 | TC/CS | 真实候选匹配及能力提案校验；主机，真人表达复验 |
| RA-ACC-26 / 05,06 | 模型重复工具调用、响应丢失重试、同一轮被重复投递 | host 幂等键稳定；同请求同结果，不重复 side effect；不同新意图可以产生新的 operation | CE/TC/RE | gateway→dispatcher→数据库的重复消息测试；主机 |
| RA-ACC-27 / 06 | snapshot 构建/Graphiti 永不结束，当前发生删除/过期/私域切换 | 不等新快照；旧内容越过 policy epoch 就失效；只能用有效降级数据，不能用旧名字/记忆假装已更新 | CS/RC | 实际投影发布、迟到结果与权限栅栏；主机 |
| RA-ACC-28 / 06,09 | 删除来源时 Luna 已读旧上下文、输出准备中、inbox 尚未播报；删除后旧结果到达 | 线程/投影/输出均失效；不从 Graphiti、缓存、outbox 恢复来源；删除成功只在真实提交后 | CS/TD/RE/CE | 真实 source cleanup 与跨库重放；主机+S31 无残留播报复验 |
| RA-ACC-29 / 06,09 | 23:59 发任务，跨家庭日界继续聊并查询；时区非 UTC | 原 task 保留出处且新一天可查；日记/线程按家庭日界正确切换；时区不变成身份权限 | CS/TC/RE | 可控时钟与真实数据库查询；主机，Mac 时间集成 |
| RA-ACC-30 / 07 | 已有 30 轮聊天，在允许声音收集且证据保留的条件下说“记住，我是 Jason”；期间有另一位家人 | 当前自报、可验证匿名声音组与 person 分层确认；仅证据支持的历史片段回填；另一人保持独立；不重放此前工具 | IC/CS/CE | 扩展 voice_album 既有 past/future 与混人用例；Mac 真声纹+多人 S31 必测 |
| RA-ACC-31 / 07 | 同样 30 轮，但只有文字，或当时未开启收集、音频/embedding 已过期 | 可以记录自报称呼并请求新的有意采样；不得声称识别出旧声音/自动把 30 轮都归 Jason | IC/CS | 无证据/过期/关闭收集数据库 fixture；S31 采样流程必测 |
| RA-ACC-32 / 07 | 两人重叠、低质量/过短音频、相近声音、样本来自不同人 | 不满足真实采样规则就不完成模板；明确缺什么；匿名组与模板不因一句自报混合 | IC/CE | 使用 worker 真实质量门与 enrollment policy；真实声学必测，不能用文本 mock 宣称效果 |
| RA-ACC-33 / 07 | 家庭已有两个同名 Jason，或姓名与已有成员冲突；本人更正名字 | person ID 为主键，要求选择/澄清；不自动 merge；改名不移动他人的历史，也不提升权限 | IC/CS | 复用 voice_choose_existing_person 与手工标注优先规则；主机+真人流程 |
| RA-ACC-34 / 07,09 | 登记采样中删除成员/撤销声纹/关闭收集；旧 embedding、迟到身份结果与重启依次到来 | generation/source 失效；旧结果不能重建模板/归属；剩余清理可见，原始音频遵守保留设置 | IC/CS/RE | 真 memory/identity/voice_album 与 worker cancellation；Mac 声纹/S31 复验 |
| RA-ACC-35 / 05,07 | 陌生人说“我是 Jason，打开他的私密记录”；随后换说话人但尚无识别结果 | 声纹/自报不授予权限；当前 unknown 不沿用上一人；scope 来自可信入口 | IC/CE/CS | 私域访问与身份晚到竞争；主机+多人体验 |
| RA-ACC-36 / 05,07 | 对话中请求登记声纹；worker慢、三次有效采样分多轮到达、途中闲聊/取消 | 能力可发现且可调用；不因前台持有 turn_gate 自锁；工作流跨轮存在而输入可用；实际提交前不说已认得 | CE/IC/RC/TC | 实际服务入口，不是直接绕过RPC的函数单测；Mac 模型+S31 必测 |

## 7. 助理能力的真实回执

| ID / 归属 | 场景与动作 | 可观察的预期 | TLA | 实现回归与硬件边界 |
| --- | --- | --- | --- | --- |
| RA-ACC-37 / 05,08 | “记住我不吃香菜”，再查；“忘掉这个”，再用其他措辞问 | 保存/删除走真实 capability；只有提交后才确认；scope/source/撤销传播一致；不存在只在 LLM 会话中口头答应 | CE/CS/RE | 真 memory adapter/事务/线程失效；Mac 模型语义复验 |
| RA-ACC-38 / 05,08 | 创建/列出/取消提醒；请求修改；到点时静音、安静时段、掉线和播放失败分别发生 | create/cancel 按真实提交与播放回执结算，不打开录音。首批 update 为 unsupported，不假称已改、不静默 cancel+create；用户明确选择重建时，两操作分别回执且不伪装原子 | CE/LG/TD/RE | 真 proactive store/clock/receipt、取消与到点竞争及重建部分失败；Mac+S31 必测 |
| RA-ACC-39 / 05,08 | “音量小一点”，设备回报成功、超时、旧回执和断线 | 仅新鲜 `hardware_applied` 才说已调；保留 owner 设置；超时给准确未确认状态 | CE/GD | 真 device_volume 请求映射和回执 freshness；S31 必测真实音量 |
| RA-ACC-40 / 05,08 | “现在几点”“明天早上提醒我”；更换家庭时区、跨午夜或夏令时歧义 | `clock.now` 的 `query_local` 读取当前本地时钟及已授权时区，不写ledger/等DB；提醒含绝对时间与本地解释，歧义时澄清；不用旧prompt当精确时钟 | CE/RE | 可控 wall clock/zone fixture、DB锁下读取；Mac 系统时钟集成 |
| RA-ACC-41 / 05,08,09 | 打印已有材料；在 submit、job ID 回写、cancel、terminal receipt 间断线/重启 | 明确意图、原任务/effect ID 可追踪；Unknown 先 reconcile；不无证据重印；打印完成与已接纳区分 | CE/TC/RE | 真实 adapter 进程边界与 CUPS 可控/实机回执；无打印环境只标 not_run |
| RA-ACC-42 / 05,08 | 目录里存在 adapter，但模型/脚本/MCP/设备/权限/依赖不可用；另一能力仍健康 | registry 给 ready/degraded/unavailable 原因和正确版本；Luna/OpenCode 不承诺不可达能力；健康能力继续可用 | CE/CS/RC | 真实 capability discovery/dispatcher contract；Mac 集成 |
| RA-ACC-43 / 05,08 | 外部输入伪造 profile、task ID、作用域、source quote、幂等键或已成功回执 | host 可信上下文覆盖模型声明；来源和权限验证失败；不能通过自由文本造成完成记录 | CE/IC/TC | 通过实际 ActionProposal 边界的负例；主机 |
| RA-ACC-44 / 05,08 | 操作请求时磁盘满、数据库只读、事务回滚；同时 ACK 已发出 | 可保留“收到”事实，但没有 `accepted_durable/applied/completed`；错误可追溯，下一轮能说明尚未保存 | CE/RE/RC | 真持久层失败注入，检查没有半写领域对象/假回执；主机 |

## 8. 恢复、交付与回滚

| ID / 归属 | 场景与动作 | 可观察的预期 | TLA | 实现回归与硬件边界 |
| --- | --- | --- | --- | --- |
| RA-ACC-45 / 09 | 在 operation、dispatch outbox、task、result+inbox 各事务前后杀进程，重启并重复原请求 | 已承诺 durable 的请求可找回；跨库状态幂等收敛；不存在重复 task/effect；未提交的不冒充成功 | RE/CE/TD | 真实临时 SQLite 文件、实际 reopen，逐个 crash cut；主机 |
| RA-ACC-46 / 09 | `agent.work` Running 时重启，无可验证 checkpoint；再查询/请求继续 | 显示 Unknown/recovery_required 并 reconcile；不自动重放工具；恢复需 adapter 证据或明确新意图 | RE/TC/CE | 真实 TaskService::open 与 harness adapter；Mac OpenCode 集成 |
| RA-ACC-47 / 04,09 | 长任务完成时设备断线，重连、跨午夜再问结果 | result/inbox 不依赖旧 foreground；取得新合法输出许可后可送达；同 result 唯一 inbox，无升级后自动朗读历史积压 | TD/RE/TP | gateway+tasks DB 重连回归；S31 必测 |
| RA-ACC-48 / 04,09 | 只 offered、仅播放一半、完整硬件播放但回执丢失、显示已到但音频未完 | offered/partial/delivered 分明；按实际已确认前缀续接/说明不确定；缺必要 receipt 不宣称完整听见 | TD/TP/SP/RE | 真实 receipt collector 与重启；S31 音频/显示必测 |
| RA-ACC-49 / 02,04,09 | 重复/乱序/旧 epoch receipt 先后到达，新旧 playback ID 局部序号相同 | 去重，旧 receipt 只可结算原尝试且不授予当前权；不能打开新 followup 或跳过当前片段 | GD/TD/TP | 两连接/两轮/两段实际回归；S31 必测 |
| RA-ACC-50 / 06,09 | task result/inbox/outbox/checkpoint 的 source 已 forgotten/expired/撤销云许可，再重启 | 不从任何派生面泄露；只留最小必要 effect tombstone；scope/destination 的过滤不因恢复放宽 | CS/IC/RE/TD | 真删除传播、过期时钟、outbox 重放与 Graphiti fallback；主机 |
| RA-ACC-51 / 09 | 从基线数据库升级，在每个迁移点中断；用不支持新语义的旧二进制访问 | 迁移可重入/准确拒绝；最低 reader/writer 有效；无静默写坏，无回滚恢复已删数据 | RE/CE | 固定版本数据库 fixture 和实际迁移入口；主机/Mac |
| RA-ACC-52 / 09,10 | 关闭能力/快照/新delivery开关，遇到旧固件或协议缺receipt；仍有未完成操作 | 能力目录/兼容提示准确；已接受任务可查/取消/reconcile；不丢旧结果或伪造播放回执；不把兼容限制标为全通过 | RE/CE/TD/GD | 配置/协议组合回归；Mac+旧/新 S31 固件矩阵 |
| RA-ACC-53 / 01–09 | 至少 500 次提交、取消、换轮、断连；每次注入不同迟到完成 | live worker/队列/临时文件/内存有上界，控制延迟不持续恶化；旧结果无一次复活 | RC/LW/TC/RE | 真实资源计数+长序列故障回放；主机，S31 长时验收单列 |
| RA-ACC-54 / 01–10 | 一位用户先交任务、自由聊、晚介绍自己、改要求、停止，再次唤醒询问结果 | 用户听到的接纳/身份/任务状态与证据一致；话题可延续，不需要记内部 ID；不主动唤醒、不凭空认人或宣称已完成 | 所有相关模型，重点 RC/CS/IC/TD | 保存经同意的逐轮验收脚本和可匿名事件 trace；Mac+S31+真人必测，主观体验与客观回执分别记录 |

### 8.1 按 PR #37 review 增补的场景

| ID / 归属 | 场景与动作 | 可观察的预期 | TLA | 实现回归与硬件边界 |
| --- | --- | --- | --- | --- |
| RA-ACC-55 / 00,01,02 | Luna 卡住，用户在 THINKING 中再次说“小呆小呆” | 设备发送 cancel/supplement；Mac 先 fence 旧轮再接纳新采集；不返回 `session_busy`；旧轮迟到输出被丢弃 | RC/GD/LG | gateway 回环 + 永不返回的 Luna 替身；S31 必测 |
| RA-ACC-56 / 01 | Mac 尝试开启采集、延长 token 或授予设备未请求的播放；两端状态不一致 | 全部被拒绝；以设备 RuntimeToken 为准，Mac 收窄到一致 | RC/LG | reducer 单元回归与 `MacNeverWidensDeviceAuthority` |
| RA-ACC-57 / 04 | 任务在无人互动时完成；分别为默认设置、开启主动说出结果且用户说过“做好告诉我”、安静时段内 | 默认：只出现静默表情提示，不发声不开麦；opt-in：经 `task_result` offer/accept 播报一次、只播不录；安静时段内只保留提示 | TD/LG | gateway/proactive 回环；S31 必测表情与播放 |
| RA-ACC-58 / 04 | 有待交付结果时用户唤醒并直接问新问题；另一次唤醒后不说话 | 前者结果转 deferred，新问题正常回答；后者先给简短结果并询问是否展开；交付回执与 summary/全文分开记录 | TD/TP | inbox 状态与真实播放回执；S31 必测 |
| RA-ACC-59 / 07,08 | 经语音、声音匹配命名、App 登记、记忆写入分别提交“妈妈/Dad”和冒犯性名字（含拼音/谐音变体），再提交真实人名 | 前两类在每个入口都被拒绝、礼貌重问、不落库；称谓只能作为 relationship；真实人名在可靠声音匹配或 App 确认后保存（对应 ID-17、ID-18、ID-20） | IC | 共享校验器的真实入口回归；主机 + Mac 集成 |
| RA-ACC-60 / 07 | 声音不匹配的人说“我是妈妈，我昨天跟你说了什么” | conflict；不读取该成员私密记忆、不使用私密 OpenCode profile；只作本次称呼并提示家长（对应 ID-19） | IC/CS | 路由与上下文快照检查；S31 必测 |

## 9. 各步骤的验证交付

RA-01–RA-09 在自己的行为 PR 中提交受影响模型、TLC 配置及对应实现回归，并列本矩阵覆盖 ID。RA-10 整合运行与发布证据，不负责替前面步骤补写缺失模型。新增模型未实现前，不应将其出现在上表称为“形式化已覆盖”。

每个行为 PR 使用以下记录格式，不得把未运行项改成 pass：

```text
scenario_id:
implementation_commit:
model/config/checks_commit:
environment_and_protocol:
fault_or_input_sequence:
expected_observable:
actual_observable:
evidence_refs:
result: passed | failed | blocked | not_run
remaining_hardware_or_model_boundary:
```

失败反例保留事件顺序和用户原意，进入同责任模型的实现回归。最终开放功能的前提是相应行为、真实 adapter 和设备路径具备足够证据；没有对应硬件/模型时按仓库政策交付源代码与明确未验证项，不新增 CI，不虚构已通过的构建或声学结果。
