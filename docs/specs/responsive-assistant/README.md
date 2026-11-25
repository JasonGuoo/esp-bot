# 让小呆随时回应：交互解耦与助理能力体系

日期：2026-11-25。总跟踪：[issue #36](https://github.com/JasonGuoo/esp-bot/issues/36)。核查基线：[`46ecdfbd99eba8e9158f8953b9ca251a45bc1d92`](https://github.com/JasonGuoo/esp-bot/tree/46ecdfbd99eba8e9158f8953b9ca251a45bc1d92)。

**状态：设计与分步规格已编写，待审阅；RA-00～RA-10 运行时改造尚未实现。** 本次仅修改文档，不改变当前固件、模型、数据库、协议或权限；不把已有 HC 的“源码交付”或旧实板记录当作本方案通过。文档 PR 关联本 issue，但不自动关闭后续实施与验收。

## 1. 用户希望得到什么

小呆在接到一个长任务后仍能与人互动；Luna 自身变慢时，停止、合法输入接纳和本地反馈仍有效。它能够记住相关的人、经历与任务，并准确说明自己现在能做什么、做到哪里、还缺什么。

三条贯穿实现的用户旅程：

1. **做事时继续聊。** “帮我查一份资料”得到真实任务接纳回执后，用户接着提问、查进度、补要求或取消。完成结果可跨断线、跨天留存，合适时再交付。
2. **很晚才告诉名字。** 多次匿名对话后说“记住，我是 Jason”。系统分别保存称呼、确认声音、关联有证据的历史；无声学证据的历史保持未知或由用户明确指认。名字须是真实人名并经 bot-server 统一校验（称谓不能作名字、冒犯性名字被拒绝）；无可靠声音匹配的自报不解锁私密内容。
3. **通过对话使用现有功能。** “记住这个偏好”“明早提醒我”“太吵了”“刚才那件事做到哪了”能到达真正的服务，用户可确认、修改和撤销结果。

## 2. 六项设计原则

| 原则 | 必须成立的行为 |
| --- | --- |
| 即时交互由确定性控制逻辑负责 | 主循环不等待 Luna、OpenCode、ASR、声纹、TTS、数据库、网络发送或取消清理；可以等待下一条事件 |
| 生命周期独立 | 激活、轮次、模型请求、能力操作、任务、呈现、输出许可分别管理；任务未完不占住普通下一轮 |
| 能力按端到端结果定义 | 有自然入口、可信输入、就绪状态、执行、补参、恢复、取消和回执；存在函数或关键词路由不代表能力完成 |
| 名字可以晚到，关系可以纠正 | 话语、声音观察/匿名组、人物 ID、名字分开；历史关联保留来源和修订 |
| 承诺对应真实证据 | received、durably accepted、persisted/applied、displayed/played 分开；数据库未提交不能说“记住了” |
| 连续性来自可追溯状态 | 人物、相关任务、待用户补充和未交付结果进入有版本的上下文；不编造认识、进度或效果 |

“不等待”约束针对交互控制路径，不禁止工作进程等待模型或存储；也不意味着所有后端失效时仍能理解任意新问题。该设计提供隔离与过载机制，真实尾延迟仍需测量。

## 3. 复用决定与项目边界

- **主循环参考 AIRI 的 Policy / Runtime 分层**：状态与事件产生决策，慢结果以带版本的事件返回。线程/进程隔离参考 TEN，控制帧优先级参考 Pipecat；LiveKit 用作语音会话与中断对照。选择依据和上游已实现边界见[研究与源码证据](research.md)，不声称任何项目在本板实测最快。
- 保留 Mac Rust/Tauri、Tokio、现有 Luna 快核心和 OpenCode harness；不引入第二套 agent runtime、通用 broker 或替代记忆框架。
- S31 `bot_runtime` 继续决定本地激活、收音、播放、停止和资源仲裁。Bot 不独立联网运行 Agent；Mac 不逐帧控制眼睛或未安装的电机。
- SQLite 继续为权威记录，Graphiti 继续为可重建派生索引；家庭日记录、原始来源和旧私域/留存选择保留。
- 完整“小呆小呆”或有意触摸启动互动。被动人脸、时间或房间聊天不启动对话；明确配置的提醒按既有规则播报且不打开录音。
- “随时回应”在设备上的现状：`bot_runtime.c:755-767` 在 THINKING/SUBMITTED 状态已接受完整唤醒词（只有 SPEAKING 需 `echo_safe`），说话中轻触即停止（`:812`），思考中轻触即补一句（`:816-820`）。缺口主要在 Mac 侧。Luna 卡住时用户在 THINKING 中再次说“小呆小呆”：设备先发 `assistant.cancel`（或补话时发 `assistant.supplement`）撤销卡住的轮次，Mac 先 fence 再接纳新采集，不再因 `active` 返回 `session_busy`（当前 `gateway.rs:1331`）。
- 默认半双工，先验证触摸停止及长任务期间的普通新对话。AEC、可靠声学打断和高级媒体能力继续独立验收。

## 4. 当前基础与本次缺口

最新源码中，Luna 已在后台 `tokio::spawn` 路径执行；ASR/身份有独立通道；TaskService 已持久化任务；声音相册已有匿名组与后命名。此次重构不重复实现这些模块。

剩余问题是网关分支内的准备/传输/存储等待、`Delivery.tasks` 与普通输入接纳耦合，以及管理功能到自然对话之间缺少能力契约。具体代码依据见[研究](research.md)，能力成熟度见[能力规范](capabilities.md)。

这组规格补充既有[家庭陪伴设计](../household-companion/README.md)。对于“所有后端均不占控制路径”、当前输出与任务交付分离、语义能力和迟到身份工作流，以本组规格作为后续实现目标；既有来源、隐私、硬件所有权、真实回执和权限约束继续有效。旧验收记录保持原有日期和范围。

## 5. 文档入口

| 文档 | 回答的问题 |
| --- | --- |
| [运行架构](architecture.md) | 谁做决策、谁等待、如何停止、队列满怎么办、旧结果如何失效 |
| [能力体系与成熟度](capabilities.md) | 功能怎样成为能力、模型能提交什么、宿主如何验证、当前哪些入口缺失 |
| [身份连续性](identity-continuity.md) | 晚自我介绍、证据条件、补样、历史关联、重名、纠错和删除 |
| [实施计划与依赖](implementation-plan.md) | RA-00 与十步如何拆 PR、哪些可以并行、每步何时完成、怎样迁移回滚 |
| [验收与形式化映射](acceptance.md) | 真实场景、故障注入、模型边界、测量定义和未运行记录 |
| [研究与源码证据](research.md) | 上游选择依据、具体版本、当前代码观察与推论的区别 |

## 6. 分步规格

| 步骤 | 规格 | 实施状态 |
| --- | --- | --- |
| RA-00 | [Mac 侧快速解耦（无协议变更）](steps/ra-00-quick-decoupling.md) | 待实现；建议最先进行 |
| RA-01 | [控制契约与状态所有权](steps/ra-01-control-contracts.md) | 待实现 |
| RA-02 | [网关 I/O 解耦](steps/ra-02-gateway-io.md) | 待实现 |
| RA-03 | [Luna 服务隔离](steps/ra-03-luna-worker.md) | 待实现 |
| RA-04 | [独立任务交付](steps/ra-04-task-delivery.md) | 待实现 |
| RA-05 | [能力目录与受控调用](steps/ra-05-capability-registry.md) | 待实现 |
| RA-06 | [连续上下文与任务控制](steps/ra-06-context-and-task-control.md) | 待实现 |
| RA-07 | [迟到的身份介绍](steps/ra-07-identity-continuity.md)：RA-07a 称呼与当前声音、RA-07b 历史关联 | 待实现 |
| RA-08 | [首批助理适配器](steps/ra-08-assistant-adapters.md) | 待实现 |
| RA-09 | [恢复与迁移](steps/ra-09-recovery-and-migration.md) | 待实现 |
| RA-10 | [联合验收与分阶段启用](steps/ra-10-validation-and-rollout.md) | 待实现 |

## 7. 关联工作与完成标准

总 issue [#36](https://github.com/JasonGuoo/esp-bot/issues/36) 维护本组进度。已有家庭主线 [#13](https://github.com/JasonGuoo/esp-bot/issues/13)、任务 [#18](https://github.com/JasonGuoo/esp-bot/issues/18)、声音标注 [#24](https://github.com/JasonGuoo/esp-bot/issues/24) 和多媒体扩展 [#31](https://github.com/JasonGuoo/esp-bot/issues/31) 各保留原范围。现有语音修复验收 [#33](https://github.com/JasonGuoo/esp-bot/issues/33)、唤醒声学 [#34](https://github.com/JasonGuoo/esp-bot/issues/34)、steer 顺序 [#35](https://github.com/JasonGuoo/esp-bot/issues/35) 不因文档或本方案部分完成而关闭。

每个行为实现 PR 同步评估并更新受影响的 TLA+ 模型、TLC 配置及对应实现回归；没有模型的核心路径在首次改动时补齐。见[项目形式化要求](../../../formal/tla/README.md)。不新增 CI，不为本次文档改动运行模型、构建固件或刷机。

最终完成以用户场景、实际代码与环境证据判断：Luna 卡住时本地控制仍有效；长任务不占住普通聊天；迟到自我介绍可正确延续、解释和纠正；每项已开放能力都有真实状态与结果。
