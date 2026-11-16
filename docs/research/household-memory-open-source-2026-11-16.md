# 家庭记忆：Graphiti、Letta 与 Mem0 的复用取舍

核查日期：2026-11-16。本文记录官方文档与源码调研，不是模型效果评测。
没有为本次文档更新安装依赖、下载模型、执行推理、运行 Mac App 或增加 CI。
产品目标见[家庭记忆规范](../specs/household-companion/household-memory.md)，
实施工作见[HC-02、HC-04、HC-09、HC-10](../specs/household-companion/implementation-plan.md)。

## 1. 结论与职责分配

保留现有 OpenCode 作为前台 agent，SQLite 保存家庭原始记录与作业状态。
优先验证 Graphiti 作为可重建的时序知识图和检索组件；借鉴 Letta 的后台整理方法；
Mem0 保留为较轻事实检索的替代候选。这是一个组合决策，不是同时安装三套记忆系统。

| 候选 | 官方已提供的能力 | 对本项目的用途 | 取舍 |
| --- | --- | --- | --- |
| Graphiti OSS | episode、实体/关系、时间属性、命名空间、混合检索、Saga 及增量知识摘要 | 家庭关系、跨日话题、事实变化、来源检索的派生图 | HC-09 首选候选；实际中文、Mac 打包、模型和数据库组合待验证 |
| Letta / Letta Code | 有状态 agent、MemFS、跨会话记忆、后台 dreaming/reflection | 学习快照、水位、去重、后台合并的工程方式 | 保留 OpenCode；不引入第二套前台 agent runtime |
| Mem0 OSS | 事实提取、语义/词法/实体混合检索、显式 CRUD 与单条记忆历史 | 如果图组件成本不合适，可用于较简单的事实记忆 | 不把平台独有图/时间推理/Dream 功能算作 OSS 能力 |

没有一个候选能直接替代本项目的逐句说话者标注、家庭日记录、真实播放/打印回执、
源记录删除语义或学习证据等级。只在这些产品边界写薄适配，不自行实现通用图检索或另一套 agent。

## 2. Graphiti：选可复用的核心，保留本地权威来源

### 核查基线

