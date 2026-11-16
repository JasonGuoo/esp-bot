# 家庭陪伴架构：实施计划与逐步开发清单

**日期：2026-11-16；开发基线：`5ae0c780e146a72ebf10f71f4d6976a0ed47e2fe`。状态：源码开发完成，待测试。** 本计划承接[已确认设计](README.md)，保留开发拆分、依赖和验收条件；本轮实际接线、配置、依赖及剩余限制统一见 [实施记录](implementation-2026-11-16.md)。

## 交付方法与状态

- 设计 PR #12 已合并，已建立 [HC-00 / #13](https://github.com/JasonGuoo/esp-bot/issues/13) 和 HC-01～HC-15 / #14～#28。本轮源码开发完成，子项保持**开发完成，待测试**，不据此关闭验收。
- 下列“开发步骤”保留实施顺序和完整目标，其中模型评估、真实打印、声学测量和场景验收尚未执行。HC-15 只交付 18 个场景与手工记录入口，初始状态均为 `not_run`；没有勾选验收通过。本轮未编译 Rust/TypeScript/ESP-IDF、未执行模型、Mac GUI 或实板。
- 保留现有模型路线、设备配对与局域网音频选择；当前不加入设备独立联网 Agent，不把通用家庭陪伴限定成教育产品。
- 没有现成硬件/模型环境时继续交付可审阅源码，并明确未运行部分。**不创建 CI、不安装模型或增加测试基础设施来验证本次文档。** 后续验证只针对真实状态/数据/外部效果风险使用现有本地流程。
- 依赖表示功能集成的前置条件；接口确定后可并行写适配器与 UI。接口尚未实现时必须标明使用的样例或占位，不伪装完整联调。

## 阶段与最先交付的主线

| 阶段 | 内容 | 可观察的交付点 |
| --- | --- | --- |
| A：共同契约与原始记录 | HC-01、HC-02；已有缺陷独立修复 | 一天多个成员、多次唤醒归同一家庭记录，原始输入和状态不丢 |
| B：连续、及时的对话 | HC-03、HC-04、HC-05、HC-06 | 声纹不挡普通应答；长任务不挡下一轮；ACK 与真实进度不结束任务 |
| C：呈现主线 | HC-07、HC-08 的图文语音首版 | 可同步展示并回看真实素材；动画/视频按能力逐步增加 |
| D：记忆与家庭控制台 | HC-09、HC-10、HC-11 | 可查来源、改声纹标注、整理事件/学习证据，次日接着聊 |
| E：做事情 | HC-12，可在 C/D 并行 | 生成指定素材、提交真实打印任务、正确报告结果 |
| F：进一步自然交互 | HC-13、HC-14 | 多人重叠识别与全双工语音打断分别经过实际条件验收 |
| G：贯穿迭代的场景验收 | HC-15 | 每个阶段留真实记录，基础家庭主线不等待 F 全部完成 |

**设计时的开发顺序（保留拆分依据）：** HC-01 与已有 #7～#11 分别开代码 PR；契约固定后 HC-02/HC-03/HC-05 可并行推进，优先接出“唤醒→短应答→ASR→普通回答→日记录”的主线。接着落实 HC-04、HC-06 和 HC-07/08 的单图文语音路径。图索引评估 HC-09 可与呈现工作并行，打印 HC-12 可在任务/素材接口稳定后接入。

不要等完整 Graphiti、全视频或 AEC 才开始验证互动；也不能用一组后台任务取代前台响应。每个功能 issue 的验收仍是运行效果完成条件，不由本次源码交付代替。

## 已有缺陷：复用原 issue，不重复建单

