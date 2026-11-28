# 2026-11-27 本地集成记录

工作基线：`4b7b0a7`，分支 `codex/local-integration-20261127`。本机拉取 `origin/main` 后已是最新；RA-00 和 RA-04 已在主线。用户明确暂缓 #38，其余改进继续。本文记录当前工作状态，不把 issue 的设计清单当作实现或验收结果。

## 后续本机补充验收

11 月 27 日晚最新源码 App 实际文本轮次完成“现在几点”、创建并立即取消测试提醒“完成本机联调回归检查”；固定 SVM 特征映射回答产生两段模型正文和四段教学图，App 作品预览展示第一张实际渲染图。这里只证明 Mac 文本/预览路径，不是 S31 播放或完整真人听感验收。测试提醒已取消。

11 月 28 日新增统一的补充功能回归集并实际完整运行，68 个自动场景通过、16 个 TLC 配置通过；实板/真实模型及剩余能力明确另列。入口、结果与限制见[功能回归验收](review-functional-regression-2026-11-28.md)。

11 月 28 日按 owner 选择，安装版 App 已更新并启用小机器人 TTS，实际完成 Mac 报时朗读。新增回退/停止/完整性回归及 LocalSpeech 孪生；完整功能集现在 70 场景通过、17 个模型通过，19 外部/缺口项保留 not_run。详见 [TTS 接入与实际证据](research/tts-local-audition-2026-11-28.md)。

## 当前改动

| 用户场景 | 实现与责任 | 模型 / 回归 |
| --- | --- | --- |
| 网络写堵塞时停止 | Mac 独立 WSS writer：32 帧、8 MiB（包括在途）、3 秒写超时；停止提升代次，丢弃尚未开始的旧呈现。已开始的 TCP 写仍由设备 token 和本地 Stop 收窄权限 | `protocol/GatewayDispatch`；真实 Tokio duplex 背压 / 写失败 |
| 话没收齐就提交，或重复提交 | Mac 只封存与 token、stream、声明帧数完全匹配的输入；重复 commit 不能覆盖请求或延长 deadline；断线立即结算 | `macos/DeferredGateway`、`protocol/UtteranceCommit`；真实 TLS / PCM 回环 |
| 两个连接同时准备同一任务结果 | 播报前必须赢得持久收件箱 CAS；失败者只撤自己的 lease，保留获胜连接的输出 | `macos/DeliveryClaim`；live / 非 live 两条真实网关路径 |
| Luna 会话被上一轮占用 | 最多 2 个主机语义轮；200 ms 取得 Luna 通道失败时，仅按已配置策略走无状态 HTTP；不改变 Luna 线程归属；上下文已发送回执在持锁时捕获 | `macos/LunaAdmission`；实际锁竞争 / 路由隔离 |
| provider 不断产出，消费者很慢 | core 最多 4 轮、16 个 HTTP 路由；输入/输出、SSE、RPC 请求、通知、审计都有硬边界；溢出明确失败，保留已交付前缀；取消释放请求 waiter | `macos/SemanticBuffers`；真实 RPC duplex / HTTP 替身 |
| “爸爸”或辱骂被当成人名 | Bot Server 共用名字策略；创建、改名、声音组命名复用；事务记录策略版本；旧资料保留但不自动认证为可口头称呼的名字 | `macos/NameAdmission`；实际 SQLite 写入 / 重开 / 声音组绑定 |
| 没要求画图的简单分数 / 数轴问题 | fast core 可附加本机可校验静态模板；`presentation.media_preference=prefer/auto/off`，当前“只用文字”优先；不改已经冻结的句子，保留首帧门控 | `protocol/TurnPlayback` 原契约；实际 planner 与 Pillow 480×272 渲染 |
| DB 锁等待、旧 TTS 准备迟到 | 空闲结果查询移出连接循环；阻塞存储全进程最多 8 个；每连接最多 4 个语音准备，30 秒限时；清理独立额度、只带旧 lease ID；连接退出 abort 所属 async 工作 | `macos/CancellationCleanup`；锁定实际 SQLite、停止和迟到清理 |
| 明确的一次性提醒、查询时间/能力、查看和取消提醒 | 业务参数与宿主权限分开；缓存目录/时区；操作账本 + 原子 reminder/receipt；未知效果仅核对，不重放；取消通知使旧 TTS/播放失效，迟到实播事实仍可记账 | `macos/CapabilityExecution`；真实文本 turn、SQLite 并发/重开/来源更正/遗忘/私域、真实 WSS 取消 |
| SVM 特征映射的分步图解 | 共用冻结场景，保留点 ID、类别、映射、平面、原空间边界；四段均等实际首帧和旁白回执。明确是固定示意，没有执行训练 | 原 `TurnPlayback` 时序不变；Rust planner + Python 数值/实际 Pillow 渲染回归 |
| OpenCode 路由 transport / 503 失败 | 增加实际 `available_routes` 冷却与到期恢复回归，运行逻辑不变 | #8 原路由测试 |

