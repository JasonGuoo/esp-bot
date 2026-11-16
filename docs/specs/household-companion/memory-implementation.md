# 家庭记忆实现、部署与人工验收

日期：2026-11-16。覆盖 HC-02 / #15、HC-09 / #22、HC-10 / #23，以及 HC-11 / #24 的来源与标注接口。

**本次交付是已接入主链路的源码，状态为“开发完成，待测试”。** 本次没有构建 Rust/Mac App、安装模型或 Graphiti 依赖、启动图数据库、执行 ASR/声纹/MLX/云端模型推理、运行实机或 CI。Python 文件只做静态语法解析，SQLite 建表与迁移声明在空内存库做了语法检查；这不代表真实用户数据库迁移、模型或端到端验证通过。

设计依据为[家庭记忆规范](household-memory.md)、[开发计划](implementation-plan.md)与[官方开源调查](../../research/household-memory-open-source-2026-11-16.md)。本文说明当前实现及启用条件，不将上游已有能力当成本机已经配置成功。

## 1. 数据归属与默认开关

SQLite 是原始档案和派生记录的权威数据源。Graphiti 是可以删除、纠错和重建的检索投影；OpenCode 的工作 session 与 compact 都不替代档案。设备只传递显式唤醒后的输入，不新增环境录音、自动联网 Agent 或声纹授权逻辑。

| 项目 | 当前行为 |
| --- | --- |
| 家庭日记录 | 一个 `household_id`，按家庭 IANA 时区建立 `daily_conversation_id`；人物是参与者/事实主语 |
| 新共享档案 | 默认关闭；旧个人文字留存开启，不等于同意新增家庭共享档案 |
| 新档案预设保留时间 | 30 天，可在家庭设置选择 1–365 天；关闭时不持久化输入/生成回答正文 |
| 不保留正文时 | 仍可建立日记录、来源 ID、状态等必要元数据；不因此生成可长期召回的原话 |
| 云端召回 | 默认关闭；开启只影响之后接收的来源，不追溯提升旧资料的权限 |
| 原始音频 | 本实现不建立录音档案；来源页返回 `audio_available=false`，不能冒充可回放录音 |
| 自动声纹 | 仅产生候选，不能选择私人记忆范围、获得家长权限或批准打印 |
| 私人范围 | 显式 `private_profile_id` 保留；不会因匹配到某个人而变为共享范围 |

配置文件仍是现有数据根目录中的 `brain.json`；新安装默认数据根为 `~/小呆 Server/`，已有安装沿用[目录迁移规则](../../mac-bot-server.md)。记忆数据库仍为数据根下的 `memory/memory.sqlite3`。

新建家庭时读取下列 `memory` 字段。家庭建立后，家庭设置页和 `household.settings.set` 修改 SQLite 内的实际设置，不以重启覆盖用户选择：

```json
{
  "memory": {
    "household_history_enabled": false,
    "household_retention_secs": 2592000,
    "household_timezone": "",
    "household_allow_cloud": false,
    "graphiti": {
      "enabled": false,
      "python": null,
      "script": null,
      "config_file": null,
      "allow_cloud": false
    }
  }
}
```

旧 `history_enabled`、`history_ttl_secs`、`history_max_turns` 继续管理旧历史，不用于限制整个新家庭日档案。空时区按 `TZ`、系统 zoneinfo 链接或系统时区文件探测；无法识别时使用 UTC，并返回 `timezone_source=fallback_utc`。设置页显示实际使用的 IANA 时区。修改时区只影响后续分日，不重写已保存日期。

时间保留 `occurred_at_ms` 与 `recorded_at_ms`。没有可靠捕获 UTC 映射的音频，以服务端接收时刻为分日依据，并标明时间不确定；不能把设备单调时钟当 UTC。跨午夜真实捕获对齐仍需实机验收。

## 2. 迁移及来源链路

`Store::open` 将 v1–v4 顺序迁移至 **schema v5**。迁移新增家庭表，不改绑旧 conversations、旧个人记录和生物模板。旧会话通过 `household_legacy_links` 提供独立的旧记录入口，不自动将其正文导入共享家庭上下文。