候选版本为 [v0.31.2 发布页](https://github.com/getzep/graphiti/releases/tag/v0.31.2)，
发布于 2026-10-20。其 [pyproject.toml](https://github.com/getzep/graphiti/blob/v0.31.2/pyproject.toml)
与 [LICENSE](https://github.com/getzep/graphiti/blob/v0.31.2/LICENSE) 标明 Apache-2.0。
这只说明该代码版本的许可证；数据库、模型、权重和后续复制的示例仍分别记录来源与许可。

官方区分 [Graphiti 开源组件与 Zep 产品](https://help.getzep.com/zep-vs-graphiti)。
不能把托管 Zep 的整套产品能力直接当成安装 Graphiti 后已有的功能。

### 数据与时间能力

[官方命名空间说明](https://help.getzep.com/graphiti/core-concepts/graph-namespacing)
提供 `group_id` 分区，并明确由应用授权和从可信状态推导分区值。
[CRUD 文档](https://help.getzep.com/graphiti/working-with-data/crud-operations)
列出了 episode、entity、saga 及相关边类型。
本项目拟把家庭映射到 group、日记录映射到 Saga、有界对话片段映射到 episode，
人物与话题作为跨日实体；这是本项目的数据映射选择，不是上游内置“家庭模式”。

固定版本的 [graphiti.py](https://github.com/getzep/graphiti/blob/v0.31.2/graphiti_core/graphiti.py)
实现 `summarize_saga()`，使用新增摄入记录增量总结，区分摄入水位与事件发生时间水位，
并默认保存原始 episode 内容。预算限制下的摘要会丢失细节，因此不以摘要代替证据。
本项目另保留 SQLite 来源和稳定映射，图重建不依赖上一次模型摘要。

### 默认 Saga 摘要不能直接当家庭日记

该版本的 [summarize_sagas.py](https://github.com/getzep/graphiti/blob/v0.31.2/graphiti_core/prompts/summarize_sagas.py)
默认要求简短、独立的耐久事实概览，刻意弱化“谁说了什么”这样的对话过程。
这适合概览，不足以回答“今天聊了什么、孩子实际答对了哪道题”。
因此保留它的图/时间能力，在 bot-server 补有来源的事件、话题、学习观察、长期事实四类 schema，
不另写通用图抽取引擎，也不将默认摘要冒充完整家庭日志。

### 部署约束

[官方 MCP 服务说明](https://help.getzep.com/graphiti/getting-started/mcp-server)
将其标为实验性参考实现；示例默认还需模型/embedding 服务。
固定版本的 [MCP 服务目录](https://github.com/getzep/graphiti/tree/v0.31.2/mcp_server)
用于理解调用契约，不直接成为本产品的持久任务调度器。
拟使用受 bot-server 管理的 core sidecar 与现有 SQLite 作业队列，
向 OpenCode 暴露经过授权的检索接口，而非完整 `clear_graph` 等管理能力。

Graphiti core 依赖条件与嵌入式数据库不同：v0.31.2 的 FalkorDB Lite extra 只在
Python 3.12 及以上启用。若选 Lite，应隔离其环境，不为图数据库强制升级已工作的
音频 Python 环境。FalkorDB/FalkorDB Lite 的实际 Mac 分发、重启恢复、内存与磁盘预算
留在 HC-09 评估；本次没有声称它们已在用户 Mac 上运行。

**自托管存储不自动带来离线推理。** 接入时显式配置抽取模型、中文 embedding、
重排器及允许的数据目的地；不能假设现有 GLM/DeepSeek/MLX 路线与 Graphiti 的结构化
输出协议已经兼容。模型名、版本、维数、预处理变化都要进入投影版本。

## 3. Letta：借鉴后台整理，不替换 OpenCode

核查时 [letta-ai/letta](https://github.com/letta-ai/letta) 已将当前实现指向
[letta-ai/letta-code](https://github.com/letta-ai/letta-code)；旧 Python V1 资料与
当前运行时应分开阅读，不能照旧教程当作现在的安装入口。

[MemFS 官方说明](https://docs.letta.com/concepts/memfs) 描述了 Git 管理的记忆文件：
根目录资料进入常驻上下文，更深资料按需读取；默认没有语义/向量索引。
[Memory & dreaming](https://docs.letta.com/configuration/memory) 使用后台工作整理近期会话，
可以在步骤数或 compact 事件后触发；其后台 agent review 不等于要求用户逐次审批。
这些能力说明“长期有效”来自分层上下文与持续整理，而非无限增长的聊天窗口。

工程参考固定到 `eadef1c6c3594d1bf81517efd4b68fce54644ad4`：

- [reflection-transcript.ts](https://github.com/letta-ai/letta-code/blob/eadef1c6c3594d1bf81517efd4b68fce54644ad4/src/cli/helpers/reflection-transcript.ts)：来源 message ID、转录范围与去重。
- [reflection-launcher.ts](https://github.com/letta-ai/letta-code/blob/eadef1c6c3594d1bf81517efd4b68fce54644ad4/src/cli/helpers/reflection-launcher.ts)：后台整理启动与处理进度的协调。
- [memory-worktree.ts](https://github.com/letta-ai/letta-code/blob/eadef1c6c3594d1bf81517efd4b68fce54644ad4/src/agent/memory-worktree.ts)：隔离记忆编辑与合并。

对应本项目的工程规则：固定输入快照、来源版本/ID 去重、成功提交后再推进水位，
前台修改与后台派生物冲突时重新计算。我们复用现有作业/监督能力实现这个窄适配，
不把 Letta 的 session、权限与 OpenCode 并列成两套权威。

## 4. Mem0：事实记忆备选，避免按旧版宣传选型

官方 [OSS 新算法迁移说明](https://docs.mem0.ai/migration/oss-v2-to-v3)
描述单次 ADD-only 自动提取与混合检索，并将外部 graph store 集成移出 OSS。
这里的 ADD-only 是自动提取行为，不代表显式 `update` / `delete` 消失。
[平台与 OSS 对比](https://docs.mem0.ai/platform/platform-vs-oss)
仍列出 OSS CRUD/单条 history，Graph Memory、Temporal Reasoning、Dream 和汇总接口
则属于平台能力。不能从旧教程推断新 OSS 自带同样的时序图或后台整理。

源码观察固定到 `f1791daa86d469249c9b002f62644d58a1eb96a0`：

- [memory/storage.py](https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/mem0/memory/storage.py)：来源消息缓存只保留有限最近项，不能代替完整家庭会话档案。
- [utils/spacy_models.py](https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/mem0/utils/spacy_models.py)：实体提取的语言适配需要单独核查，不能假设中文姓名/亲属关系已可靠。
- [server/README.md](https://github.com/mem0ai/mem0/blob/f1791daa86d469249c9b002f62644d58a1eb96a0/server/README.md)：存在自托管服务/界面说明；对比页的“无 OSS dashboard”描述不能作为排除依据。

本项目暂不选 Mem0，原因是家庭需要跨人物关系、事件时间与来源修订，Graphiti 更贴近
这项数据组织工作；这属于基于需求的工程判断，不是未经测试的性能优劣结论。
若 HC-09 的 Mac 部署或模型成本不合适，保留现有 SQLite 词法召回可用，并评估 Mem0
替代检索层；替换不能改变上层来源、权限或删除契约。

## 5. OpenCode compact 的独立职责

[官方 compact 文档](https://opencode.ai/docs/compaction/) 的工作目标是控制当前模型
上下文；它不是家庭记忆数据库。近期原文、摘要、按需资料与持久源档案必须区分。
前台 session 的复用、重启恢复与动态召回注入由 HC-04 实现，不能用不断删 session
绕开上下文增长，也不能只保存向量片段而丢掉是谁在何时说了什么。

[Server plugin 文档](https://opencode.ai/docs/build/plugins/server/) 和
[API 文档](https://opencode.ai/docs/api) 是后续接线依据；项目当前源码目标仍是
OpenCode V2 2.2.5，使用后续接口前需确认版本兼容，不在文档 PR 中升级运行时。

## 6. 接入前必须形成的实际证据

HC-09 产出固定依赖/模型/数据库版本、许可来源、中文检索样例和 Mac 资源记录。
围绕“家长转述孩子偏好”“同名昵称”“昨天/明天/下周”“事实更正”“两人重叠讲话”
构造定向数据，分别检查实体合并、来源定位、时间过滤与未知处理，不能用上游 benchmark
替代家庭场景结论。

HC-10 重点检查：旧任务重试、删除后图重建、身份修订、重复 episode、后台写入后崩溃
以及受限来源进入派生摘要的情况。只有有实际 Mac/模型环境时运行对应验收，
逐项记录“已写、已构建、模型运行、实机验证”；不建立 CI，不为本次设计交付安装新组件。
