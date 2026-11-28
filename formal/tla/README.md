# 小呆的 TLA+ 孪生与对话自测

每个模型都描述小呆在一个具体场景里可以做什么，并配有调用真实实现的本机检查。先写出人说的话、设备收到的事件和用户能观察到的结果，再确定哪些顺序、权限和进展可以用 TLC 检查。

## 现在覆盖的对话场景

| 用户遇到的情况 | 小呆应有的反应 | 孪生与实现 |
| --- | --- | --- |
| 家人在旁边说话，或一个已设置的定时提醒到了 | 被动观察不能开启远端收音。提醒只占用播报，完整唤醒或有意触摸才打开对话。过期的语音起始事件不能进入新一轮。 | [ListeningGate](firmware/ListeningGate/ListeningGate.tla) · [固件状态机](../../firmware/components/bot_runtime/bot_runtime.c) · [提醒回归](../../firmware/components/bot_runtime/tests/test_proactive.c) |
| 「请告诉我……树叶为什么是绿色的」在正式提交前接着说 | 停顿可以触发一次预识别。新音频抵达要丢掉旧候选，Mac 等本轮声明的帧到齐后才接纳完整提问。 | [UtteranceCommit](protocol/UtteranceCommit/UtteranceCommit.tla) · [设备提交](../../firmware/components/brain_link/brain_secure.c) · [Mac 音频输入](../../server/bot-server/src-tauri/src/brain/media.rs) · [网关回环测试](../../server/bot-server/src-tauri/src/brain/gateway.rs) |
| 用户问完了，声纹还没回来，或者答案要想一会儿 | 普通回答可以先走。慢回答在条件允许时可简短应一次，刚唤醒、近期刚应过或已收到首个回答内容时抑制提示。两句音频即便以后一句先准备好，也依原顺序送出。 | [ReplyTiming](macos/ReplyTiming/ReplyTiming.tla) · [识别任务](../../server/bot-server/src-tauri/src/brain/orchestration.rs) · [工作进程通道](../../server/bot-server/src-tauri/src/brain/worker.rs) · [流式回答](../../server/bot-server/src-tauri/src/brain/fast_core.rs) |
| 讲小兔故事时，用户点「补一句」，说「把小兔换成小狗」 | 补话紧接原轮时，保留原问题，并只将有完整播放回执的前文标为已播。还没播过正文也能补话。点补话后立即停止，网关接纳当前轮或为这次补话预留的直接后继轮次的关闭请求。「重新说」、停止、跨会话及跳过一轮均不能复用这次补话许可。 | [Supplement](protocol/Supplement/Supplement.tla) · [网关](../../server/bot-server/src-tauri/src/brain/gateway.rs) · [补话上下文](../../server/bot-server/src-tauri/src/brain/orchestration.rs) · [对话核心替身测试](../../server/xiaodai-core/tests/codex_fake.rs) |
| 思考中补一句，模型先把补话通知送出，steer 的确认还没被调用方处理 | 确认之前的通知先留在这次请求的屏障里。确认成功才交给新的回答者，而且只交一次；拒绝、超时或原轮已结束则丢掉这次补话的早到输出，沿原有的新轮退路。上一次补话的通知不能清掉下一次的等待。 | [SteerHandoff](macos/SteerHandoff/SteerHandoff.tla) · [Codex steer](../../server/xiaodai-core/src/codex.rs) · [替身回归](../../server/xiaodai-core/tests/codex_fake.rs) |
| 话说完之前先提交，或失败提示还在准备时控制台取消了这一轮 | 尾帧没到不能进模型。停止、排空超时或新的一轮先抬高计数，迟到的帧和过期的失败或提醒提示都不能再获得播放权。 | [DeferredGateway](macos/DeferredGateway/DeferredGateway.tla) · [网关](../../server/bot-server/src-tauri/src/brain/gateway.rs) · [提交孪生](protocol/UtteranceCommit/UtteranceCommit.tla) |
| 小呆正在播报，用户点停止，或等它讲完接着问 | 停止释放播放权，旧回执不能启动下一轮。未打断时，在当前回答所需的显示、播放、最终呈现回执都到齐后才能进入追问。委派说明同样要等最后一段的回执，后台任务不因此结束。 | [TurnPlayback](protocol/TurnPlayback/TurnPlayback.tla) · [固件及 Mac 事件映射](../../docs/specs/household-companion/tla/README.md) |
| 「帮我查资料」之后又问别的，或者结果在断线后才到 | 本轮可以先结束。结果留在收件箱里，不靠旧的播放许可播出。下次设备自己开始的轮次，没说话就先说简短结果；直接提新问题则先回答，结果改期。丢了通知可以再扫，不把任务重跑一遍。 | [TaskDelivery](macos/TaskDelivery/TaskDelivery.tla) · [任务库](../../server/bot-server/src-tauri/src/brain/tasks.rs) · [网关](../../server/bot-server/src-tauri/src/brain/gateway.rs) |
| 「小呆，晚安啦」和「讲个晚安故事」 | 前一句是道别，短回应完成后安静退出。后一句是请求，继续讲故事。「先别说再见」和「谢谢小呆，继续讲」也要保持对话。 | [ConversationClose](macos/ConversationClose/ConversationClose.tla) · [真实中文输入与判定](../../server/bot-server/src-tauri/src/brain/flow.rs) · [播报结束后关闭会话](../../server/bot-server/src-tauri/src/brain/gateway.rs) |
| 网络写不动，或旧语音已经在发送队列里时点停止 | writer 独立、有界；本地停止推进代次，未开始的旧输出丢弃，在途写的旧回执不能授权新轮。 | [GatewayDispatch](protocol/GatewayDispatch/GatewayDispatch.tla) · [实际 writer](../../server/bot-server/src-tauri/src/brain/gateway/io.rs) |
| 两个连接同时准备同一份任务结果 | 只有赢得持久 CAS 的连接能发送；失败者不能撤销获胜者的租约。 | [DeliveryClaim](macos/DeliveryClaim/DeliveryClaim.tla) · [网关](../../server/bot-server/src-tauri/src/brain/gateway.rs) |
| Luna 还占着上一轮的会话，新问题抵达 | 有界等待后使用已有 HTTP 策略；不借用或改写 Luna 线程，容量满明确拒绝。 | [LunaAdmission](macos/LunaAdmission/LunaAdmission.tla) · [核心适配](../../server/bot-server/src-tauri/src/brain/fast_core.rs) |
| 模型不断输出，消费者或 RPC ACK 很慢 | 消息和字节有界；保留已交付前缀，溢出明确失败，不静默跳过语义。 | [SemanticBuffers](macos/SemanticBuffers/SemanticBuffers.tla) · [RPC](../../server/xiaodai-core/src/codex.rs) |
| 名字输入是角色称呼或辱骂，旧资料又已有此名字 | 新写入先共用校验；旧资料保留供更正，不据此作口头身份称呼。 | [NameAdmission](macos/NameAdmission/NameAdmission.tla) · [名字策略](../../server/bot-server/src-tauri/src/memory/names.rs) |
| “一分钟后提醒我喝水”、取消或遗忘后旧回执才来 | 唯一意图只提交一次；只读查询不写库；取消撤播放权但保留已经听见的事实；归档过期与主动遗忘分开。 | [CapabilityExecution](macos/CapabilityExecution/CapabilityExecution.tla) · [助理入口](../../server/bot-server/src-tauri/src/brain/capability_dispatch.rs) · [原子领域事务](../../server/bot-server/src-tauri/src/memory/assistant_actions.rs) |
| 空闲结果查询被数据库锁住，或停止清理很迟才执行 | Stop 和状态继续处理；准备工作有上限；清理只撤旧租约，不能误停下一轮。 | [CancellationCleanup](macos/CancellationCleanup/CancellationCleanup.tla) · [有限后台工作](../../server/bot-server/src-tauri/src/brain/gateway/jobs.rs) |

