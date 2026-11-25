# RA-00：Mac 侧快速解耦（无协议变更）

**状态：implemented, host-tested, pending board acceptance。日期：2026-11-25（按 PR #37 review 增补；M0 主机实现于其后）。** [总规格与 issue](../README.md) · [实施计划](../implementation-plan.md)。核对基线：`46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`。开关：`responsive.quick_decouple`，默认 `true`。

## 目标与依赖

**依赖：无。** 在建立完整 control reducer（RA-01/02）之前，先用最小的 Mac 侧改动消除用户最能感到的三处耦合：后台任务占住下一轮、待补参任务把所有闲聊推给 OpenCode、`select!` 分支内的少数已知等待。本步**不改 WSS v3 字段、不改固件、不刷机**，回滚只需关闭一个 Mac 配置开关。

RA-00 为后续 RA-01/02 留出接缝：后者以 strangler 方式在这些接缝后逐步抽出 reducer 与 I/O，而不是作为任何用户可见改进的前置条件。

## 源码落点与改动

| 文件 | 当前证据 | 本步改动 |
| --- | --- | --- |
| [`brain/gateway/segments.rs`](../../../../server/bot-server/src-tauri/src/brain/gateway/segments.rs) | `next()` 仅在 `self.tasks.is_empty()` 时把段标为 `final`（L36）；`report()` 同样以 tasks 为空结束交付（L104） | 任务已 `accepted_durable` 时，把本轮 ACK/说明作为真实 `purpose=final, final=true` 内容段发送；`Delivery` 不再持有后台 `tasks` |
| [`brain/gateway.rs`](../../../../server/bot-server/src-tauri/src/brain/gateway.rs) | `audio.begin` 等在 `delivery.is_some()` 时返回 `session_busy`（L1331）；`task_clock` 轮询后台任务（L593） | busy 只由真实前台/音频状态判定；任务结果改走 RA-04 收件箱，不再由当前 Delivery 等待 |
| 同上 | `finish_plain_input(...).await`（L1345）、`turn.task.stop().await`（L1099）、`prepare_failure`/`prepare_proactive`（L957、L1144）在分支内等待 | 先推进 fence/撤权，再把等待移入受限后台 task，以事件回到循环；保留既有回执与顺序 |
| [`brain/fast_core.rs`](../../../../server/bot-server/src-tauri/src/brain/fast_core.rs)、[`brain/orchestration.rs`](../../../../server/bot-server/src-tauri/src/brain/orchestration.rs) | `orchestration.rs:203-224` 收集同日任意 `AwaitingInput` 任务，`fast_core.rs:260` 随即返回 `pending_task_input`，所有普通闲聊都走 OpenCode | 仅当本轮明确指向待补参任务（上下文中的 task handle，或在同一 activation 内紧接补参问题回答）时转 OpenCode；否则由快核心回答，任务继续等待 |
| [`brain/timing.rs`](../../../../server/bot-server/src-tauri/src/brain/timing.rs)、[`brain/latency.rs`](../../../../server/bot-server/src-tauri/src/brain/latency.rs) | 已有分阶段计时与控制台 p50/p95 | 改动前后用同一计时对比，记录到验收记录 |

既有契约已允许这一做法：[模块契约 §9](../../../module-contracts.md) 写明“final 内容段可以在独立 Mac 任务仍在处理其他请求产物时完成播放”。耦合在 Mac 代码，而不在设备契约。

## 并行的独立修复：#35 steer 竞争

[#35](https://github.com/JasonGuoo/esp-bot/issues/35) 的 `turn/steer` ACK 与早到通知竞争不依赖 reducer。按 [RA-03 §4](ra-03-luna-worker.md#4-显式-supplement-的竞态) 的 request-scoped barrier 单独提交小 PR，扩展 `codex_fake.rs` 的既有用例；设备上每次“补一句”（`bot_runtime.c:816-820` 的思考中轻触）都会走这条路径。

## 必须覆盖的案例

| 案例 | 预期 |
| --- | --- |
| “帮我查资料”被接单后立即问“树叶为什么是绿的” | 第一轮以真实 final 段结束；查资料任务继续；新问题由快核心回答，不返回 session_busy |
| 存在待补参任务时闲聊“今天好冷” | 快核心回答；任务保持 awaiting_input；只有明确回答补参问题才进入 OpenCode |
| 接单写库失败 | 不发送“已安排”的 final 段；如实说明未接下 |
| PCM 尾帧晚到时触摸 Stop | 本地撤权立即生效；后台补帧结果按 fence 丢弃 |
| 关闭 RA-00 开关 | 恢复基线行为；已接纳任务与结果不丢失 |

## 固件与刷写

本步固件改动：**无**；app 分区预算消耗 0 KB；不刷机。实板验证只需安装新 App 并复用当前固件。

## DoD 与运行状态

- [x] 后台任务运行期间，合法新前台输入不再因 `delivery`/`tasks` 返回 session_busy。（主机：Delivery 不再挂后台任务；`blocks_new_input` 单测。硬件回执未在板上采集。）
- [x] 委派回复以真实 `final` 段结束；ACK 不伪装任务成功。（主机：失败写库不说「接到这件事」。硬件回执待实板。）
- [x] 待补参任务不再强制普通闲聊走 OpenCode。（主机单测。）
- [x] 三处分支内等待已迁出，fence 先于 cleanup；既有 gateway 回环测试保留并通过。
- [ ] 前后分阶段计时与验收记录分开报告；实板项单独记录。（沿用既有 `latency` 分阶段计时，未做前后对比测量，也未上板。）

**当前运行状态：implemented, host-tested, pending board acceptance。**