新增主要表包括：

- `households` / `household_days` / `household_episodes`：家庭、日期与有界互动片段。
- `household_turns` / `household_source_revisions`：原始来源、生成回答、状态与修订。
- `household_segments` / `household_identity_observations` / `household_annotations`：逐段自动候选、人工标注及撤销历史。
- `household_receipts`：实际呈现或效果回执，与“模型生成完成”分别保存。
- `household_derived` / `household_derived_sources`：事件、话题、学习观察、长期事实及其来源关系。
- `household_jobs` / `household_graph_projection` / `household_graph_rebuild`：持久作业、图投影映射及分批重建标记。
- `household_tombstones` / `household_source_cleanup`：遗忘抑制及跨数据库作品/任务清理 outbox。
- `household_model_conversations`：家庭档案与 OpenCode 工作会话的关联，支持显式清理而不混淆 compact。
- `legacy_memory_evidence`：旧待归属观察重新分配时保留原始 quote；没有原始版本的旧记录明确标为旧证据，不能补造 revision。
- `household_source_ref_versions` / `legacy_memory_source_refs`：呈现、任务与记忆之间的版本依赖；永久失效的是旧版本，不是以后所有修订。
- `profile_aliases` / `profile_merge_events` / `active_enrollments`：显式合并的人物别名、历史操作与当前可匹配模板视图。

`journal_input` 使用外部 `turn_id` 作为幂等键。音频先以空正文记录 `received` 元数据，随后 ASR 用同一键填写正文；后续更改必须走修订接口。ASR/推理失败、取消和生成完成都能落到既有来源上。模型生成完成只填 `assistant_generated`，不声明用户已经听到。

当前 episode 按同一 activation、15 分钟窗口和最多 12 个 turn 分段。每日档案不受这 12 个 turn 限制。后台每次读取有限片段，长输入明确标记为 excerpt；整天不会作为一个巨大的图 episode 发送。

后台单个 job 的全部原文摘录合计最多 6,000 个 Unicode scalar，单来源最多 2,000 个；每条来源还只带有限的生成文字、最多 8 个身份片段和 2 个回执摘要。完整原文摘要哈希、来源修订和回执计数参与版本判断。完整档案不因该模型预算被截短；未进入摘录的题目或回答不能被包装成已验证学习证据。

原始来源例子：

```json
{
  "conversation_turn_id": "ct_...",
  "daily_conversation_id": "day_...",
  "source_revision": 2,
  "text": "妹妹最近喜欢恐龙。",
  "private_profile_id": null,
  "allow_cloud": false,
  "segments": [
    {
      "speaker_segment_id": "ct_...:s0",
      "automatic_person_id": "person_father",
      "annotated_person_id": null,
      "annotation_set": false,
      "overlap": false
    }
  ]
}
```

人工标注改变说话者关联，不改变声纹模板。`person_id=null` 表示人工标为未知；`clear_annotation=true` 表示撤销人工覆盖、重新显示自动候选。两种操作都保留审计记录。迟到的自动结果保存为 observation，不覆盖已有新修订，也不重跑前台回答或任务。

重复人物的合并是受信控制台显式 `profile.merge` 操作，要求 `source_profile_id`、`target_profile_id` 和 `guardian_confirmed=true`。当前参与者、事实主语和私人资料所有者重定向到目标，私人范围和旧云许可保持原值；原识别候选、人工标注审计和旧会话保留原 ID。源人物保留为 `merged_into` 别名供历史名称展示，不能再作为新标注/登记的目标。源人物的生物模板原样留作非激活历史，`active_enrollments` 排除这些模板；合并不会混合向量、训练或自动登记到目标人物。需要新的有效模板时，应单独完成登记。删除目标人物会同时清理其合并别名及档案。

## 3. 声纹及分段的前置条件

ASR 与身份识别通过独立的受管本地工作通道运行；普通回答不等待身份识别。声纹登记仍使用现有受信 Mac 控制台、质量门与家长确认。