## 修改与新增功能

### 2026-11-25：随时回应设计的模型影响

[Issue #36](https://github.com/JasonGuoo/esp-bot/issues/36) 的[十步规格](../../docs/specs/responsive-assistant/implementation-plan.md)和[验收映射](../../docs/specs/responsive-assistant/acceptance.md)规划交互控制、网关等待、Luna 隔离、独立任务交付、能力操作与迟到身份工作流。**本次为文档-only：未修改运行行为、`.tla`、`.cfg` 或 `checks.sh`，没有新模型检查或实板通过声明。**

后续首次实现各行为时，在同一 PR 更新受影响的现有孪生并建立所需新孪生。当时规划的目录随后分步实现：`macos/TaskDelivery` 已随 RA-04 加入；11 月 27 日新增 `protocol/GatewayDispatch` 及上表五个 Mac 模型。`macos/ResponsiveControl` 和完整 `macos/LunaWorker` 仍未实现。能力、身份、上下文和恢复的模型范围以验收映射为准。#35 的 steer 交接是 Mac 侧并发，先用 `macos/SteerHandoff` 记录「早到通知只交接一次、撤回后不漏进新回答者、旧通知不能清掉新的等待」；它不替代尚未实现的 LunaWorker。RA-00 不改 `TurnPlayback`：模块契约已经允许 final 内容段在独立任务仍运行时结束播放，这次只是 Mac 不再比契约更严。RA-04 给 `TurnPlayback` 加上委派前台：最后一段的硬件回执仍然是进入追问的条件，后台任务不因此变成完成。`macos/TaskDelivery` 检查收件箱和租约。

控制循环的进展不应依赖模型/网络/数据库必定完成；业务完成则明确其健康与公平调度假设。有界队列、丢弃/合并策略、来源撤回、未知效果核对、重连与持久回执均需对应实现回归。下方 11 月 20 日记录只证明当时六个模型的范围；11 月 27 日的新增模型和本机检查见[集成记录](../../docs/review-local-integration-2026-11-27.md)，不能扩展为完整 RA-01～10 或实板通过。

### 通用要求

1. 每次变更指出受影响的场景和决定它的组件。核心状态、任务并发、资源上限、等待策略或协议语义变化时，在同一变更中更新相应的 `.tla`、`.cfg` 和实现回归。首次触及尚无模型的核心行为时建立孪生。若只重构实现而可观察的行为一致，写明原因并复跑对应检查。对纯文案、素材也评估是否会改变行为，例如一句文案可能是实际参与识别的唤醒词。
2. 按决定行为的组件建立 `formal/tla/<责任域>/<模型名>/`。设备本地状态放在 `firmware/`，Mac App、Rust 后端及其工作进程放在 `macos/`，跨设备的消息与回执放在 `protocol/`。各端共用同一份协议模型。新平台引入独立决策时增加相应责任域。每个模型目录包含同名的 `.tla` 和 `.cfg`，以及调用该场景实际实现检查的 `checks.sh`。在上表登记场景、模型和实现入口。
3. 表达用户可观察的错误，例如上一轮的硬件完成通知使新一轮提前追问。写出绝不能发生的结果，需要保证进展时也写出在什么条件下最终必须完成。为输入数量、队列、轮次数给出有限边界，为线程或设备进展给出公平性假设。发生反例时，将原话或事件顺序带回实现，修复后重跑模型和回归。
4. 在变更交付中记录影响判断、运行命令、实际结果和仍待验证的部分。首音延迟、误截句、回答是否接住补充、语气是否合宜，使用实际录音、回执、对话回放或真人试听评价。无板时先完成本机可重复的检查。


小机器人 / 系统声音回退、独立首句通道、Stop 及迟到结果归属见 [LocalSpeech](macos/LocalSpeech/README.md)。

## 本机运行

新增功能或修复完成后使用统一的 [功能回归入口](../../tests/regression/README.md)：`./tools/check-functional-regression.sh`。它同时执行以下所有孪生、实际代码回归与本机构建，并保留按用户场景区分的报告。单独模型入口仍用于定向排查。

```sh
./tools/check-tla-twins.sh
./tools/check-tla-twins.sh macos/ConversationClose
```

第一条自动发现所有孪生并逐一执行 TLC 和同目录的 `checks.sh`。第二条只运行指定场景。原有 `./tools/check-turn-playback.sh` 等价于指定 `protocol/TurnPlayback`。TLC 使用固定的官方 TLA+ Tools 1.7.4，首次下载后校验 SHA-256。本机需要 Java、Apple Command Line Tools、已按项目配置的 ESP-IDF 头文件和已生成的板级配置，以及锁定的 Cargo 依赖缓存。自测使用本机回环连接、临时数据和模型替身。[检查脚本](../../tools/check-tla-twins.sh) · [ESP-IDF 本机环境](../../docs/esp-idf-development.md) · [Mac 服务端](../../docs/mac-bot-server.md) · [TLA+ 官方发行包](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)

## 模型边界与验收

默认配置分别探索两个激活令牌、三个输入帧、两句异步准备的正文、两段可用作补话的旧回答、两轮各两段的播放，以及最多三个语义单元组成的道别输入。输入帧代表可识别的音频块，道别单元代表称呼、道别短语或其他内容。真实中文样例由 Rust 判定，真实设备的执行时间需用时间戳测量。进展性质以健康的传输、模型和设备回执能继续获得处理机会为前提。

补话模型从来源有效、允许补话的原轮开始，只跟踪本次临时许可和有完整回执的前文。普通家庭记忆的可见范围仍由实际权限、来源版本和上下文长度决定。道别模型从一轮可识别的输入走到该轮应当安静或继续的决定，真实回话内容由对话模型生成。

「提交前继续说」和「停多久才会过早提交」需要不同证据。11 月 19 日实板用「请告诉我……树叶为什么是绿色的」做过对照，较长停顿出现过提前截断。下次有板时，用同一句话和相同增益重复不同停顿，记录是否收齐后半句、语音结束到首次实际写入扬声器的耗时，并试听回答是否接题。播报中补话以当前可用的触屏操作为基线，声学打断另做实板验收。[实板对话与停顿记录](../../docs/review-dialogue-rhythms-2026-11-19.md) · [设备音频契约](../../firmware/HOUSEHOLD_INTEGRATION.md)

### 2026-11-20 本机记录

在未连接 S31 的 Mac 上，六个默认 TLC 配置均搜索完毕。两组固件 C 主机测试通过 ASan、UBSan 检查，23 个不同的 Rust 用例通过，Mac 服务端调试版构建成功。中文回归在修复前重现了「讲个晚安故事」被判成道别，修复后验证了故事请求、否定句和直接告别。补话测试使用进程内模型替身。

六个临时模型各注入一个错误，TLC 均产生反例。它们分别是提醒播完打开收音、提到晚安就关会话、回答等待声纹完成、补话引用尚未播出的故事、过早打开追问，以及复说后仍使用半句预识别。此次声学效果、真实模型的回答质量和真人听感仍待设备验收。
