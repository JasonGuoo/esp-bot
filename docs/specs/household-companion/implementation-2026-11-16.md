# 家庭陪伴实现交付：开发完成，待测试

日期：2026-11-16。代码以设计 PR #12 的合并提交
`5ae0c780e146a72ebf10f71f4d6976a0ed47e2fe` 为起点。
本文记录本次实现，原始设计和开发步骤保留在[总规格](README.md)与[实施计划](implementation-plan.md)。

后续[第二轮静态审查与修正](../../review-household-static-2026-11-16.md)记录家庭范围、
遗忘、任务取消、打印对账和分段呈现的源码修正及待测情形；不改变本页的待测试状态。

**交付状态是源码开发完成、等待本机和实板验收。** 本次没有运行 Rust、TypeScript、
ESP-IDF 构建，没有运行模型、打印机或实板，没有安装依赖、下载模型、烧录或建立 CI。
源码审阅、Python AST、JSON/TOML 和资源引用检查不代表构建或功能测试通过。
另在空的内存 SQLite 中检查了 memory v1–v5、tasks、artifacts 的建表声明；
没有读写用户数据库，也没有执行带真实数据的迁移或应用功能测试。
既有 [11 月 15 日验收](../../review-acceptance-2026-11-15.md)只证明当时版本。

## 1. 现在的实现路径

Bot 保留完整「小呆小呆」本地唤醒、104 条离线回应、触摸停止、单一 codec owner、
runtime 仲裁、表情和有界媒体播放。授权录音送到配对 Mac 后，ASR 与身份处理进入
不同的受管理 worker 通道；ASR 完成即可进行普通问答，声纹和多人分段可以晚到。
人物候选只用于标注，不能改变权限或自动选择私密资料。

Mac 将输入和失败状态写入家庭日记录，再使用 OpenCode 完成理解与工具工作。
当日共同话题不随识别到的家庭成员改变而分配到不同的日会话；显式私密范围仍隔离。
长工作通过持久任务运行，回答可以先接纳任务，随后显示真实阶段并分段播放结果。
取消当前播报、取消生成、取消某个任务以及取消打印队列是不同操作。

呈现先冻结语义，再并行准备该段 TTS 和画面；网络发送结束不是播放结束。
设备每段的真实 LVGL 显示与音频完成回执回到 Mac，下一段才获得发送机会。
后台记忆引用这些来源、版本与实际呈现记录，不把模型生成过的内容直接当成孩子听过、
理解或掌握过的知识。

## 2. Issues 与实现入口

以下均为**开发完成，待测试**；GitHub issues 保持 open，验收项不提前勾选。
HC-15 完成的是人工验收入口与场景定义，现场执行仍未进行。

