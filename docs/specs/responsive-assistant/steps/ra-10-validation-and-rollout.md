# RA-10：集成验收与分阶段开放

日期：2026-11-25。状态：**待实现**。验证：**not_run**。

依赖：**RA-00–RA-09 与 RA-07b 全部完成各自的设计/实现交付，并明确剩余验证状态**。关联总议题：[#36](https://github.com/JasonGuoo/esp-bot/issues/36)。实施基线：`46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`。

本步骤把已分步完成的模型、回归、Mac 集成与 S31 体验证据串起来，决定哪些能力可以开放。它不把前面步骤的 formal/回归工作推迟到项目末尾，也不允许将源代码合并等同于设备通过。当前只有规格，没有执行本步骤。

## 1. 目标与验收入口

验收入口为 [acceptance.md](../acceptance.md) 的 RA-ACC-01–RA-ACC-64。每个场景已经列明动作、可观察预期、TLA 责任、实际代码回归和硬件边界。

需要回答的产品问题只有经过证据才能标为完成：

1. 任意后端挂起时，小呆是否仍能处理 Stop、连接变化和下一轮接纳？
2. 后台任务尚未完成时，用户是否能继续正常聊天、查看状态、取消、补参和修改要求？
3. 用户在第 30 轮才介绍自己，系统是否只使用有效证据连接身份与历史，并清楚表达仍未知的部分？
4. 个人助理能力是否真的通过模型入口可发现、调用、取消和查询，且回答取决于真实回执？
5. 崩溃、重连、午夜、删除与回滚后，这些保证是否仍成立？

## 2. 源码、模型与拟议变更位置

| 位置 | 本步骤的具体责任 |
| --- | --- |
| `server/bot-server/src-tauri/src/brain/gateway.rs`、`brain/gateway/segments.rs`、RA-01/02 新 control/gateway 模块 | 审计 control loop 所有可达调用；实际 reader/writer/backpressure、pending commit、delivery/input 分离回归 |
| `brain/fast_core.rs`、`brain/worker.rs`、`server/xiaodai-core/tests/codex_fake.rs`、RA-03 新 worker 边界 | Luna 线程/补充/取消、同 lane 与跨 lane 竞争、迟到输出隔离；保留既有有序前缀与补充反例 |
| `brain/tasks.rs`、RA-04/06 的 inbox/context/task control 模块 | 耐久接受、任务版本、结果/送达正交、query_snapshot 无 DB 写依赖、跨库 reconciliation |
| `brain/capability_registry.rs`、`brain/capability_dispatch.rs`、`brain/assistant_adapters/`（均由前序步骤拟新增） | 目录与实际 adapter/readiness 一致；每项能力的真实回执、取消和失败路径 |
| `memory/identity.rs`、`memory/voice_album.rs`、`memory/household.rs`、RA-07/09 workflow/operation 扩展 | 晚登记、多人证据、名字冲突、来源撤销、迁移与崩溃恢复 |
| `firmware/components/bot_runtime/`、`firmware/components/brain_link/`、`firmware/HOUSEHOLD_INTEGRATION.md` | S31 的输入许可、唯一音频所有者、半双工、本地 Stop、协商与真实硬件回执 |
| `formal/tla/`、`tools/check-tla-twins.sh` | 使用现有本地模型/实现双验收入口；新增模型的 `.tla/.cfg/checks.sh` 由所属步骤提供 |
| `docs/specs/responsive-assistant/acceptance.md` | 记录实际执行结果与场景证据链接；不可仅打勾替代证据 |
| 拟议 `docs/review-responsive-assistant-YYYY-MM-DD.md` | 实施完成后的独立验收记录，记录实际 commit/环境/结果；本次不创建假验收报告 |

除 `server/xiaodai-core` 和固件外，表中短路径相对 `server/bot-server/src-tauri/src/`。实现回归优先放在现有模块测试及正式模型的 `checks.sh`；不增加 GitHub Actions 或为展示覆盖率单独建立测试平台。

## 3. 验收接口与状态

### 3.1 每次验证的记录

```text
ValidationRun {
  run_id, implementation_commit, model_commit,
  scenario_ids[], environment, app_and_firmware_versions,
  protocol_features, catalog_version, feature_flags,
  result: passed | failed | blocked | not_run,
  trace_refs[], counterexamples[], unverified_boundaries[]
}
```

执行条件不具备时记 `not_run` 并写清缺什么；实际尝试被环境/工具链阻止记 `blocked`；观测违反预期记 `failed`。旧修订的通过证据只能作为背景，不能改变新代码的状态。

### 3.2 功能开放状态

```text
documented → implemented → host_evidence → mac_evidence → device_evidence
```

这是一条交付证据链，不是操作运行状态。只需要主机的能力可以在其证据边界内开放；涉及声纹真实效果、设备音量/停止、播放完成和麦克风门的行为必须保留对应 Mac/S31 验收门槛。状态不替代 RA-05 的实时 readiness：某能力曾验收通过，当前设备离线时仍应 unavailable/degraded。

`query_snapshot` 返回 `read_result(request_id, snapshot_version, source_refs, as_of, stale)`，无需写操作账本、无需 `OperationRef`、不能发 `accepted_durable`。RA-08 的 `clock.now` 以 `query_local` 读取有界的当前本地时钟及已经授权的时区，同样不写账本、不等 SQL/网络；不能用旧 prompt 的时间替代当前观察。mutation/workflow 使用统一操作账本；真正提交后才返回 `accepted_durable`。测试必须包含 DB 被锁而只读状态仍能回答“截至某时的最近状态”，同时遵守即时撤销栅栏。

## 4. 详细实施与验证顺序

### 4.1 第一步：收齐每个步骤自己的证据

1. 为 RA-01–RA-09 列出受影响场景、TLA 文件/config/checks、实现回归位置和执行结果。缺失模型或只测 happy path 的步骤返回其责任步骤补齐。
2. 核对所有新模块/字段与规格一致；状态名称、ID、version、来源引用、权威数据库统一。确认 task revision、task input revision、capture input revision、source revision、operation revision 没有互相替代。
3. 检查整个调用链，而不只看发生改动的行。特别查 gateway 的 timer、错误、supplement、proactive、task result、receipt 分支；核对 warm helper 是同步准备还是仅启动已有后台任务，避免错误拆走已经隔离的逻辑。
4. 核对实际持锁范围。Luna 的 generation/thread 串行锁可以留在 worker 内；主循环不能等它。enrollment/identity commit 不应在模型提取期间持全局 turn 写锁，也不能由前台持读锁后重入写锁。
5. 将静态审查发现的可阻塞路径放进故障注入回归；没有 `.await` 的同步 SQLite、事件 sink、回调、序列化、文件读写、析构和 join 也在范围内。

### 4.2 第二步：Formal 与确定性实现回归

按照 [TLA+ 孪生索引](../../../../formal/tla/README.md) 的本地环境运行受影响模型，保留完整反例/配置和对应实现结果。现有入口示例：

```sh
./tools/check-tla-twins.sh protocol/UtteranceCommit
./tools/check-tla-twins.sh macos/ReplyTiming
./tools/check-tla-twins.sh protocol/Supplement
./tools/check-tla-twins.sh protocol/TurnPlayback
./tools/check-tla-twins.sh
```

新模型只有在所属步骤真正建立后才加入执行清单。本次文档 PR 没有执行以上命令。配置好的 Mac 上的 Java、Rust、C/ESP-IDF 头文件、Apple 工具链和缓存要求遵循既有索引，不另设 CI。

每个新模型至少核对：

- state/action 与实际 owner 对应；没有将复杂外部工作抽象成 control 内“瞬间完成”的一步。
- 队列容量有限，包含 full、关闭、重复、迟到、取消、重启/epoch 交错。
- control 活性与后台生成活性分开。不得用 `WF(backend_completes)` 来满足“后端永不返回仍能 Stop”。
- 删除/撤权、版本冲突、Unknown effect 是状态，不是测试默认忽略的错误分支。
- 既有不变量没有为了让 TLC 通过被削弱；变更性质或 fairness 时解释原因与新的产品限制。
- 每个反例的事件顺序在实际 implementation test 中可重现；模型通过与实现通过分别记账。

实现故障注入需穿过真实边界：SQLite 锁要用真实连接/事务，socket 压力要有真实不读 peer/有限 writer 队列，queue full 要填实际 channel，子进程取消要观察实际退出/隔离与 slot 计数。只让 stub 返回 `busy` 不能证明容量用尽时仍响应。

### 4.3 第三步：Mac 集成与能力盘点复核

使用前序步骤的能力目录逐项检验模型入口、host 校验、adapter、领域回执和用户可见反馈。至少覆盖记忆保存/删除、任务查询/取消/补参/改要求、身份声明/登记/纠错、提醒、音量、时间、内容准备/播放和明确打印请求；不存在的邮件/日历等集成保持 unavailable/未实现，不能靠 OpenCode 泛用工具推断已交付。

先以已存在的本地/受控 adapter 验证状态与失败路径，再在配置好的 Mac 上记录真实依赖调用。需要真实模型/打印机的项目单列授权环境和验证证据。本次规格编写不启动任何此类调用。

重点交叉测试：

| 组合 | 必须观察到的行为 |
| --- | --- |
| task Running + 10 轮聊天 + query_snapshot | 前台槽反复正常释放；快照能报告任务；status 不为写账本等待 DB |
| Running amendment + cancel + 旧 attempt finish | 新输入版本/取消决定有效；旧结果无法复活；旧 effect 证据仍可追踪 |
| late identity + memory delete + Luna 已有上下文 | 不补写越权历史，不重放操作；旧线程/generation 失效 |
| DB busy + 接纳/保存 ACK | 临时收到与耐久接受分开；无假成功；控制面仍处理 Stop |
| task completed + 断线 + 重连 + repeated receipt | inbox 不丢，重新裁决输出；只据真实 receipt 结算 delivered |
| restart during agent.work/print | Unknown 可查询/reconcile；没有盲重放 |

### 4.4 第四步：S31 半双工与真人脚本

使用实际 **ESP32-S31-Korvo-1 V1.1**。只刷 app 分区，并在验收记录中附 `idf.py size` 及相对约 34 KB 余量的消耗（见[实施计划 §4.1](../implementation-plan.md#41-固件预算与刷写)）。保留 owner 音量、现有配对与模型分区；沿用既有板上验证流程。验收不默认开启 AEC，不把声学打断当作本方案前置保证。

执行 acceptance 的 S31 必测项，覆盖完整唤醒词、触摸补充、本地 Stop、播放回执、缺尾帧、断网/重连、提醒不录音、长任务期间连续聊天、多人晚登记。记录屏幕/扬声器/麦克风门与主机事件的对应关系，不仅看 App 文本框。

晚登记脚本至少包含 30 轮、两个人和三个变体：有合法保留的干净声学证据、仅文字/未启用收集、样本混人或被删除。观察称呼与历史归属的范围是否准确；不能因为一段示范中猜对姓名就宣称声纹识别已经可靠。

最后请真人按自然说法进行一条连续任务链：交办资料准备、闲聊、介绍姓名、查询状态、更改要求、停止当前讲话、再次唤醒、取回结果。客观状态/回执与用户是否感到被记住分别记录。主观满意不能覆盖权限或重复 effect 错误。

### 4.5 第五步：性能与资源上界

沿用 acceptance 的 proposal 定义，采样 p95/p99，分别计：socket 可读→event 接纳、event 接纳→控制裁决、commit→完整输入、完整输入→ACK、首内容、TTS 首包、硬件首声、取消→资源退出/隔离。

ACK 在本地/缓存素材可用且输出许可满足时测；writer 入队并非网络发送/硬件播放。Stop 撤销当前 token，不应禁止下一次合法 activation。队列/DB/网络永久挂起用例看控制进展和有界降级，不能把超时退出的样本从延迟分布删掉。

进行 RA-ACC-53 的重复故障序列，观察每轮后 live worker、子进程、队列、临时文件、输出租约、内存和 task/operation 待核对数量。确定泄漏或不可回收资源时暂停该能力开放，修复责任步骤；不得仅提高容量阈值掩盖积压。

## 5. 分阶段开放与回滚决策

| 阶段 | 开放内容 | 进入条件 | 未满足时的行为 |
| --- | --- | --- | --- |
| 文档评审 | 设计、接口、规格与验收计划 | 当前 PR，只需文档一致性及链接/基线审查 | 不更改运行时默认行为；全部新验证保持 not_run |
| control/worker 隔离 | 新接纳/reader/writer/Luna worker 边界 | RA-01–03 各自模型与实现证据，旧协议兼容清楚 | 保留兼容路径并显示限制；不宣称全功能响应保证 |
| 独立 task delivery + context | 新前台槽/inbox、有效快照、只读状态、任务控制 | RA-04–06，长任务新聊、receipt、版本竞争通过相应环境验证 | 未证明的 mutation 不列 ready；可保留安全 query_snapshot |
| identity + adapters | 对话式身份/记忆/提醒/设备能力 | RA-07/08 的真实 adapter 和对应 Mac/S31 证据 | 明确 unavailable/需要补采样/缺设备；不口头假成功 |
| 恢复与广泛开放 | 持久 workflow、迁移、重启、断线续取 | RA-09 和本步骤的组合验收满足目标环境范围 | 禁止自动重放 Unknown effect；保留查询/reconcile/取消路径 |

开关依赖、协议协商与数据库兼容按 RA-09 实施。回滚优先撤下未验证的新入口并保留已接受操作的查询/取消/核对；不能丢 operation/inbox、撤销幂等规则或从旧备份恢复被删除资料。缺关键 receipt 的旧端可降级，但不得在同一验收报告里被计为完整通过。

## 6. 异常、取消与验收终止条件

发现任何旧结果复活、私域/遗忘内容泄露、无证据重放外部 effect、被动录音、control 在永久挂起后端下无法停止，应停止开放受影响能力，保留复现和反例，回到责任步骤修正。清理测试创建的数据时只操作隔离测试范围；真实用户任务/身份/提醒不能为了测试“重置环境”被删除。

若缺 Mac、S31、模型或打印环境，按项目政策交付可审查源代码与明确未运行项，不擅自创建 CI，不伪造集成完成。只要尚有关键环境未验收，最终说明须限定已证明的层次。

## 7. 完成定义

- [ ] RA-01–RA-09 每一步均有自己的 formal/实现回归映射；实际执行或 not_run 原因明确。
- [ ] RA-ACC-01–RA-ACC-64 的覆盖、结果和证据可定位，没有用一个 smoke test 代替不同失败路径。
- [ ] control 可达调用图审查覆盖 await、同步阻塞、callback、锁、析构、网络和存储；修复没有破坏已有 worker 内必要串行规则。
- [ ] 有真实容量饱和、socket 背压、DB 锁、忽略取消、旧 epoch、资源清理及重复故障序列的证据。
- [ ] 主机/Mac/S31/真人证据分别记录；性能目标全部标明 proposal 或真实样本统计，绝不混写。
- [ ] 能力清单与模型/UI/SDK 实际可达路径、readiness、操作/硬件回执相符；只读查询无需数据库写入。
- [ ] 迁移/恢复/回滚方案通过其适用环境验证；Unknown 外部 effect 不自动重放，删除不会被恢复。
- [ ] 最终验收记录说明开放范围、未验证部分、功能开关与恢复办法；无需新建 CI。

当前本步骤仅有规格，上述条目均未完成，执行状态为 **not_run**。
