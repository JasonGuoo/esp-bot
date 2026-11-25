# RA-03：Luna 作为可隔离的语义 worker

**状态：待实施；仅规格，未改 Luna/Core 运行代码，未运行本步骤测试。日期：2026-11-25。** [总规格与 issue](../README.md) · [目标架构](../architecture.md)。核对基线：`46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`。

## 目标与依赖

**依赖：[RA-01](ra-01-control-contracts.md)、[RA-02](ra-02-gateway-io.md)。** 将 Luna 的会话、推理、重置、路由、输出准备和取消等待完全放在控制外的语义 worker 中。主循环可以在 Luna 不返回时继续控制；Luna 仍是现有快速对话核心，不成为主循环，也不被另一套 Agent runtime 替换。

当前 `Core::start_messages` 已 spawn 推理；本步新增价值是调用边界、有限接纳、版本化结果、独立取消和可信结构化输出，不能把“再包一个 spawn”当成完成。

## 源码落点

| 文件 | 当前职责 / 拟议改动 |
| --- | --- |
| [`brain/fast_core.rs`](../../../../server/bot-server/src-tauri/src/brain/fast_core.rs) | 既有 Luna lane、上下文、2 秒 HTTP 竞速、流式句子和 TTS；保留路由与顺序，改成 worker 输入/输出适配 |
| [`brain/orchestration.rs`](../../../../server/bot-server/src-tauri/src/brain/orchestration.rs) | 接纳 ASR/身份/上下文结果后派发语义命令，不把语义等待带到 control；保留普通回答不等身份 |
| [`server/xiaodai-core/src/core.rs`](../../../../server/xiaodai-core/src/core.rs) | 已有 `Turn`、spawn、候选路由竞速与 Event；审计 stream 容量、取消与 loser 输出 |
| [`server/xiaodai-core/src/codex.rs`](../../../../server/xiaodai-core/src/codex.rs) | 保留单 thread gate、steer/interrupt 与会话重建；修正 ACK/早到通知竞争，限流内部事件 |
| [`server/xiaodai-core/tests/codex_fake.rs`](../../../../server/xiaodai-core/tests/codex_fake.rs) | 扩展真实 core 协议替身测试，覆盖挂起、乱序、过载与 reset |
| [`brain/persona.rs`](../../../../server/bot-server/src-tauri/src/brain/persona.rs)、[`brain/flow.rs`](../../../../server/bot-server/src-tauri/src/brain/flow.rs) | 继续维护同一前台人格和明确道别规则；不能让控制模板假装完成语义判断 |
| **拟议新增** `server/bot-server/src-tauri/src/brain/luna_worker.rs` | 有界 command mailbox、语义 worker 状态、core bridge、deadline 和 supervisor 接口 |
| **拟议新增** `formal/tla/macos/LunaWorker/` | 同名模型、配置、真实 adapter/core 测试入口 |

RA-01 的 `control_contracts.rs` 是唯一 envelope/proposal 定义，不在 worker 中另造一套同义协议。RA-05 才接真实 capability registry；本步保留其输入接口，但没有 registry 时不得假装工具可用。

## worker 契约

### 1. 接纳与状态

拟议 `LunaCommand` 包含 `Respond`、`Supplement`、`ResetContext`、`Warmup`；均由 host 绑定 `ForegroundRef`、source、snapshot/catalog version、上下文引用与 deadline。取消/撤权使用 RA-01 独立 fence，不排在普通 Respond 后等待。

每个家庭/thread 保持一个受控的语义执行序列；首版建议只有一个正在处理和至多一个明确接纳的待处理请求，具体常量须在实现中冻结并加入容量测试。待处理请求不是无提示地“latest wins”：若显式 supersede，旧请求要有 cancelled/superseded 终态；否则新请求不得进入 Luna thread，也不偷偷丢弃已接纳输入。多家庭/worker 的总量也有限。

**不能让现有退路退化。** 目前 Luna 卡住并不阻挡新回答：`core.rs` 在 `fallback_after_ms` 后启动 HTTP 路线，Luna-first 健康策略（`core.rs:408-445`）在 Luna degraded 时让最快的近期 HTTP 路线于 t=0 领先。因此“一个处理中、至多一个待处理”的限额只约束 **Luna thread 尝试**，不约束整轮：Luna 会话 gate 超过期限仍被占用时，新的 fresh 轮次由 HTTP 路线在无 thread、无 steer 的情况下回答，并记入现有 Luna 延迟日志。worker_busy/overload 只在所有路线都饱和时返回。健康来源复用 `xiaodai-core` 已有的延迟日志与健康策略，不另建 `HealthChanged` 来源。