| Issue | 源码交付 | 重点待测 |
| --- | --- | --- |
| [#14 HC-01](https://github.com/JasonGuoo/esp-bot/issues/14) | `brain/contracts.rs`、家庭来源、任务、呈现和 output lease 契约；WSS 能力协商 | 旧固件兼容、旧 runtime/lease 拒绝 |
| [#15 HC-02](https://github.com/JasonGuoo/esp-bot/issues/15) | `memory/household*.rs`、SQLite 增量迁移、家庭时区/日记录、修订/遗忘 | 午夜、保留时间、旧私密资料迁移、重启恢复 |
| [#16 HC-03](https://github.com/JasonGuoo/esp-bot/issues/16) | `brain/worker.rs` 独立通道、`orchestration.rs` ASR/身份并行与迟到写回 | 实际并行延迟、缺模型、短句和取消 |
| [#17 HC-04](https://github.com/JasonGuoo/esp-bot/issues/17) | 按日 OpenCode 会话、稳定策略指纹、原生 compact、当前 inbox 事件绑定 | 连续追问、压缩与前台交错、断线后不重放 |
| [#18 HC-05](https://github.com/JasonGuoo/esp-bot/issues/18) | `brain/tasks.rs` 持久队列、取消、恢复、同意图续填参数、来源失效 | 重启、并发、任务/播报分别停止 |
| [#19 HC-06](https://github.com/JasonGuoo/esp-bot/issues/19) | 播放用途、真实阶段/延时提示、进度语音缓存、表情联动 | ACK/progress 不提前开启追问 |
| [#20 HC-07](https://github.com/JasonGuoo/esp-bot/issues/20) | `presentation.rs`、`artifacts.rs`、冻结计划、不可变素材、实际回执 | 语音与图中数值一致、首帧门控、缺图降级 |
| [#21 HC-08](https://github.com/JasonGuoo/esp-bot/issues/21) | Pillow/FFmpeg 媒体制作、逐段 JPEG/PCM、Mac 预览/保存与可选真实笔顺数据 | 英语/汉字/图解、真实音视频、中文字体、设备资源 |
| [#22 HC-09](https://github.com/JasonGuoo/esp-bot/issues/22) | Graphiti 0.31.2 sidecar、来源核验、幂等摄取/删除/重建、SQLite 降级 | 实际图后端与中文模型组合 |
| [#23 HC-10](https://github.com/JasonGuoo/esp-bot/issues/23) | 事件、话题、学习证据、长期事实、持久后台作业与来源修订传播 | 多人事实归属、矛盾、删除不复活、前台优先 |
| [#24 HC-11](https://github.com/JasonGuoo/esp-bot/issues/24) | `src/household.ts` 时间线、来源、人工标注/未知/撤销、任务与作品、成员合并 | 操作冲突、来源跳转、模板与标注分离 |
| [#25 HC-12](https://github.com/JasonGuoo/esp-bot/issues/25) | CUPS/IPP 打印、固定 argv、意图幂等、job 状态与独立对账 | 实际纸张、离线/缺纸、提交未知不重复出纸 |
| [#26 HC-13](https://github.com/JasonGuoo/esp-bot/issues/26) | 本地 pyannote Community-1 适配、样本区间/重叠、合格段 ECAPA | 父母孩子轮流/同时说话及电视背景 |
| [#27 HC-14](https://github.com/JasonGuoo/esp-bot/issues/27) | ES8389 实际 DAC reference、ESP-SR AEC、失效门控与本地打断 | S31 四时隙、双讲、误打断、回声尾部；默认关闭 |
| [#28 HC-15](https://github.com/JasonGuoo/esp-bot/issues/28) | `brain/acceptance.rs` 与控制台人工报告、18 类场景、JSON 导出 | 全部现场观察与资源测量 |

已有缺陷同时提供源码修复：

| Issue | 修复 |
| --- | --- |
| [#7](https://github.com/JasonGuoo/esp-bot/issues/7) | Stop 由服务快照和 `device.activity` 决定，覆盖设备单独工作、本地采集和媒体播放 |
| [#8](https://github.com/JasonGuoo/esp-bot/issues/8) | 连接/5xx 冷却参与实际选路；保留 429 策略与已输出禁止回放 |
| [#9](https://github.com/JasonGuoo/esp-bot/issues/9) | 指纹区分策略/来源失效与动态召回重排；只有真正内容/权限失效才清上下文 |
| [#10](https://github.com/JasonGuoo/esp-bot/issues/10) | 声音设置 CLOSE 带有效页面 lease、revision 与最后已接受快照；Stop 仍优先 |
| [#11](https://github.com/JasonGuoo/esp-bot/issues/11) | 转发当前回合 `turn.synthesizing`，关闭语音回答时不发该阶段 |

## 3. 数据与长对话

### 家庭日来源和记忆

`runtime/memory` 的 SQLite 是来源权威；`runtime/tasks/tasks.sqlite3` 管理任务、
呈现、输出许可和回执。新表与旧表并存，旧 profile 私密历史不会自动变成共享资料。
新家庭归档初始关闭，需在“家庭时间线”中明确开启；保留期限、家庭 IANA 时区与云召回
是独立设置。开启之后产生的合法来源才按配置整理，不凭空补回未保存的录音。

音频不默认落盘。界面可检查转写、候选分数、阈值、质量、样本位置和人工修改历史，
没有录音时明确显示不可回放。候选相似度不是“识别正确率”。成员合并是控制台显式操作，
旧身份历史保留；原模板停用，不能把两个人的声纹样本自动拼在一起。

服务端目前用**接收时间**分配日记录。音频元数据带 `time_basis=server_received` 和
`occurred_time_uncertain=true`；跨午夜录音的实际开始时间尚需实机时钟映射验收，
不得把此时间误称为硬件采样开始时间。

修订、标注和遗忘使用来源 revision。相关派生记忆与图投影失效，旧模型上下文被撤销，
旧输出许可和依赖这些来源的结果/素材停止使用。删除的持久清理任务可在重启后续做。
已经交给打印机的外部效果保留最小 job 对账信息，不能用“忘记这段对话”伪造纸张被收回。

输出依赖使用版本键 `source-ref:<turn>:v<revision>`；旧补充记忆和独立保留的长期事实
使用独立版本键。任务与前台呈现都记录依赖，遗忘沿已有依赖传播。旧版本永久失效不会
阻止再次使用人工更正后的新版本。读取作品前也检查待完成清理，App 停止服务时仍不会
因为后台 worker 暂停而返回已删除的旧资源。

更详细的格式、迁移与 Graphiti 部署见[记忆实现说明](memory-implementation.md)。

### OpenCode 连续性和 compact

同一日会话按自身 gate 串行，后台 `agent.work` 使用独立工作会话；不同会话有总并发预算。
动态召回排序不会重建 native session。删除、权限收紧、过期等真实失效仍清理已注入内容。

每累计八个成功回合会尝试原生 compact。宿主先持久化唯一 compaction message ID，
再提交 `/api/session/{id}/compact`；POST 的接纳不代表压缩完成。gate 内只有有限的
接纳等待，查询原 message 的状态在 gate 外进行，不等待整个 native 执行队列空闲。
只有原消息的 `completed` 与摘要得到确认才推进水位。请求结果不明保留 pending，
不盲目重提；下一次协调或手动核对继续读原 ID。

SSE 必须先匹配本轮 `session.inbox.delivered.inboxID`，再接收本轮 assistant message
和 execution 终态。前一次 compact 的结束事件不能使新问题提前结束。断线/取消后
保留既有禁止重放保护，不靠重发用户问题恢复未知工具效果。

## 4. 呈现、媒体与做事情

`PresentationPlanner` 可以消费主模型同一轮输出的 `presentation.segments`，也可以
确定性拆分普通回答，不要求每问再调用一个规划 LLM。每段含语音、字幕、表情、画面需求
和停留时间；语义冻结后才能开始图片与 TTS。必需画面失败时不播放失去参照的台词，
设备显示降级说明，Mac 保留计划和失败原因。

作品使用内容摘要与来源元数据保存，Mac 可以查看逐段图片、语音、视频/PDF、下载文件，
并从来源跳到任务和回执。默认不归档时，普通前台呈现是短期播放缓存，过期清理；
用户明确发起的持久任务及其作品有独立生命周期。

`presentation.render` 支持文字、英语拼写、分数/数轴/概念图和 PDF。
汉字 `stroke` 可读取用户安装的 Make Me a Hanzi `graphics.txt` JSONL 数据，用真实
笔顺和 medians 制作逐笔预览；没有对应数据时只展示字形并标明限制，不生成想象的笔顺。
数据文件不随本仓库分发，需保留上游图形数据的 Arphic Public License 和来源。

`media.prepare` 只接受已经核验的工具来源 receipt 或本地作品。用户配置精确媒体域名后，
Mac 才能准备远程音视频；任意网页地址不会被当成媒体。FFmpeg/ffprobe 生成缩略图、
16 kHz WAV、MJPEG、帧索引与可用字幕。内容及其网址/时间片段来源保留在作品元数据中。

设备限制如下；这些是实现预算，不是性能测量：

| 边界 | 当前值/行为 |
| --- | --- |
| JPEG | 最大 480×272，每帧 ≤65,536 字节 |
| 段调度 | 同时只有一段等待实际回执，每次 runtime 累计 ≤64 段 |
| 声音 | PCM S16LE、16 kHz、单声道；有图时等真实显示后才开始 |
| 短视频 | Mac 最多选择原素材前 12 秒、≤60 帧、≤5 FPS 的片段；固件连续媒体窗口 ≤15 秒 |
| 回压 | 下载/解码/播放慢时实际帧率下降，不能宣称稳定实时 5 FPS |
| 超预算/超时 | 撤销当前输出，报告延后/失败；完整作品仍可在 Mac 查看，不静默裁去尾段后报完成 |
| 旧固件 | 没有 `presentation.segments.v1` 时沿用原单结果契约 |

完整 wire 字段与硬件限制见[固件集成说明](../../../firmware/HOUSEHOLD_INTEGRATION.md)
及 [Mac Brain v3](../../../shared/mac-brain-v3.md)；安装、来源许可和媒体制作细节见
[呈现实现说明](presentation-implementation.md)。

任务目前包括 `agent.work`、`presentation.render`、`media.prepare` 和 `print.submit`。
需要补参数的任务为 `awaiting_input`，控制台与同一家庭范围内的后续对话可续填原任务；
不会用一个新任务掩盖旧打印的未知状态。对话续填仍受来源的云上下文许可约束，控制台
可以直接补充。后台工作默认最多两个，前台独立接纳，过载、超时和缺依赖有明确结果。

唤醒回应复用已有离线素材；初始模型等待按实际阶段显示提示。持久任务在长等待中可播
缓存的阶段语音并配合表情，真实结果返回后切换到新计划。停止当前设备输出使旧 lease
失效；任务在断线后完成也不能自行夺回屏幕或重新播放。

打印由 CUPS/IPP 适配器接管，模型不能拼 shell 绕过该路径。明确打印意图、不可变 PDF/
图片、打印机和选项形成同一任务；`intent_key` 防止重试变成第二份。提交未知只查询原 job，
不重新 `lp`。队列接纳、打印机报告完成与人实际拿到纸张分开陈述。人工取消后仍可保留
实际已发生的队列效果与对账事实。

## 5. 拉取后准备

先停止旧 App，并保留完整 `~/小呆 Server/` 数据目录的备份，包含 `brain.json`、
`runtime` 与 OpenCode 会话状态。示例文件仅展示新增字段，**不要覆盖现有凭据、路由、
配对证书、模型目录和已保存家庭设置**。

```bash
git switch main
git pull --ff-only origin main
cd server/bot-server
npm run tauri -- dev
```

上面沿用项目已有本机开发入口，需要现有 Node/Rust/Xcode 与前端依赖；本次并未在交付
环境执行。打包和固件构建/应用分区烧录继续使用[Mac 文档](../../mac-bot-server.md)
与 [ESP-IDF 工作流](../../esp-idf-development.md)，保留 NVS、配对和模型分区。

| 设置 | 使用方式 |
| --- | --- |
| `memory.household_history_enabled` | 新库初始化默认为 false；首次使用在家庭页面开启并确认保留期限 |
| `memory.household_timezone` | 可填 IANA 时区如 `Asia/Shanghai`；初始化后以家庭设置中的数据库值为准 |
| `models.python` | 原本地模型环境的绝对路径；默认媒体 helper 继承它 |
| `models.diarization_model` | 可选、已经安装的 Community-1 本地模型目录；null 时不伪造多人分段 |
| `tasks.media_script` | App 自动解析随包 helper；开发环境可指定 `local-models/media_prepare.py` 绝对路径 |
| `tasks.ffmpeg` / `ffprobe` | 已安装程序的可执行路径；图文/PDF 主要使用原环境的 Pillow |
| `tasks.allowed_media_hosts` | 远程媒体的精确域名列表；默认空，不自动开放所有网站 |
| `tasks.default_printer` | 实际 CUPS 队列名；未指定时任务进入等待必要信息 |
| `tasks.stroke_data_path` | 可选 Make Me a Hanzi JSONL 数据文件绝对路径；默认 null |
| `memory.graphiti` | 默认关闭；需独立 Python、图数据库、LLM 与 embedding 配置，见记忆部署文档 |
| `presentation` | 默认 16 段、每段 200 字符、静态阅读停留 2.5 秒；设备另有总预算 |

完整字段样例见 [brain.example.json](../../../server/bot-server/examples/brain.example.json)。
可选多人依赖见 `local-models/requirements-diarization.txt`；Graphiti 依赖独立于声音 worker。
代码不会自动下载安装这些组件，不会因为“已启用”就报告模型可用。

AEC 与自动语音打断保留默认关闭。在真实 S31 麦克风/扬声器上确认参考时隙、误打断、
双讲和资源余量之前，继续使用半双工与触摸停止。

## 6. 首次人工验收顺序

在“任务与作品”创建人工验收报告，所有项目初始都是“尚未执行”；记录环境、固件、
App、OpenCode 与模型版本。可以先从以下主线开始，完整场景和预期由控制台列出：

1. 保持 AEC/Graphiti/diarization 关闭，确认启动、配对、完整词唤醒、短回应和 Mac Stop。
2. 开启家庭归档与合适保留时间；两名成员轮流提问、追问，检查同一日记录与迟到声纹。
3. 在来源页改标、标未知、撤销、修改转写，检查版本冲突、记忆来源与素材联动。
4. 让小呆分段讲解拼写、分数或概念图，核对第一帧、实际声音和中间段不提前追问。
5. 发起长资料/媒体任务，同时提另一个问题；分别停止播报、取消任务、补齐缺失参数。
6. 预览一个真实 PDF 后明确打印，检查 job 状态、离线、取消及未知结果，避免重复提交。
7. 连续对话触发 compact；次日继续话题。再删除来源并重启，确认 SQLite、图索引、
   任务/素材和 native 上下文都不能恢复被删内容。
8. 单独开启并验收 Graphiti、多人分段、真实笔顺数据、短媒体和 AEC；记录资源与失败，
   任一扩展不可用时回到已确认的基础路径。

本次人工报告与所有 issue 的验收勾选都留给实际执行结果。源码完成不自动关闭 issue。

## 7. 回退

停止当前 App 和工作进程后再回退版本。SQLite 迁移是增量增加表/索引，但旧 App 不理解
新任务、来源失效和 output lease 语义，不能让新旧版本同时写同一数据目录。需要完整
回退时恢复升级前备份并使用对应版本；仅关闭可选 Graphiti/diarization/AEC 可保留基础
主线，不需要抹除家庭档案。已经提交的打印先用真实 job 对账，回滚代码不会撤销外部效果。