多人分段为可选适配：`models.diarization_model` 指向完整的本地 **pyannote Community-1** snapshot，依赖见 `server/bot-server/local-models/requirements-diarization.txt`。模型获取、许可接受和完整离线文件由操作者准备；运行时不自动下载。模型接口参考[官方 Community-1 说明](https://huggingface.co/pyannote/speaker-diarization-community-1)。

未配置 diarization 时，可以保存整段声纹候选，但 `single_speaker_verified=false`；它不等于证明整段只有一个人。重叠、短片段、低质量或没有登记模板的情况保持 unknown/候选，支持人工改标。ASR 与说话者时间范围目前是片段重叠对齐，不声称已经做到逐字强制对齐；原文引用跨度与音频样本区间分别保存。

## 4. 后台整理及可解释的学习记录

`brain/memory_worker.rs` 从 SQLite 领取带租期的作业。空闲约 3 秒后合并输入、身份修订和回执；本地模型位置沿用数据根的 `models/memory-qwen3-4b`，执行器为配置的 `models.rma_qwen_python` 或 `models.python`。模型文件缺失时作业留在队列，原文、状态、人工标注与前台交互继续可用。

整理器 `memory_extract.py` 使用本地 MLX，设置离线模式；没有工具调用和持久媒体输出。每个结果都经 Rust 再验证：

1. kind 必须是 `event`、`topic`、`learning` 或 `fact`。
2. 来源 ID、revision 必须与仍存在的快照一致。
3. quote 必须是对应版本原文中的连续文本；Rust 计算 Unicode scalar 跨度。
4. 私人来源使派生物保持同一私人范围；混合不同私人所有者的结果不写入。
5. 云端权限取所有引用来源的交集；自动抽取不能把关闭的权限变为开启。
6. 自动观察保持 `confirmed=false`；人工确认与云端权限是两个不同动作。
7. 派生记录、来源关系与图索引 outbox 同事务提交，之后才推进 `processed_version`。

示例派生来源：

```json
{
  "conversation_turn_id": "ct_...",
  "source_revision": 2,
  "speaker_segment_id": "ct_...:s0",
  "quote": "妹妹最近喜欢恐龙",
  "span_start": 0,
  "span_end": 8,
  "span_unit": "unicode_scalar",
  "source_expired": false,
  "occurred_at_ms": 1794787200000
}
```

人物既可以是说话者，也可以是陈述的主语；`subject_entity_ids` 与 `speaker_segment_id` 不互相替代。`assertion_kind` 区分 observed、self_reported、reported_by_other、uncertain。`topic_id` 由家庭与稳定话题名生成，供跨日关联。事实变化保留各次原话和发生时间；自动观察不宣称已经解决所有冲突。

学习证据采用以下等级，不生成掌握百分比：

| 等级 | 记录含义 |
| --- | --- |
| `introduced` | 接触了内容，不等于学会 |
| `self_reported` | 学习者自称理解或会做 |
| `correct_with_help` | 有题目、回答和可定位的提示来源 |
| `correct_independently` | 有题目、实际回答，提示情况为 none |
| `retained_later` | 有跨日先前回答与再次回答的来源，不由同一次聊天推断 |

高级学习观察需要 `learning` 结构：`knowledge_point`、`question`、`answer`、`hint_level`、`question_source_id`、`answer_source_id`；有提示需 `hint_source_id/hint_quote`，跨日需 `prior_answer_source_id`。机器人提出的题目还要引用 `question_receipt_key`，且对应完成回执的 `rendered_text` 包含题目。这个文本来自服务端已冻结的呈现段，不能信任设备上传的自由文字。

后台最多带入 4 条仍有效的先前学习原始来源作比较。新学习记录必须引用当前片段，不能把旧来源重新算成今天的学习。即使结构完整，模型评价仍为待核对观察，实际语义判定质量留待人工验收。

前台新输入会改变 generation，后台 MLX/Graphiti 请求被取消，其受管进程组被回收。抢占不计作失败；失败采用有限重试，依赖或配置缺失显示 blocked。重启后可回收超时租期，继续持久队列。

## 5. Graphiti 的真实部署条件

适配固定为 **graphiti-core 0.31.2**，独立依赖清单为 `server/bot-server/local-models/requirements-graphiti.txt`。所用官方接口为 `Graphiti.add_episode`、`Graphiti.search`、`SagaNode` 以及 driver/实体关系 CRUD，源码依据见 [v0.31.2](https://github.com/getzep/graphiti/tree/v0.31.2)。没有安装参考 MCP 服务，也没有把其内存队列作为本项目作业系统。

此实现需要操作者提供：

- 一个单独的 Python 环境及上述固定 core 依赖；运行时不会 pip install。
- 本机可连接的 FalkorDB 或 Neo4j 服务；目前没有内嵌/打包 FalkorDB Lite，也没有替用户启动 Docker 或安装数据库。
- 可用的 OpenAI-compatible **chat/completions** 模型端点，支持配置的 JSON 输出模式。
- 独立明确的 embedding 端点、模型名与维数。不能把现有 GLM/DeepSeek 聊天配置当作已有 embedding 服务。

可选环境安装命令供人工准备时执行，本次交付没有执行：

```sh
python3 -m venv .venv-graphiti
.venv-graphiti/bin/python -m pip install -r server/bot-server/local-models/requirements-graphiti.txt
```

在 `brain.json.memory.graphiti` 指定该环境 Python 的绝对路径，以及用户管理的 `config_file` 绝对路径。`script=null` 使用 App 随附的 `graphiti_worker.py`。Finder 启动的 App 不一定继承终端 PATH，应使用完整路径。

下面是 **结构示例**，模型名需替换为实际已运行端点支持的值，不能原样视为可运行配置：

仓库还提供可复制的 [graphiti.example.json](../../../server/bot-server/examples/graphiti.example.json)，以及 [brain.example.json](../../../server/bot-server/examples/brain.example.json) 中的独立开关。

```json
{
  "backend": {
    "kind": "falkordb",
    "host": "127.0.0.1",
    "port": 6379,
    "database": "espbot_household"
  },
  "llm": {
    "base_url": "http://127.0.0.1:8000/v1",
    "model": "REPLACE_WITH_LOCAL_CHAT_MODEL",
    "structured_output_mode": "json_schema"
  },
  "embedding": {
    "base_url": "http://127.0.0.1:8001/v1",
    "model": "REPLACE_WITH_LOCAL_EMBEDDING_MODEL",
    "dimension": 1024
  },
  "allow_cloud": false
}
```

Neo4j backend 使用 `kind="neo4j"`、`uri`、`username`、`password_env` 和可选 `database`。两个 backend 都限制在 loopback，本实现不开放远程图数据库。

远程模型端点必须是 HTTPS，并显式提供 `api_key_env`。配置仅写环境变量名，进程从环境取实际密钥；不会自动读取 OpenCode 子进程的钥匙串注入值。远程调用同时要求 sidecar 配置、`memory.graphiti.allow_cloud` 和本次来源权限允许。一个 episode 中只要包含不允许云端的被引用共享观察，就不能把整个 episode 发送给远程模型。

模型/embedding 配置文件与固定适配器版本进入 `engine_signature`。配置改变会使旧投影不可召回，并按 episode 分批排重建，前台不一次展开整年的档案。更换实际数据库地址属于存储迁移，旧数据库仍需单独管理；应用只管理当前配置指向的投影，不能宣称删除外部备份或此前复制的数据库。

sidecar 是 App 管理的 JSONL 子进程，只接受 health/ingest/search/delete，按请求 ID 匹配结果，有输入输出大小与时间限制，没有公开 HTTP/MCP 管理监听端口。第三方 stdout/stderr 不进入用户日志；Graphiti telemetry 明确关闭。

`household.graph.status` 返回配置状态、固定版本与签名。它不为了读取状态而调用模型或探测数据库：`configured=true` 只表示配置检查通过，`ready=false`、`models_not_probed=true`、`db_reachable=null` 和 `model_calls_verified=false` 明确说明尚未证明服务可用。`client_initialized` 只说明曾初始化客户端，不能证明当前连接健康。正常作业或搜索是否成功，以对应的实际回执为准；依赖/模型/配置缺失返回相应 not-ready 错误，不返回假搜索结果。

## 6. 召回、compact、纠错与删除

前台保留 SQLite 的有界近期来源和已提交记忆。共享对话可向 Graphiti 做有软时间预算的混合检索；失败或超时回退到 SQLite。索引只接收验证过的共享观察，私人原文不进入家庭图。图命中还必须回查 SQLite：当前图 UUID/签名、有效来源 ID/revision、未遗忘、未过期、当前目的地权限。`group_id` 只是分区，不能代替这一步检查。

`context_policy_fingerprint` 不使用每次 append 都变化的全局数据计数，也不依赖这次 query 的命中顺序。它覆盖当前范围可向云端召回的旧记忆内容/权限/到期，以及家庭策略修订。因此正常动态召回不会每轮删除 OpenCode 会话；更正、遗忘和撤权会使旧上下文失效。原生 compact 的实现和完成水位在 `harness/compaction.rs`，与家庭来源数据库独立。

| 操作 | 数据和派生物处理 |
| --- | --- |
| 改 ASR 原文或人工身份 | 校验 `expected_revision`，新增修订；旧派生物失效，旧图删除排队，重新整理；不重放工具 |
| 忘记一条派生记忆 | 抑制相关原始 turn 再次自动提取；原档案仍可单独查看，近期模型回填排除这些来源；同一来源的其他派生项也受抑制 |
| 忘记一轮/一天 | 清正文、修订、候选、标注、回执和相关派生物；保留不含正文的 tombstone；阻止旧 job/重建复活 |
| 原文到期 | 清原始内容；只有已经由用户确认的长期 `fact` 可保留最小证据并标 `source_expired=true`，不能声称仍可查看完整原文 |
| 删除人物 | 清私人来源、登记模板与其标注/相关派生断言；共享日记录保留其他家庭参与者，相关旧输出清理排队 |
| 关闭云端召回 | 撤销已有来源/派生物的云端许可，旧图清理与上下文失效；再次开启只授权新来源 |
| 显式清空全部对话 | `forget_all_household` 删除家庭来源并排清理；正常 harness invalidation 不调用这一方法 |

图删除的适配依据上游 `remove_episode` 的来源关联算法，保留其他 episode 支持的关系、删除独占关系/节点，并清除可能残留内容的实体/Saga 摘要。删除路径不调用 LLM 重建摘要，所以撤销云许可或移除模型凭据后，仍能只连接本机数据库做清理。SQLite 与图数据库不假装拥有跨库事务：预分配 UUID、持久删除 outbox 与成功回执负责恢复。

任务、呈现计划与作品库属于另一个数据库。来源清理会同时写 `household_source_cleanup`；后台调用 `tasks.forget_sources` 成功后才 ack。失败或进程退出不会丢掉清理意图。`household.jobs` 展示 graph 重建及 source cleanup 的等待数量。已经提交给系统的物理打印不能通过删除记忆“撤销已发生”，须保留准确的提交/未知结果语义。

跨库依赖有三种明确命名空间：

- `source-ref:<conversation_turn_id>:v<revision>`：家庭原始来源版本。人工纠错、标注、合并、过期或删除时，清理 canonical turn（原始输出）及全部旧版本引用；新修订产生的新 key 可再次用于回答。自动迟到身份不清理 canonical turn，不因此打断正在发生的普通回答。
- `legacy-memory-ref:<32 位 epoch>`：旧笔记的不可复用版本 key。SQLite 触发器在内容、来源、归属或权限改变及删除时原子排旧 key；TTL 和人物级联删除也覆盖。删除后恢复同一笔记 ID 或相同文字会获得新 key。
- `household-memory-ref:<32 位 epoch>`：已确认且原始正文到期后独立保留的长期事实。原始 quote 仍标 `source_expired` 用于审计；新回答依赖该保留事实 key，不能继续依赖已永久失效的原始版本。事实纠错、失效或删除会清理其旧 key。

取消这类已过期事实的人工确认，也撤回了独立保留许可：该事实会删除并排其作品清理；普通尚有有效原始来源的记录仅更新确认状态，不停止原始轮次的作品。

`MemoryContext.source_refs` 给出实际选入旧笔记上下文的依赖。为了覆盖 OpenCode native session 中早于最近 12 轮的内容，`household_day_source_refs(day, profile)` 提供当天所有有效、当前范围的版本引用；历史命名的 `legacy_context_source_refs(profile, cloud)` 提供当前可用旧笔记与独立保留事实的保守全集。它们只返回 ID，不返回额外正文，不包含其他私人范围；每类最多 2,048 个，超预算明确报错，不静默丢掉删除依赖。跨轮来源不会因为检索排名改变而脱离后续遗忘处理。

## 7. 受信控制台接口

这些是 Mac 控制台接口，不直接开放给设备、模型或任意 MCP 客户端。

| 方法 | 主要用途/输入 |
| --- | --- |
| `household.settings.get/set` | 实际时区、保留正文、天数、未来云许可 |
| `household.days` | 日期列表；`limit` 最大 365 |
| `household.timeline` | `daily_conversation_id`；单次最多 2,000 条来源 |
| `household.source` | `conversation_turn_id`；完整修订、自动 observation、人工标注、真实回执 |
| `household.annotate` | `speaker_segment_id`、`person_id`、`expected_revision`；可 `clear_annotation=true` |
| `household.correct` | `conversation_turn_id`、`text`、`expected_revision` |
| `household.forget` | 一个来源或整个日期；实际删除选择不受时间线分页限制 |
| `household.memories` | 按 kind/日期查看派生记录与学习证据 |
| `household.memory.confirm/forget` | 人工确认或来源级抑制，不授予云端权限 |
| `household.jobs/retry` | 作业状态与显式重试 blocked/failed |
| `household.graph.status/rebuild` | 实际配置状态及有界持久重建 |
| `household.legacy` | 旧范围元数据，原始私密归属保留 |
| `household.legacy.evidence` | `memory_id`；可用的旧原话证据与缺失版本说明 |
| `profile.merge` | `source_profile_id`、`target_profile_id`、`guardian_confirmed=true`；保留别名与模板历史，不合并训练模板 |

跨库更改先调用 `household_mutation_sources`，在使旧任务/作品失效前校验来源 revision，避免旧 UI 覆盖新标注。读取来源返回实际 join 得出的 household/day ID，供任务提交绑定检查。

## 8. 人工测试清单及边界

以下项目均仍为 **待测试**，不得以本文或代码存在替代验收结果：

1. Mac 构建、v1–v4 副本迁移和默认不开共享正文；旧私密/云许可/保留选择不扩大。
2. 完整“小呆小呆”唤醒后 ASR 与身份并发；身份很慢或缺模型时普通回答继续；失败来源可查。
3. 一天多次唤醒、两人交替/重叠、跨午夜及家庭时区变更；手动覆盖/撤销和旧 UI revision 冲突。
4. 同一输入重复投递、工作进程崩溃、前台抢占、租期恢复、失败重试，不生成重复或已删除记忆。
5. 原文纠错、旧候选晚到、忘记记忆/来源/人物、TTL 到期；SQLite、OpenCode、Graphiti、任务/作品库都完成对应清理。
6. 学习问题/回答/提示和真实呈现回执能回溯；“讲过”“孩子说懂了”不能写为独立答对或跨日保持。
7. 实际选定的中文 embedding、结构化输出模型、FalkorDB/Neo4j 版本、Mac 内存/启动耗时；配置改变重建不阻塞正常回答。
8. 长对话跨重启与原生 compact；旧权限撤销后不会从历史 instruction update、旧图或旧任务继续带回正文。
9. 重复成员合并后历史名称可查、新标注选择不再出现源别名；旧生物模板不参与匹配，目标旧私人许可不扩大。
10. 同一来源多次纠错之后，新版本仍能生成作品；旧版本作品被清理。旧笔记删后重建、确认事实原文到期、以及人物级联删除都沿版本依赖清理，不产生永久误封或残留旧内容。

当前没有自动语义合并所有昵称、可靠儿童声纹准确率、客观掌握率、完整音频回放、FalkorDB Lite 内嵌分发或数据库外部备份擦除的保证。上述项目需要对应模型与设备环境形成单独验收记录。