拟议 worker 状态为 idle、running、cancelling、unhealthy/restarting，另带有限 pending command。控制保存其**观察快照**，不等状态变 idle 才处理 Stop 或 UI。worker 等内部会话 gate 时仍受 deadline/cancel 约束；supervisor 可回收或隔离不合作的进程。

不能为每个新输入无限创建 Core、HTTP race 或 fallback task。后端全部挂起时，控制可说明“语音回答暂时不可用”，但不能承诺任意新语句已理解；如果 ASR 也挂起，只有本地触摸/明确协议控制仍可靠可用。

### 2. 有类型的输出

| 拟议输出事件 | host 处理 |
| --- | --- |
| `SpeechSegment` | 校验 source、foreground、连续序号与正文上限，交呈现准备；不能直接写 socket/驱动音频 |
| `ActionProposed(ActionProposal)` | 只交统一验证入口；`capability_id/parameters/target_handle?` 均非权限。RA-05 未启用则明确 unsupported 或沿既有转交路线 |
| `Escalate` | 单次明确转交，继续使用既有 OpenCode；不把“转交”文字读给用户，也不复制副作用 |
| `ProducerClosed` | producer 已封口及完整/partial/failed 等生成结果；不等于已播放或任务成功 |
| `WorkerFailed / HealthChanged` | 版本化错误/健康事实（健康来自 `xiaodai-core` 既有延迟日志与 Luna-first 策略）；controller 决定状态与既有 fallback，不能把错误当内容完成 |

所有输出带 RA-01 envelope。只有当前 generation 的前台结果可申请输出；已接受持久任务的结果进入 RA-04 的 operation/task 路径，不能为继续使用旧前台输出而换贴 token。

结构化提案与可朗读文本使用分开的类型/字段。任何自由文本、字幕、引用或部分 JSON 都不能被扫描成可执行工具。收到当前请求中该提案的完整消息边界、解析完成且 schema/长度/数量正确后，才可提交一份提案；若 provider 支持独立完整的结构化事件，不必等待整轮回答结束，若只提供整轮 JSON 则不能执行尚未闭合的片段。提案去重由宿主 operation/幂等逻辑承担。model 不拥有任意 shell、数据库、声音路径或 OpenCode/MCP 的直接授权。

`query_snapshot` 返回合法缓存的 `read_result`；`query_local` 返回经过审计的有界宿主观察，例如 `clock.now` 的系统时钟/授权时区采样。两者可无 operation，不写操作账本、不等 DB/网络/I/O，不能让 Luna 猜时钟或身份事实；mutation/workflow 必须经过持久接纳回执。Luna 只能据真实回执说“已安排/已记住”，不能依据自己输出的提案说成功。动作关联的接单/成功句由 host 在相应 receipt 到达后放行或渲染；不能仅靠 prompt 要求模型诚实，却让带“已记住”的 proposal 文本先进入 TTS。

### 3. 保留顺序锁，隔离其等待

[`CodexEngine::turn`](../../../../server/xiaodai-core/src/codex.rs) 维护同一个会话 thread 的顺序，`fast_core` 还有 lane 与 supplement reservation。保留这些约束；删除锁并让两轮同时写同一 thread 会串上下文。

外层取消先使旧 generation 不可发布，再异步发 interrupt 和回收。worker 可以在内部等该 thread 到安全边界；期间控制和其他独立任务继续。达到监督 deadline 后，按既有受控恢复方式隔离/重建受影响 core，旧 epoch 结果无效；不能让恢复循环自动执行旧工具或复述已经播出的内容。

新家庭日、`memory_changed`、`identity_revoked` 的 context reset 仍需要有序处理。受影响来源/策略先撤权，worker 在安全边界换 thread/重建上下文；身份晚到只是标注更新，不一律 `cancel_all`，也不重放该轮工具。精确的来源传播在 RA-06/07/09 实现。

### 4. 显式 supplement 的竞态