名字策略是有限的词语校验，不是身份认证或完整语言分类器。旧姓名没有被重写。静态模板支持经过验证的分数、数轴和固定 SVM 分步图解；不声称已实现 #31 的连续动画、全部媒体规划或视频投送。

## 已执行的本机验证

- 基线 Rust 服务端：180 通过、3 个显式忽略。随后网关改动专项 40 通过；持续变更的最终总数以本文后续更新为准。
- 核心完整回归：90 通过（34 lib、37 Codex 替身、19 HTTP 替身），1 个文档用例显式忽略。日志 `/tmp/esp-bot-core-full-final-20261127.log`。
- 服务端 212 通过、3 个显式忽略：`/tmp/esp-bot-server-full-final-20261127.log`；随后 SVM 新用例单独 3 通过，Python 数值与实际渲染 4 通过。
- 提醒回归包含：DB 锁住仍可只读查询、同意图并发、跨库提交后重开、无证据 unknown 不执行、歧义/私域拒绝、来源更正/遗忘、归档过期、TTS 期间取消与迟到硬件回执。
- TypeScript / Vite 构建通过；本机 Tauri release 和签名 App 构建通过。App 在源码目录启动，没有覆盖安装版。
- 本机 ESP-IDF 6.1 固件构建通过：app `0x7f7f60`，8 MiB 分区余 `0x80a0`（32,928 B）。没有修改固件或刷写设备。
- 原有 9 个 TLA+ 孪生及关联实现检查通过。新增模型按各自检查执行；最终完整运行另记。没有 GitHub Actions / CI。
- 本地 `model-python` 中，用 Tingting 合成“请告诉我，树叶为什么是绿色的。”并送本机 MLX Whisper：TTS 1.569 s，3.093 s / 16 kHz 音频；冷 ASR 10.05 s，正确返回问题。不是板上收音。
- 源码构建 App 中向真实 Luna 发最小测试，请求只回答“联调正常”，2.9 s 得到正确回复；App 音频播放控件实际运行。没有把软件播放状态当作真人听感。
- 两个新静态模板用本机 Pillow 实际渲染并目视检查：`/tmp/esp-bot-media-20261127/`，各 480×272，分数 1/3 与数轴 -3～3，文字与参数相符。
- 完整 Rust 回归曾在本地 21:30 安静时段暴露一个测试夹具问题：它错误期待主动播报。修正夹具使测试时刻处于允许时段，产品安静时段行为保持不变；专项重跑通过。

构建、测试均设置 `DEVELOPER_DIR=/Library/Developer/CommandLineTools`。本机没有连接 S31 USB 串口；用户确认稍后接板，先继续本机开发和测试。

## 尚未完成的范围

- #36：补上 RA-02/03 的部分隔离、RA-07a 名字校验、RA-04 竞争修复，以及 RA-05/08 的目录、操作账本与一次性提醒/时间查询闭环。完整 control reducer、存储/呈现事件化、通用上下文投影、版本化任务修改、自然语言身份/记忆/设备动作、历史关联及整体恢复验收仍不能标完成。
- #31：已有分数、数轴和 SVM 四步图解。Luna 媒体意图和竞速隔离、连续动画、多源视频、连续媒体会话、专用浏览器捕获仍不能标完成。持续播放的 codec/profile 选择依赖本板基准，不以本地构建代替。
- #34、#27 及旧 HC 验收项：完整词唤醒、AEC/声学打断、硬件扬声器/屏幕回执、真人对话体验需要接板。保留完整“四音节” MultiNet7、默认半双工和主人音量；不调整阈值冒充声学通过。
- #38 按用户确认继续暂缓。

远端 issue 没有关闭或评论；尚未提交 / 推送。本文会随后续实现和真实验收更新。

SVM 模板验证命令：`<本机 model-python> -B -m unittest discover -s server/bot-server/local-models -p test_teaching_media.py -v`；四帧实际图与数值证据位于 `/tmp/esp-bot-media-20261127/svm/`。先核对了 [scikit-learn 官方核函数示例](https://scikit-learn.org/stable/auto_examples/svm/plot_svm_kernels.html) 和 [Pillow ImageDraw](https://pillow.readthedocs.io/en/stable/reference/ImageDraw.html)，继续复用现有 Pillow renderer；本模板的径向映射是显式示意，并非 RBF 或训练最大间隔面的宣称。