| 已有 issue | 与本设计的关系 | 处理位置 |
| --- | --- | --- |
| [#7 设备单独工作时 Mac 停止按钮不可用](https://github.com/JasonGuoo/esp-bot/issues/7) | 所有新任务/播放必须始终可停止 | HC-05/06/11 的前置修复；保留独立缺陷范围 |
| [#8 连接/5xx 冷却未参与实际选路](https://github.com/JasonGuoo/esp-bot/issues/8) | 故障 provider 反复增加互动等待 | 先独立修复，HC-15 验证路由故障体验 |
| [#9 召回变化删除 OpenCode 会话](https://github.com/JasonGuoo/esp-bot/issues/9) | 日常追问连续性直接受损 | HC-04 明确依赖；修复仍须保留真正撤销/删除语义 |
| [#10 关闭声音设置丢最后编辑](https://github.com/JasonGuoo/esp-bot/issues/10) | 音量/静音等偏好必须可靠 | 独立修复；HC-11/15 复用权威状态回执 |
| [#11 准备 TTS 进度未转发](https://github.com/JasonGuoo/esp-bot/issues/11) | “小呆在做什么”缺少真实阶段 | HC-06 明确依赖，不另建相同 bug |

**#7～#11 已有源码修正，状态为开发完成、待测试。** 对应变更见 [实施记录](implementation-2026-11-16.md)，仍保留原 issue 和待执行验证，未以文档或源码合并关闭。

## 工作分解与依赖

| 编号 / issue 入口 | 开发范围 | 前置 | 阶段 |
| --- | --- | --- | --- |
| [HC-01](https://github.com/JasonGuoo/esp-bot/issues/14) | [统一家庭、激活、任务与呈现契约](#hc-01) | 无 | A |
| [HC-02](https://github.com/JasonGuoo/esp-bot/issues/15) | [建立家庭日会话与可追溯原始记录](#hc-02) | [HC-01](#hc-01) | A |
| [HC-03](https://github.com/JasonGuoo/esp-bot/issues/16) | [Mac 并行执行 ASR 与说话人识别](#hc-03) | [HC-01](#hc-01)、[HC-02](#hc-02) | B |
| [HC-04](https://github.com/JasonGuoo/esp-bot/issues/17) | [OpenCode 家庭日连续上下文与 compact](#hc-04) | [HC-02](#hc-02)、[#9](https://github.com/JasonGuoo/esp-bot/issues/9) | B |
| [HC-05](https://github.com/JasonGuoo/esp-bot/issues/18) | [持久任务调度与取消、输出许可分离](#hc-05) | [HC-01](#hc-01)、[HC-02](#hc-02) | B |
| [HC-06](https://github.com/JasonGuoo/esp-bot/issues/19) | [本地应答、真实阶段播报与表情联动](#hc-06) | [HC-03](#hc-03)、[HC-05](#hc-05)、[#7](https://github.com/JasonGuoo/esp-bot/issues/7)、[#11](https://github.com/JasonGuoo/esp-bot/issues/11) | B |
| [HC-07](https://github.com/JasonGuoo/esp-bot/issues/20) | [语义呈现规划、不可变素材与交付回执](#hc-07) | [HC-01](#hc-01)、[HC-05](#hc-05) | C |
| [HC-08](https://github.com/JasonGuoo/esp-bot/issues/21) | [分段图文、音视频与设备有界同步播放](#hc-08) | [HC-06](#hc-06)、[HC-07](#hc-07) | C |
| [HC-09](https://github.com/JasonGuoo/esp-bot/issues/22) | [接入 Graphiti 可重建家庭知识索引](#hc-09) | [HC-02](#hc-02) | D |
| [HC-10](https://github.com/JasonGuoo/esp-bot/issues/23) | [后台整理家庭事件、话题、学习证据与长期记忆](#hc-10) | [HC-04](#hc-04)、[HC-09](#hc-09) | D |
| [HC-11](https://github.com/JasonGuoo/esp-bot/issues/24) | [Mac 家庭时间轴、声纹标注与来源检查](#hc-11) | [HC-02](#hc-02)、[HC-03](#hc-03)、[HC-05](#hc-05)、[HC-10](#hc-10) | D |
| [HC-12](https://github.com/JasonGuoo/esp-bot/issues/25) | [Mac 打印能力与幂等任务回执](#hc-12) | [HC-05](#hc-05)、[HC-07](#hc-07) | E |
| [HC-13](https://github.com/JasonGuoo/esp-bot/issues/26) | [多人说话分段、重叠与家庭身份归属](#hc-13) | [HC-02](#hc-02)、[HC-03](#hc-03) | F |
| [HC-14](https://github.com/JasonGuoo/esp-bot/issues/27) | [播放参考、AEC 与真正的语音打断](#hc-14) | [HC-03](#hc-03)、[HC-05](#hc-05)、[HC-06](#hc-06)、[HC-08](#hc-08) | F |
| [HC-15](https://github.com/JasonGuoo/esp-bot/issues/28) | [家庭陪伴完整场景与资源验收记录](#hc-15) | [HC-04](#hc-04)、[HC-06](#hc-06)、[HC-08](#hc-08)、[HC-10](#hc-10)、[HC-11](#hc-11)、[HC-12](#hc-12)、[#7](https://github.com/JasonGuoo/esp-bot/issues/7)、[#8](https://github.com/JasonGuoo/esp-bot/issues/8)、[#9](https://github.com/JasonGuoo/esp-bot/issues/9)、[#10](https://github.com/JasonGuoo/esp-bot/issues/10)、[#11](https://github.com/JasonGuoo/esp-bot/issues/11) | G |

依赖没有把所有工作排成单一长队。HC-11 的时间轴/标注可先随 HC-02/03 交付，记忆审阅等最后接齐；HC-08 静态图文首版完成后即可参与基础验收，视频作为扩展能力单独记录；HC-13 的 UI 接入与 HC-11 协作，不要求等完整记忆 UI 才开始后端开发。

## HC-01

**当前交付：开发完成，待测试。** 共享契约和协商接线完成；设备精确 wire 见固件接线记录。

**统一家庭、激活、任务与呈现契约**

**目标：** 先把家庭会话、输入授权、Mac 工作与设备输出分开，避免后续模块继续复用同一个 session/turn ID。

**前置：** 无。**规格：** [设计说明](README.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/14)。

**代码/文档入口：** [docs/module-contracts.md](../../../docs/module-contracts.md) · [shared/mac-brain-v3.md](../../../shared/mac-brain-v3.md) · [server/bot-server/src-tauri/src/brain/gateway.rs](../../../server/bot-server/src-tauri/src/brain/gateway.rs) · [firmware/components/bot_runtime/include/bot_runtime.h](../../../firmware/components/bot_runtime/include/bot_runtime.h)。

### 开发步骤

1. 定义 household_id、daily_conversation_id、episode_id、conversation_turn_id、utterance_id、speaker_segment_id 与来源 revision；保留 RuntimeToken 字段原义。
2. 定义 activation_id、task_id、presentation_id、playback_id、output_lease_id 的映射、失效规则与允许的状态迁移；将 task 状态、播放状态和外部动作状态分开。
3. 编写版本化的宿主 DTO、持久事件 envelope 和错误类型，包含 event_id、sequence、occurred_at、recorded_at、schema_version、来源版本及 causation/correlation ID。
4. 约定音频 sample index、设备单调时钟、家庭时区和墙上时钟用途；不可跨时钟域直接比较。
5. 扩展能力协商草案与兼容适配：媒体格式/分辨率/预算/同步/取消能力，未知字段及旧客户端明确降级；不要直接替换当前线协议。
6. 列出 SQLite schema 迁移与回滚前置条件、旧会话映射、feature flag 和每条新事件的唯一产生方。

### 验收条件

- 同一天两次唤醒映射到同一家庭日记录，但 activation/RuntimeToken 不复用。
- 旧固件继续工作；新能力不支持时明确降级或报错；旧 lease 的迟到结果不能占用新会话。
- 契约示例覆盖跨日任务、身份迟到、断线重连、取消和重复事件；阶段评审后才能各模块独立实现。

**交付边界：** 提交共享类型与兼容层，附现有路径和新路径的状态对照；不宣称协议草案已在设备运行。

## HC-02

**当前交付：开发完成，待测试。** 家庭 SQLite 迁移、日记录、原文修订与来源 RPC 已接线；迁移/跨日实际执行待验收。

**建立家庭日会话与可追溯原始记录**

**目标：** 先可靠保存发生过的对话，再从这些记录派生摘要和记忆；参与者变化不再切断家庭上下文。

**前置：** [HC-01](#hc-01)。**规格：** [设计说明](household-memory.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/15)。

**代码/文档入口：** [server/bot-server/src-tauri/src/memory/schema.sql](../../../server/bot-server/src-tauri/src/memory/schema.sql) · [server/bot-server/src-tauri/src/memory/store.rs](../../../server/bot-server/src-tauri/src/memory/store.rs) · [server/bot-server/src-tauri/src/memory/types.rs](../../../server/bot-server/src-tauri/src/memory/types.rs) · [server/bot-server/src-tauri/src/brain/mod.rs](../../../server/bot-server/src-tauri/src/brain/mod.rs)。

### 开发步骤

1. 新增家庭、时区、日记录、话题/episode、轮次、话语/说话段、参与者、引用与修订模型；唯一约束防止并发创建重复日记录。
2. 将用户输入的接收、ASR 结果/修订、取消和失败状态及时落盘；不再等图片/TTS 完成才补写用户历史。
3. 分别保存模型生成、设备已显示/已播放、工具执行与外部动作回执；助手未成功播放的内容不能标为用户已听到。
4. 按家庭时区确定归档日；保存发生与记录时间，跨午夜 utterance 保持完整，跨日任务通过关联记录返回，不强行取消。
5. 迁移旧 profile/conversation/memory：保留稳定 person ID、旧引用、已有限制和用户留存选择；不得把个人私有记录默认转家庭共享。
6. 提供可配置的文字、原始音频、图像/视频、生成素材与长期记忆独立留存策略，支持长周期文字档案并保留旧安装的选择。区分原文到期清理与显式遗忘；音频不因日会话默认长期保存。

### 验收条件

- 同日多人、多次唤醒只对应一个家庭日记录；不同话题仍可独立检索。
- ASR 后模型/TTS 失败或用户取消，原始输入和状态仍可查，重试不会重复写轮次。
- 跨日、时区变更、迁移中断恢复和删除后无旧数据重新出现都有明确行为；迁移保留旧权限。

**交付边界：** 数据库迁移、数据访问接口和最小家庭会话映射；附可回滚方案与样本迁移说明。

## HC-03

**当前交付：开发完成，待测试。** ASR/身份独立受管通道已接线；模型并发延迟和识别效果待实测。

**Mac 并行执行 ASR 与说话人识别**

**目标：** 普通问答的响应不再等待声纹；身份结论可稍后补充，并保持同一份音频的准确来源。

**前置：** [HC-01](#hc-01)、[HC-02](#hc-02)。**规格：** [设计说明](device-and-audio.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/16)。HC-02/03 可并行开发，HC-03 完成集成前必须接入家庭来源与修订接口。

**代码/文档入口：** [server/bot-server/src-tauri/src/brain/mod.rs](../../../server/bot-server/src-tauri/src/brain/mod.rs) · [server/bot-server/src-tauri/src/brain/worker.rs](../../../server/bot-server/src-tauri/src/brain/worker.rs) · [server/bot-server/local-models/worker.py](../../../server/bot-server/local-models/worker.py) · [server/bot-server/src-tauri/src/brain/media.rs](../../../server/bot-server/src-tauri/src/brain/media.rs)。

### 开发步骤

1. 复用现有监督器拆出可独立调度的 ASR 与 identity 通道/进程；保留各自健康、取消、超时、模型热加载和内存预算，不对单互斥 worker 直接 join。
2. 授权音频使用一份有界源与一致的 sample range/utterance_id，启动 ASR 和声纹分支；声纹路径不得修改原始音频时间。
3. ASR 可用后先持久化并继续普通推理；无声纹模型、短语音、低质量或身份超时返回 unknown/insufficient，不使问答失败。
4. 身份结果携带候选、相似度定义、模型/模板版本、质量和来源修订；迟到仅修订标注，不重放模型、不更换日会话或提升权限。
5. 将前台语音优先级、队列上限和取消传播落到进程监督器；后台记忆任务应可让出资源。
6. 保留现有 ECAPA 模板格式；独立记录 ASR 可用时间、身份完成时间与资源占用，为后续分段识别提供基线。

### 验收条件

- 给身份分支注入延迟/不可用时，普通问题在 ASR 完成后继续且无 local_worker_busy。
- “好”“停”等短语音仍能表达意图；未知身份不被强制分配给当前选中档案。
- 取消后的结果不触发输出；有效迟到身份能修订同一条来源并留下版本记录。

**交付边界：** 两个可管理执行通道及事件接线，附未安装模型时的降级说明；真实阈值等待 Mac/家庭环境验证。

## HC-04

**当前交付：开发完成，待测试。** 家庭日会话、稳定指纹、原生 compact 持久核对和 inbox 归属已接线；OpenCode 实际运行待验收。

**OpenCode 家庭日连续上下文与 compact**

**目标：** 同一家庭当天能持续追问，同时允许上下文压缩、记忆更新与跨天主题延续。

**前置：** [HC-02](#hc-02)、[#9](https://github.com/JasonGuoo/esp-bot/issues/9)。**规格：** [设计说明](household-memory.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/17)。

**代码/文档入口：** [server/bot-server/src-tauri/src/harness/turn.rs](../../../server/bot-server/src-tauri/src/harness/turn.rs) · [server/bot-server/src-tauri/src/harness/mod.rs](../../../server/bot-server/src-tauri/src/harness/mod.rs) · [server/bot-server/src-tauri/src/brain/mod.rs](../../../server/bot-server/src-tauri/src/brain/mod.rs) · [docs/mac-opencode.md](../../../docs/mac-opencode.md)。

### 开发步骤

1. 先在原 issue #9 修复动态召回/排序导致 session DELETE 的问题；不要以移除撤销检查换取连续性。
2. 将稳定人格/权限/版本与每轮检索材料分离；把 daily_conversation_id 映射到前台 OpenCode 会话世代，按家庭串行投递有顺序的输入。
3. 对项目实际 OpenCode V2 版本核对 compact、指令、事件、历史读取和取消 API；记录兼容范围，禁止直接套用旧 V1 或仅凭当前网站文档假定 2.2.5 可用。
4. 达到上下文预算时触发原生 compact，并保存摘要覆盖水位、最近原文、待办与关联来源；不删除权威原始记录。
5. 重连/重启从会话历史和持久 ledger 恢复，不能指望 live SSE 补发漏失事件；未完成外部动作不自动重放。
6. 记忆纠正/撤销时识别受影响的注入版本，清理污染上下文并从仍合法的日记录重建；跨天新会话只加载必要摘要、开放话题和检索来源。

### 验收条件

- 同一天换说话人、再次唤醒、检索次序变化和一次 compact 后，“刚才那个/继续”仍对应同一主题。
- 来源被删除或权限收紧后不再从旧 prompt/摘要带回；compact 失败可恢复且不伪称记忆已整理。
- 长任务不会让同一 OpenCode 会话同时接受无序 prompt；模型/路由错误保留可解释状态。

**交付边界：** 日会话协调器与 compact 接线，附真实版本适配证据；#9 由其代码修复关闭，本设计 PR 不关闭。

## HC-05

**当前交付：开发完成，待测试。** 持久任务、事件、取消、补参恢复和输出许可分离已接线；重启恢复与外部效果待验收。

**持久任务调度与取消、输出许可分离**

**目标：** 搜索、画图、视频准备和打印等长工作能够持续，而机器人仍能听下一次请求和响应停止。

**前置：** [HC-01](#hc-01)、[HC-02](#hc-02)。**规格：** [设计说明](presentation-and-tasks.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/18)。

**代码/文档入口：** [server/bot-server/src-tauri/src/brain/mod.rs](../../../server/bot-server/src-tauri/src/brain/mod.rs) · [server/bot-server/src-tauri/src/brain/gateway.rs](../../../server/bot-server/src-tauri/src/brain/gateway.rs) · [server/bot-server/src-tauri/src/brain/rpc.rs](../../../server/bot-server/src-tauri/src/brain/rpc.rs) · [server/bot-server/src-tauri/src/harness/turn.rs](../../../server/bot-server/src-tauri/src/harness/turn.rs)。

### 开发步骤

1. 建立 SQLite task/job/event/attempt 记录及幂等提交键，管理 queued/running/awaiting_input/succeeded/partial/failed/cancel_requested/cancelled/unknown 状态，并与播放和外部适配器的细分状态分开。
2. 拆开当前全局 turn_gate/ActiveTurn 对采集、推理、素材制作和播放的总锁；按前台会话、能力并发和资源预算调度，不能只用后台 spawn 包住原阻塞流程。
3. 长工作使用独立工作上下文或能力作业；前台 OpenCode 仍按序处理输入，任务结果通过有来源的事件回传日记录。
4. 定义停止当前播放、取消当前生成、结束采集、取消具体任务、全局停止和已发生副作用的处理范围；保留取消墓碑/代次，拒绝失效输出许可的新播放。迟到的真实外部效果回执仍按原 task_id/attempt 归档核对，不冒充当前任务结果。
5. 断线/休眠/重启按任务类别恢复或标 unknown；可继续的任务不依赖旧 activation，但没有新输出许可不能自动播报。
6. 增加任务状态快照、真实阶段事件、心跳与进度区别、每能力时限、重试上限和错误归因；物理动作不自动重放。

### 验收条件

- 一个慢媒体任务运行时，另一轮普通问题可以进入前台；两次输入不会同时污染同一 OpenCode 会话。
- 断线后任务可在允许的边界完成并归档，重连不会突然播放旧内容。
- 按 task_id 取消只影响指定工作；全局停止及时清除设备输出并显示未能撤销的外部效果。

**交付边界：** 持久调度器、能力适配接口与可恢复状态 API；保留现有权限入口。

## HC-06

**当前交付：开发完成，待测试。** 播放 purpose、ACK/进度和实际阶段反馈已接线；播放节奏和停止待实板验收。

**本地应答、真实阶段播报与表情联动**

**目标：** 明确听到了，也让用户知道小呆正在做什么；中间短语音不会破坏正式回答的 token 或播放时序。

**前置：** [HC-03](#hc-03)、[HC-05](#hc-05)、[#7](https://github.com/JasonGuoo/esp-bot/issues/7)、[#11](https://github.com/JasonGuoo/esp-bot/issues/11)。**规格：** [设计说明](presentation-and-tasks.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/19)。

**代码/文档入口：** [firmware/components/bot_runtime/bot_runtime.c](../../../firmware/components/bot_runtime/bot_runtime.c) · [firmware/components/bot_responses](../../../firmware/components/bot_responses) · [firmware/main/wake_response_player.c](../../../firmware/main/wake_response_player.c) · [server/bot-server/src-tauri/src/brain/gateway.rs](../../../server/bot-server/src-tauri/src/brain/gateway.rs) · [server/bot-server/src/companion.ts](../../../server/bot-server/src/companion.ts)。

### 开发步骤

1. 区分 wake_ack、task_ack、progress、content、final 等播放用途及完成迁移；wake_ack 回到同一 activation 的采集就绪流程，task_ack/progress 回同一任务等待，不 arm_followup 或误生成新 token。
2. 复用 104 条本地语音的时段/场景/表达元数据，增加去重、冷却与静音/夜间偏好；快答案到达时丢弃未播 ACK。
3. 将 OpenCode/能力适配器真实事件映射为查找、准备图、合成语音、等待打印机等用户可懂阶段，带 task/source/sequence。
4. 实现状态驱动的播报节流：首个有意义更新以 5–8 秒作为初始可配置目标，后续语音间隔至少约 15–20 秒或关键变化；这些数值不是已测性能。
5. 复用当前 runtime 表情/字幕权限；进度语音不得与用户说话、最终答案或停止操作争抢；心跳不伪装成工作进展。
6. 关联并完成 #11 的 synthesizing 转发与 #7 的设备活动停止入口；修正取消/断线后排队 ACK 或进度迟到的路径。

### 验收条件

- 慢任务会短暂应答并报告真实阶段；快速回复没有重复“我在/好的”。
- ACK 播完仍能接受同一任务的内容；取消后旧 ACK/语音/表情均不能影响下一次激活。
- 等待工具权限、服务失败和超时不会谎称正在画图；Mac 在设备单独工作时也能停止。

**交付边界：** 状态机与可配置应答策略；提供慢任务、快任务、取消和静音场景的验收说明。

## HC-07

**当前交付：开发完成，待测试。** 冻结计划、不可变资产、来源及实际回执已接线；额外创意生成适配仍按下述能力边界处理。

**语义呈现规划、不可变素材与交付回执**

**目标：** 让每次结果选择合适的语音、图文和表情组合，并能证明用户实际收到了哪些内容。

**前置：** [HC-01](#hc-01)、[HC-05](#hc-05)。**规格：** [设计说明](presentation-and-tasks.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/20)。

**代码/文档入口：** [server/bot-server/src-tauri/src/brain/scene.rs](../../../server/bot-server/src-tauri/src/brain/scene.rs) · [server/bot-server/src-tauri/src/brain/mod.rs](../../../server/bot-server/src-tauri/src/brain/mod.rs) · [server/bot-server/src-tauri/src/brain/display.rs](../../../server/bot-server/src-tauri/src/brain/display.rs) · [server/bot-server/src-tauri/src/brain/gateway.rs](../../../server/bot-server/src-tauri/src/brain/gateway.rs)。

### 开发步骤

1. 引入独立 PresentationPlanner 接口：简单场景走模板，主模型可同轮生成计划，仅复杂情况额外规划；将教育限定提示改为通用家庭陪伴能力。
2. 定义有界 segment 的 speech/caption/visual/expression、依赖、阅读停留、fallback 与完成标准，宿主做 schema/资源/来源验证。
3. 解决 image_prepare 改写 speech 的时序：先冻结语义段与 revision，再并行 TTS/图片制作；修订必须产生新版本并取消旧派生物。
4. 建立 immutable asset manifest，保存 digest、来源/生成方式、版本、可用终端、格式、字幕/时间轴和引用；同一素材可供设备、Mac、回忆和打印。
5. 分开 planned/prepared/dispatched/displayed/audio-started/audio-completed/failed/cancelled 回执，均绑定计划版本和播放尝试；不把网络发送完成当作已看/已听。
6. 必要视觉缺失时标 partial，并选择明确的文字/语音替代；精确笔顺、公式、图表采用可靠数据与确定性渲染。创意图生成仍是后续扩展：本轮提供确定性图解、受控素材和现有工具结果接入，不内置新的生成模型；只有已配置且通过能力检查的服务可用，未配置时采用检索、图解或明确降级。

### 验收条件

- 同一段 speech、字幕和图片引用同一冻结版本；迟到素材不替换新问题。
- 用户问“刚才那张图”能定位实际展示的素材；未播放内容不会进入已听见事实。
- 普通问答无需额外串行 LLM；必需图失败有可解释的 partial 状态，不伪称已显示。

**交付边界：** 呈现 schema、planner、素材仓库/回执模型及最小模板；保持旧场景适配可用。

## HC-08

**当前交付：开发完成，待测试。** 分段图文语音、媒体准备、JPEG 帧播放及 Mac 预览已接线；帧率、同步和资源预算均未实测。

**分段图文、音视频与设备有界同步播放**

**目标：** 优先把声音、清晰图文和表情一起送到用户面前，再按实测能力扩展动画和视频。

**前置：** [HC-06](#hc-06)、[HC-07](#hc-07)。**规格：** [设计说明](presentation-and-tasks.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/21)。

**代码/文档入口：** [server/bot-server/src-tauri/src/brain/display.rs](../../../server/bot-server/src-tauri/src/brain/display.rs) · [server/bot-server/src-tauri/src/brain/gateway.rs](../../../server/bot-server/src-tauri/src/brain/gateway.rs) · [firmware/components/brain_link](../../../firmware/components/brain_link) · [firmware/components/bot_audio](../../../firmware/components/bot_audio) · [firmware/components/face](../../../firmware/components/face)。

### 开发步骤

1. 先完成单段语音+图片/文字+表情的完整路径，再支持多段预取、阅读停留和回看；先答已知内容，后续素材可渐进加入。
2. Mac 复用 FFmpeg 与现有渲染器做字体/布局/转码/字幕和缓存，音视频搜索保留来源、标题、片段时间、访问限制；不将任意网页当成可直接播放媒体。
3. 在设备复用 ESP-GMF esp_player/esp_video_render 与当前 codec/LVGL owner；协商可用格式、大小、分辨率、队列上限和取消能力。
4. 以设备相对播放时钟协调音画，视觉相关台词等实际 first-frame；文字/图解占有足够屏幕面积，眼睛可缩小或切换。
5. 对 S31 JPEG 编解码共享资源安排 camera/播放互斥或降载，测量内存、唤醒延迟和稳定性；不预设 H.264 硬解或未测 FPS。
6. 高级视频路径按独立能力开关逐步开放；格式/网络不支持时提供静态帧、字幕/音频或 Mac 查看入口，并记录实际降级。

### 验收条件

- 图文与对应语音版本一致，“看这里”不早于画面出现；停止能丢弃未播段和解码队列。
- 分段展示可回看，Mac 同样能打开素材；资源不足的结果明确降级，不影响本地完整词唤醒。
- 至少一组真实音视频来源完成 Mac 预处理→能力协商→播放/降级验证；只有实测组合才能宣称支持。

**交付边界：** 先图文语音 PR，再动画/音视频 PR；分别记录 Mac 与 S31 实测能力，复杂视频不阻塞静态图文交付。

## HC-09

**当前交付：开发完成，待测试。** Graphiti v0.31.2 sidecar、检索投影与重建已接线；尚未安装依赖或运行模型/图数据库评估。

**接入 Graphiti 可重建家庭知识索引**

**目标：** 复用成熟的时序实体/关系和检索能力，把权威来源留在现有宿主数据库。

**前置：** [HC-02](#hc-02)。**规格：** [设计说明](household-memory.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/22)。

**代码/文档入口：** [server/bot-server/src-tauri/src/memory](../../../server/bot-server/src-tauri/src/memory) · [server/bot-server/src-tauri/src/brain/memory_worker.rs](../../../server/bot-server/src-tauri/src/brain/memory_worker.rs) · [server/bot-server/local-models](../../../server/bot-server/local-models) · [docs/research/household-memory-open-source-2026-11-16.md](../../../docs/research/household-memory-open-source-2026-11-16.md)。

### 开发步骤

1. 完成固定版本 Graphiti v0.31.2 的 Mac/中文/抽取模型/embedding/图后端小规模适配评估，记录许可证、模型资源与失败边界；未通过则保持 SQLite 基线并记录替代决策。
2. 通过受管理本地 sidecar 暴露有限 ingest/search/delete/rebuild 操作，宿主决定 household/group_id；不把实验 MCP 的内存队列直接作为生产作业系统。
3. 映射 household→group_id、daily_conversation→Saga、bounded episode→EpisodicNode，保留来源ID/版本/原文跨度和实体别名；不把一天塞成一个巨大 episode。
4. 明确 Graphiti LLM/embedding 路由与外发规则；自托管图不等于推理离线，中文兼容与现有 GLM/MLX 路由必须实测，不假定可直接替换。
5. 评估 FalkorDB/FalkorDB Lite 的本地打包与资源；如需 Python >=3.12，隔离组件环境，不升级音频 worker 的既有 Python 环境。
6. 提供按家庭重建、重复摄取去重、来源删除/修订传播、时间/实体/关键词/语义混合查询；所有召回均返回可验证来源，索引掉线时前台可降级。

### 验收条件

- 图索引可从 SQLite 合法来源重建；同一来源重复提交不生成重复事实。
- 能回答“昨天谁说下周去哪里”和实体跨日别名关系，结果带来源及时间而非仅 embedding 相似度。
- 图侧故障不丢原始对话、不阻塞唤醒/ASR；旧权限和已删来源不会在重建后重新出现。

**交付边界：** 适配评估记录与最小 Graphiti adapter，明确实际通过的模型/后端组合；不引入 Letta 第二主控。

## HC-10

**当前交付：开发完成，待测试。** 持久后台整理、事件/主题/学习证据及撤销传播已接线；真实抽取质量待验收。

**后台整理家庭事件、话题、学习证据与长期记忆**

**目标：** 在不拖慢前台互动的前提下，让当天共同经历变成可追溯、可纠错、可检索的家庭记忆。

**前置：** [HC-04](#hc-04)、[HC-09](#hc-09)。**规格：** [设计说明](household-memory.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/23)。

**代码/文档入口：** [server/bot-server/src-tauri/src/brain/memory_worker.rs](../../../server/bot-server/src-tauri/src/brain/memory_worker.rs) · [server/bot-server/local-models/memory_extract.py](../../../server/bot-server/local-models/memory_extract.py) · [server/bot-server/src-tauri/src/memory/store.rs](../../../server/bot-server/src-tauri/src/memory/store.rs) · [server/bot-server/src-tauri/src/brain/mod.rs](../../../server/bot-server/src-tauri/src/brain/mod.rs)。

### 开发步骤

1. 复用并扩展 SQLite 持久队列，按 bounded episode 增量提交，记录输入水位、源版本、模型/模板/schema 版本、重试与资源优先级。
2. 区分事件摘要、话题进展、学习观察与长期事实/关系四类结构化产物；Graphiti 默认 Saga 短摘要仅用于上下文线索，不冒充家庭日报。
3. 建立 source_quote/span 与原文实际存在性的校验；区分 speaker、subject、被转述信息、不确定性和有效时间。
4. 学习观察记录 introduced、self_reported、correct_with_help、correct_independently、retained_later 等证据；不因讲过/点头自动记录 mastered，与家庭记忆规范使用同一组枚举。
5. 派生物成功提交后再推进水位；处理重复投递、迟到身份/ASR 修订、删除墓碑与重新抽取，避免被删记忆复活。
6. 实现召回装配：最近原文+开放话题+相关长期事实+实际素材来源，在预算内加入前台；冲突保留时间/证据并支持确认纠错，不把摘要循环当原始事实。

### 验收条件

- 进程在提交前/后中断均可恢复，前台新语音可让后台让出资源，水位不会跳过未持久化内容。
- 父亲说“女儿喜欢恐龙”归属说话者与subject正确；多人对话形成同一事件而不是多份孤立档案。
- 改错一个来源能更新受影响记忆/摘要/索引/上下文；第二天可检索事件、学习证据与原图。

**交付边界：** 异步整理 worker、检索装配与修订传播，附结构化输出样例及人工确认入口。

## HC-11

**当前交付：开发完成，待测试。** 家庭时间轴、来源标注/纠错、记忆核对、任务/素材回看与成员合并 UI 已接线；Mac GUI 未运行。

**Mac 家庭时间轴、声纹标注与来源检查**

**目标：** 让用户在 bot-server 看清今天发生了什么、谁说了什么、哪些被记住，以及小呆实际完成了什么。

**前置：** [HC-02](#hc-02)、[HC-03](#hc-03)、[HC-05](#hc-05)、[HC-10](#hc-10)。**规格：** [设计说明](household-memory.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/24)。

**代码/文档入口：** [server/bot-server/src/companion.ts](../../../server/bot-server/src/companion.ts) · [server/bot-server/src/companion.css](../../../server/bot-server/src/companion.css) · [server/bot-server/src-tauri/src/brain/rpc.rs](../../../server/bot-server/src-tauri/src/brain/rpc.rs) · [docs/mac-console-design.md](../../../docs/mac-console-design.md)。

### 开发步骤

1. 家庭日时间轴为主入口，展示多人共同轮次、话题、任务、素材和实际呈现状态；人物筛选是同一记录的视图，不创建按人割裂的会话。
2. 每段显示 ASR 原文/修订、speaker 候选/相似度说明/质量/模型版本，支持未知、改标、撤销、合并重复成员；自动候选不覆盖人工确认。
3. 标注与声纹模板训练分开；仅显式选择合格片段加入 enrollment，重叠/噪声/短音频不能自动污染模板。
4. 提供记忆→具体话语/图片/任务回执的来源跳转与纠错/遗忘；展示共享范围、矛盾版本、学习证据等级。
5. 呈现任务状态和作用范围清楚的停止/取消入口，复用 #7 修复；重启或打开页面先取状态快照，不仅依赖新事件。
6. 分步上线日时间轴/声纹检查，再接任务/记忆审阅，最后完善素材回看/打印；声音设置沿用 #10 修正后的权威回执。

### 验收条件

- 用户能找到并改正一次错认，后续派生记忆可追踪修订；历史对话与真实模板版本不被覆盖成伪历史。
- 无音频留存时如实显示证据不可回放，仍可检查文字和身份来源，不伪造回放入口。
- 按人筛选不拆散家庭共同事件；任务/素材失败与 partial 均清楚显示。

**交付边界：** 以当前控制台为基础渐进更新，无需重新构建整套管理后台。

## HC-12

**当前交付：开发完成，待测试。** 不可变 PDF、幂等提交、CUPS/IPP 查询和 unknown 核对已接线；没有执行实际打印。

**Mac 打印能力与幂等任务回执**

**目标：** 把对话中的“打印这张”落实到确定素材、实际打印任务和可追踪结果，避免重复出纸或虚假成功。

**前置：** [HC-05](#hc-05)、[HC-07](#hc-07)。**规格：** [设计说明](presentation-and-tasks.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/25)。

**代码/文档入口：** [server/bot-server/src-tauri/src/brain/display.rs](../../../server/bot-server/src-tauri/src/brain/display.rs) · [server/bot-server/src-tauri/src/brain/rpc.rs](../../../server/bot-server/src-tauri/src/brain/rpc.rs) · [server/bot-server/src-tauri/src/harness/turn.rs](../../../server/bot-server/src-tauri/src/harness/turn.rs) · [server/bot-server/src/companion.ts](../../../server/bot-server/src/companion.ts)。

### 开发步骤

1. 在能力接口中新增 print 适配器，复用 AppKit NSPrintOperation 或 CUPS，读取实际打印机/选项；不依赖 LLM 拼任意 shell 命令。
2. 准备不可变 PDF/图片，保存 artifact digest、版本、页数、printer/options/copies 和意图来源；沿用用户已明确的打印指令或已有设置，仅缺必要参数时询问。
3. 区分用户 print intent 与执行 attempt，持久幂等键；“再打一份”建立新意图，网络重试不能生成第二份。
4. 记录 submitting、spool_accepted(job_id)、processing、printer_reported_completed、failed/cancelled/unknown；提交到队列不等于实体纸张已输出。
5. 提交后连接中断先查询原 job/队列并对账；结果未知时展示 unknown，不盲目重试，不以通用 shell 成功证据证明完成。
6. 把进度/可取消边界/已发生效果写回日记录与控制台；设备断线不阻止任务归档，未经新输出许可不自动播报。

### 验收条件

- 重复提交同一意图最多产生一个确定任务；未知提交可对账且不会自动再打印。
- “打印刚才那张图”使用实际展示的版本；新要求修改后打印时可区分两份版本。
- 无打印机/缺纸/队列取消/无物理完成回执时都有诚实状态；不误报纸张已到手。

**交付边界：** 一个可配置的本机打印适配器、任务/素材接线和真实队列回执；实物验收单独记录。

## HC-13

**当前交付：开发完成，待测试。** 本地 pyannote Community-1 分段与 ECAPA 匹配已接线；模型未安装，小规模实录评估待执行。

**多人说话分段、重叠与家庭身份归属**

**目标：** 多人轮流或同时说话时保留可解释的参与者证据，避免把整段录音错误归给一个家庭成员。

**前置：** [HC-02](#hc-02)、[HC-03](#hc-03)。**规格：** [设计说明](device-and-audio.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/26)。

**代码/文档入口：** [server/bot-server/local-models/worker.py](../../../server/bot-server/local-models/worker.py) · [server/bot-server/src-tauri/src/brain/media.rs](../../../server/bot-server/src-tauri/src/brain/media.rs) · [server/bot-server/src-tauri/src/memory/store.rs](../../../server/bot-server/src-tauri/src/memory/store.rs)。

### 开发步骤

1. 评估 pyannote.audio/community-1 等官方已运行方案，记录模型使用条件、下载要求、Apple Silicon执行与中文家庭音频时序效果，避免自造 diarization 引擎。
2. 在独立 lane 生成 speaker_segment_id/sample range/匿名簇/重叠状态；diarization 的 Speaker A/B 不直接等于真实姓名。
3. 按合格分段复用现有 ECAPA 模板匹配并对齐 ASR；保留短段、重叠、不确定和unknown，不能默认全片 single_speaker。
4. 身份/分段结果迟到时按来源 revision 修订参与者和派生记忆，不能重新执行已经完成的打印/搜索。
5. 将分段证据接入 HC-11 的标注组件；人工改标与模型候选分开，避免整段错认污染模板。

### 验收条件

- 父母孩子轮流讲话、插话、重叠和电视背景有各自可解释结果；无可靠证据时保留未知。
- 短句不因分段门槛而消失；片段引用可准确回到原语音时间。
- 身份晚到只改变归属与有依据的记忆，不改变既有动作授权。

**交付边界：** 分段适配器及小规模实录评估；不是以模型安装成功代替家庭识别验收。

## HC-14

**当前交付：开发完成，待测试。** 单 codec 实际 DAC 参考、AFE 门控和语音打断已接线；两个开关默认关闭，声学测量尚未执行。

**播放参考、AEC 与真正的语音打断**

**目标：** 在实际喇叭播放中能可靠听到用户打断，保留触摸停止与当前半双工回退。

**前置：** [HC-03](#hc-03)、[HC-05](#hc-05)、[HC-06](#hc-06)、[HC-08](#hc-08)。**规格：** [设计说明](device-and-audio.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/27)。

**代码/文档入口：** [firmware/components/bot_audio/bot_audio_afe.c](../../../firmware/components/bot_audio/bot_audio_afe.c) · [firmware/components/bot_audio/bot_audio.c](../../../firmware/components/bot_audio/bot_audio.c) · [firmware/components/bot_runtime/bot_runtime.c](../../../firmware/components/bot_runtime/bot_runtime.c) · [server/bot-server/src-tauri/src/brain/media.rs](../../../server/bot-server/src-tauri/src/brain/media.rs)。

### 开发步骤

1. 先核查官方 ESP-SR/AFE 与 S31 BSP 支持的播放参考接法、通道/采样格式、延迟和时钟；复用单音频 owner，不另开 codec。
2. 为扬声器下发与实际写入提供可对齐参考及 sample clock，测量回声路径和尾声；不能只把 aec_init 改成 true。
3. 接入 AEC/VAD 与播放期间有效语音判断，区分本机 TTS、背景电视和现场打断；不由任意响声触发新对话。
4. 用户打断先本地停止/截断旧播放并失效输出 lease，再按明确意图和取消范围决定是否取消生成或指定任务；插话问新问题不自动取消已提交打印等持久任务。迟到音频不续播，真实外部效果回执继续归档。
5. 设置双讲/丢参考/资源超限降级，随时能回到现有半双工和触摸停止；指标分别记录误打断、漏打断、唤醒/首音延迟。

### 验收条件

- 使用实际板载麦克风和喇叭，在不同音量、距离和双讲下验证，不用无回声文件推断全双工完成。
- 机器人自己的语音不会循环唤醒；显式 Stop 始终可用，AEC故障明确降级。
- 取消的音频/图片/进度不会污染下一轮；新增处理未破坏本地完整词唤醒。

**交付边界：** 参考音频/AEC适配与真实声学验收记录；未验收前默认保持半双工，不宣称已可语音 barge-in。

## HC-15

**当前交付：开发完成，待测试。** 18 个手工场景、记录与导出入口完成；所有运行场景保持待执行，未生成通过结论。

**家庭陪伴完整场景与资源验收记录**

**目标：** 验证用户最终得到连续、有记忆、能呈现也能做事的体验，并明确基础能力与扩展能力的实际边界。

**前置：** [HC-04](#hc-04)、[HC-06](#hc-06)、[HC-08](#hc-08)、[HC-10](#hc-10)、[HC-11](#hc-11)、[HC-12](#hc-12)、[#7](https://github.com/JasonGuoo/esp-bot/issues/7)、[#8](https://github.com/JasonGuoo/esp-bot/issues/8)、[#9](https://github.com/JasonGuoo/esp-bot/issues/9)、[#10](https://github.com/JasonGuoo/esp-bot/issues/10)、[#11](https://github.com/JasonGuoo/esp-bot/issues/11)。**规格：** [设计说明](README.md)。**跟踪：** [对应 issue](https://github.com/JasonGuoo/esp-bot/issues/28)。

**代码/文档入口：** [docs/specs/household-companion](../../../docs/specs/household-companion) · [docs/review-acceptance-2026-11-15.md](../../../docs/review-acceptance-2026-11-15.md) · [docs/specs/server-voice-tasks/acceptance.md](../../../docs/specs/server-voice-tasks/acceptance.md)。

### 开发步骤

1. 按基线场景录入可重复操作步骤：孩子唤醒提问→家长补充→共同图文语音回答→打印→次日继续同话题并找回素材。
2. 覆盖英语拼写/汉字笔顺/数学物理图解/人文音视频，以及旅行讨论、创作和家务任务；呈现质量与通用陪伴均作为目标。
3. 记录 wake→ACK、语音结束→ASR、身份返回、首个有用答案、first-frame/first-audio、任务完成、资源峰值及不确定来源；区分目标和实测分布。
4. 检查取消/断线/超时/重复回执/午夜/compact/来源纠错/删除/索引重建/打印未知状态，不扩大为无目的全套测试基础设施。
5. 单列 HC-13 多人重叠与 HC-14 AEC 扩展验收；它们未通过不阻止当前半双工基础主线如实交付。
6. 在现有验收文档体系记录使用的固件、Mac App、OpenCode/model/graph版本和实际设备；未具备的资源写未运行，保留缺陷 issue。

### 验收条件

- 基础主线每一步都有可观察的UI/设备/存储或动作回执，且能从记忆回到原始对话与实际素材。
- 既有 #7～#11 通过各自代码变更解决；文档合并本身不视为修复。
- 交付明确区分源码完成、构建通过、本机执行与实板验收；不新增 CI，不用模拟输出冒充物理效果。

**交付边界：** 本轮完成场景定义、手工记录/导出和状态接口，实际执行与残留问题记录仍待用户环境验收；不能因源码 PR 合并勾选通过。

## 结束条件与后续维护

HC-00 的基础里程碑要求 HC-01～HC-12 的必要路径完成，并通过 HC-15 的基础家庭场景；HC-08 的高级视频、HC-13/14 分别记录为扩展能力，不用未完成扩展掩盖已可交付的基础体验。**整个开发总计划完成**则要求所有承诺的子范围有实现和相应验收，或由用户明确调整范围，不能提前关闭总跟踪项。

资料修订先更新本规格及相应 issue；实现状态写入具体交付/验收记录，不不断改写旧日期快照。每个实现 PR 必须说明：改变了什么用户可观察行为、数据与兼容迁移、异常/取消路径、实际运行范围和剩余缺口。完成“已写源码”不自动满足“实板通过”。

官方复用依据见[记忆研究](../../research/household-memory-open-source-2026-11-16.md)、[设备与音频](device-and-audio.md)、[呈现与任务](presentation-and-tasks.md)。具体上游 API/模型在接入时按固定版本核对，避免将最新网页能力误当当前项目已支持能力。