该竞争即 [#35](https://github.com/JasonGuoo/esp-bot/issues/35)，与 reducer 无关，应按 [RA-00](ra-00-quick-decoupling.md#并行的独立修复35-steer-竞争) 作为独立小 PR 先行修复，不等待 RA-01/02；本节描述其设计。[2026-11-20 voice review](../../../review-voice-interaction-2026-11-20.md) 记录的 `turn/steer` ACK/事件竞争：provider 可能先处理/发出 `userMessage`，调用方随后才从 ACK await 恢复，若此时才设置 `steer_pending` 就可能漏掉早到事件。

拟议在发送 request 前安装 request-scoped handoff barrier；reader 可在有界范围内保存早到的相关通知，ACK 成功后按 exact turn/request 证据提交 handoff。ACK 拒绝、超时、turn 已封口或被取消时显式撤 barrier，丢弃不适用候选并沿既有 fresh-turn fallback。不能靠 sleep 规避竞争，也不能让 rejected steer 的早到输出流入新 sink。

supplement 只在授权的相邻 runtime/turn 复用；正文前缀仅取完整播放回执，不取已生成、已发送或半句内容。普通新问题使用 `respond_fresh`，不能隐式 steer 上一个人的语句。

## 有界流与反馈

目前 vendored core/codex 存在 unbounded event/subscription channel。外层加一个 bounded mailbox并不能证明整体有界：若 bridge 阻塞，上游仍可能无限积压。因此实现须审计 core 事件、RPC 出站/订阅、sentence/TTS future、待重建上下文以及 terminal 回执的所有缓冲。

给 token/文本字节、句子数、pending TTS、订阅与待终结请求设上限；普通 token 可合并到同一个有界连续片段，最终状态使用保留容量。达到预算时停止该生成/媒体准备并标记 partial 或失败；不能跳过中间内容继续读尾部，也不能静默丢终态。使用 bounded channel 时不得把等待队列空位重新传到 control。

第一句已冻结并开始输出后，其文本不可改写。保留当前 TTS 准备可以并行、发布按序，以及最多 16 段/默认每段 200 字等既有呈现限制；具体值如要变更必须单独评估与回归。末尾 authoritative item 只能补已验证的连续后缀，冲突则显式失败。

长等待反馈由 control 的已知阶段/本地素材提供，不等待 Luna 生成 ACK。保留 idle-only ACK warmup、唤醒/近期 ACK 抑制及首正文 TTS 优先，不因 worker 重启再次自动排 ACK。前台人格仍是小呆；不同 provider/后台任务不直接轮流抢话。

## 实现清单

- [ ] 将现有 fast core 调用封装为有界 worker command/result；保留现有 Luna-first、HTTP race、错误转交与配置关闭路线。
- [ ] 定义 enqueued、producer running、content frozen、producer closed、presentation completed 的不同事件；不拿某个 final token代表全链路完成。
- [ ] 保留会话 gate/lane 和显式补话许可，把其等待完全留在 worker；取消使用独立 fence。
- [ ] 使 deadline、cancel、reset 与 health 事件均带当前 epoch；限定待处理请求、恢复次数、同时竞速 provider 和残留句柄。
- [ ] 审计并约束 core/codex 内部 unbounded channels，补 partial/error 的非丢失终结路径，避免只修最外层。
- [ ] 增加结构化 proposal bridge 与 schema 验证；RA-05 未接线时保持不可执行；不得允许文本指令直接调用工具。
- [ ] 为 context 接口预留同一权限范围内的 identity/task/inbox snapshot（RA-06 填充）；不得同步等 DB 或身份才回应普通问题。
- [ ] 实现 request-scoped steer barrier、早到事件处理与失败 fresh fallback；保留已播不可变前缀。
- [ ] 增加模型挂起/不合作取消/重启/过载/旧事件测试及对应 TLA；保留并复跑相关既有回归。

## 关键案例

| 类型 | 输入/故障 | 预期 |
| --- | --- | --- |
| 正常 | Luna 流式回答，后一句 TTS 先完成 | 发布仍按语义顺序，producer closed 与真实呈现完成分开 |
| 失败 | Luna、HTTP、OpenCode 全部永不返回 | control 继续 Stop/状态；有限 worker 挂起或超时，不无限 fallback/spawn，不承诺新语义理解 |
| 取消 | 已入队的旧 token/句子在 Stop 后到达 | 丢弃旧输出，不等 interrupt ACK；已播前缀保留事实记录 |
| 过载 | 模型高速输出/consumer 停读/满 pending TTS | 总内存有界，显式 partial/失败，不读“头尾跳中间” |
| 补话 | userMessage 早于 steer ACK；随后 ACK success/reject/timeout | 成功只发生一次 handoff；拒绝/超时无旧输出泄漏，fresh fallback 保留原许可边界 |
| 顺序 | 新家庭日/记忆撤回与回答交错 | 来源先撤权，thread 有序重置；后台已接受任务不因换日丢失 |
| 能力 | 非法 JSON、模型自填权限/voice path、重复 proposal | 不执行；返回明确拒绝/不支持；身份/工具事实不被模型语言伪造 |

## TLA 与实现回归

拟议 `formal/tla/macos/LunaWorker/LunaWorker.tla`：两个轮次、两个 provider、一个有序 thread、有限 pending/output、早到 steer 事件和不返回的 worker。进展性质只对控制调度和已可用消息提出公平性；不假设 provider 最终完成。

| 拟议性质 / 既有模型 | 对应回归 |
| --- | --- |
| `SingleThreadOrder` | 同一 thread 不交错接纳两轮；取消在 worker 内等待不阻挡 control |
| `CurrentGenerationOnly` | loser route、旧 terminal、已取消 worker 与旧 reset 回调不能发布到新轮 |
| `BoundedSemanticWork` | 洪泛 token、pending TTS、队列满时容量不增长且有明确终态 |
| `HandoffOnce` | steer 早到通知、ACK 重复/拒绝/超时/取消的所有顺序 |
| `NoToolEffectFromText` | 自由文本和未验证 proposal 不触发 capability；同请求重复提案不重复执行 |
| 既有 `ReplyTiming` / `Supplement` / `ConversationClose` | 身份晚到不阻挡回答；补话来源正确；道别与故事不混淆 |

扩展 [`codex_fake.rs`](../../../../server/xiaodai-core/tests/codex_fake.rs) 的 `old_nested_turn_completion_cannot_finish_the_current_question`、`next_turn_waits_for_the_interrupted_turn_to_complete`、`fresh_question_cannot_steer_a_previous_speakers_turn`、`steer_rejected_falls_back_to_a_new_turn` 等场景；新回归要穿过真实 worker/core adapter。保留 [`fast_core.rs`](../../../../server/bot-server/src-tauri/src/brain/fast_core.rs) 的连续正文前缀、分段预算、TTS 顺序和路由测试。

计划使用 `./tools/check-tla-twins.sh macos/LunaWorker` 及受影响现有模型；按[formal 规范](../../../../formal/tla/README.md)记录 TLC、实现、真实 provider 和实板四种不同证据。

## 兼容、迁移与回滚

先在新控制/worker 边界保持现有纯文本输出与 `〔转交〕`，再通过显式版本启用结构化提案。旧核心不支持新 schema 时使用明确能力声明与旧转交路线，不解析任意自然语言为动作。`brain_core.enabled=false` 继续可回到既有 OpenCode 路由，且该路由也在控制外运行。

worker 热切换必须关闭旧接纳、推进 generation、处理已冻结输出和未确认效果，再创建新实例；不双写一个 Luna thread。回滚不能重放未决工具、丢失已播前缀或恢复被撤回的记忆。保留现有会话顺序锁和权限边界，不能用“回滚到兼容”绕过它们。

## DoD 与运行状态

- [ ] gateway/control 不等待 fast core、context reset、warmup、provider interrupt 或 worker join。
- [ ] 一个挂起的 Luna 不阻挡控制；挂起时可解释的事实与不可承诺的语义能力有测试和用户文案。
- [ ] 会话顺序、显式 supplement、早到通知、输出代际与结构化提案全部有真实 adapter 回归。
- [ ] 所有生成/订阅/TTS/恢复缓冲有界；过载输出连续 partial，真实回执不伪造。
- [ ] 不新增第二套 Agent runtime；既有 OpenCode、路由配置、关闭开关可用。
- [ ] 对应 TLC 与回归记录完成，真实模型/听感/实板未运行项单独列出。

**当前运行状态：所有实现与验收均待执行；本文不表示 Luna worker 隔离已完成。**
