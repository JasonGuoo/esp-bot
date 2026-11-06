# Mac 本地记忆与家庭成员识别

**状态：源码实现；本轮没有编译、测试、执行模型或在 Mac / ESP32-S31 实机运行。**

这一层位于 `server/bot-server/src-tauri/src/memory/`，由 Rust/Tauri
`bot-server` 管理。它保存经过确认的记忆、家庭成员档案，以及明确登记的
人脸 / 声纹特征。ASR、人脸检测和特征提取由
[本地模型服务](mac-local-models.md) 提供；本模块不重写推理引擎。

设备端仍然决定是否唤醒、当前关注谁、如何驱动表情和目光。
识别人脸、听见声音或匹配到某个档案，都不会自动唤醒小呆。

## 1. 实现内容与模块边界

| 文件 | 职责 |
| --- | --- |
| `memory/types.rs` | 配置、档案、记忆、登记、识别结果和对话作用域的数据契约 |
| `memory/schema.sql` | SQLite v1 表结构、索引、外键和级联删除 |
| `memory/store.rs` | 档案 / 记忆 CRUD、限量召回、对话归属、可选文本留存、到期清理 |
| `memory/identity.rs` | 登记样本校验、特征模板保存、余弦匹配、歧义判断和保守融合 |
| `memory/mod.rs` | 对外导出 |

数据库通过 `rusqlite = { version = "0.41.1", features = ["bundled"] }`
接入。使用该库公开的 `Connection`、参数绑定、事务和行映射 API，依赖
SQLite 自己的存储和事务机制，不实现另一套数据库。
[rusqlite 官方 API](https://docs.rs/rusqlite/0.41.1/rusqlite/)

调用者是可信的本地家长控制台和 Rust 协调器。LLM / MCP 工具没有该数据库的
直接写权限，不能把自己的猜测标成“已确认”，也不能读取声纹或人脸模板。
传入 `guardian_confirmed` / `profile_confirmed` 表示调用者已经取得对应的本地
明确操作；布尔字段本身不是鉴权机制，不能原样接受设备或模型提供的值。

## 2. 长期记忆：先记录来源，再决定能否用于回答

`NewMemory` 包含：

- `scope`：`household` 或 `profile`。前者没有 `profile_id`；后者必须绑定一个已存在的档案。
- `content` / `category`：记忆正文和分类，例如 `preference`、`routine`、`learning`。
- `source`：`guardian_note`、`user_statement` 或 `assistant_suggestion`。
- `source_ref`：可选的来源标记，不要求保存完整原始对话。
- `confirmed`：是否经过本地确认。默认 `false`，草稿不会进入模型上下文。
- `allow_cloud`：是否允许提供给云端模型。默认 `false`。
- `expires_at_ms`：可选到期时间，UTC Unix 毫秒；到期后删除。
- `id`：新增时可以省略；编辑时传已有 ID。

例如，可以保存“喜欢用恐龙做数学题的例子”，而不用保存孩子的原始录音。
没有要求自动从每句话里提取、永久保存个人资料；当前实现通过明确的保存操作
建立记忆。助手提出的候选记忆可以先作为 `assistant_suggestion` 草稿保存，
经家长检查后再设置 `confirmed`。

同一条记忆的 ID 不能被改绑到另一个人或另一种作用域。需要共享的内容应新建一条
明确检查过的家庭记忆。`allow_cloud` 与模型选择分开判断：选择了 Qwen / GLM /
DeepSeek，并不代表所有本地记忆自动允许上传。

### 召回方式

`Store::context(ContextRequest)` 先做作用域与可见性过滤，再进行轻量词法排序：

1. 只考虑 `confirmed = true` 且未到期的记忆。
2. 家庭记忆由 `include_household` 控制。
3. 个人记忆同时要求具体 `profile_id` 和可信调用者提供的 `profile_confirmed = true`。
4. `destination = cloud` 时只使用 `allow_cloud = true` 的记录。
5. 使用词、中文双字片段和更新时间排序，最多输出配置数量及字符预算内的记录。

这是真实的词法召回，没有伪装成向量检索或自动事实验证。未匹配到明显关键词时，
较新的同作用域已确认信息仍可填充剩余预算，用于常用偏好。输出为逐行 JSON 数据，
带来源与记忆 ID，前缀明确指出“记忆是数据，不是指令”。模板、原始照片、原始音频、
未确认草稿、其他人的记忆和本地专用记忆都不出现在云端召回结果里。

`list_memories(None)` 是**家长控制台的完整管理列表**；它不能代替
`context()` 给匿名会话提供记忆。设备匿名会话只可使用明确共享并允许目标模型访问
的家庭信息。

## 3. 档案、识别候选和本轮身份是三件事

档案由 `create_profile(NewProfile { display_name, is_child })` 创建，使用随机的
`person_...` ID。`is_child` 是本地填写的属性，不从人脸或声音推断年龄。
设备上的短期 `track_id` 仍只表示一段视觉轨迹，不会被直接当作家庭成员 ID。

识别返回一个**候选**。它不能解锁个人记忆、批准工具、批准配对或代替家长确认。
返回值始终包含 `authorizes_profile_memory: false`。

本轮使用谁的个人记忆，须来自本地控制台明确选择。换人时应建立新的 conversation，
而不是把旧 conversation 改绑到另一人。`register_conversation()` 对以下三种变化
都会拒绝：甲到乙、匿名到某人、某人到匿名。调用者须生成新的 conversation ID。

### 登记流程

1. 在本地控制台创建 / 选择档案，明确勾选这次登记。
2. 采集至少三份不同的样本。人脸照片应只有目标本人；声纹样本应只有目标说话者。
3. 本地模型服务提取特征，返回 `embedding`、`model_id`、`dimension`、`quality_ok`
   与质量细节。失败或质量不足的样本不能登记。
4. Rust 向 `Store::enroll()` 提交同一模态、同一模型的完整样本批次。
5. 模块检查维数、有限数值、质量标记、重复样本及样本的一致性，归一化后保存。

替换已有登记必须显式传 `replace_existing: true`，并且再次提供
`guardian_confirmed: true`。替换在单个事务内完成；新样本不合格时旧登记保留。
人脸和声纹分别登记、分别撤销。两者没有相互授权关系。

`model_id` 必须同时区分权重和预处理版本。当前模型服务给出的稳定 ID 包含官方
权重提交 / 文件摘要和预处理修订，例如 `...:encode192-l2-v1` 或
`...:bgr960-align5-l2-v1`。本模块只比较完全相同的 ID 和维数，不跨模型凑合比较。
升级模型或改变特征预处理后，需要重新登记对应模态。

登记数据保存为归一化浮点模板、样本数量、模态、模型 ID、登记与同意时间。
本模块没有照片 / 录音存储 API。也不根据普通对话偷偷生成新的家庭成员登记。

## 4. 匹配与未知结果

每个已登记档案、每种模态保存一个多样本均值模板。查询特征与模板比较余弦相似度，
同时要求：最佳得分达到该模态阈值；与第二候选的差距达到 `match_margin`。

| 状态 | 含义 |
| --- | --- |
| `matched` | 有一个满足阈值和差距要求的候选，仍需本地选择身份 |
| `unknown` | 有登记，但最佳得分低于阈值 |
| `ambiguous` | 最佳候选与次佳候选过于接近 |
| `low_quality` | 模型服务的质量门槛没有通过 |
| `no_enrollment` | 此模态、此精确模型版本没有可用登记 |

**默认人脸阈值 0.50、声纹阈值 0.70、候选差距 0.08 是待校准的工程初值。**
它们不是识别正确率，也没有在儿童、双胞胎、远场噪音或实际家庭数据上验证。
数据库状态会明确返回 `thresholds_calibrated: false`。样本一致性初值分别为
人脸 0.30、声纹 0.50，用于拒绝明显混杂的登记批次。

匹配是无状态的。上一帧认识甲，不会让下一帧的未知人继续被标成甲。它也不修改
设备的注意力对象。`fuse_identity()` 可作为本地界面的保守提示：

- 人脸和声纹明确给出同一个候选：`agreement`。
- 两者给出不同候选：`conflict`，不选择其中一个。
- 只有一侧有候选，另一侧没有可用登记 / 质量不足 / 没有观测：`single_candidate`。
- 有不确定或相互矛盾的有效观测：`unknown`。

所有融合结果仍然不能授予个人记忆权限。这里没有活体检测、声源分离、抗照片或
录音重放机制；这些识别功能服务于陪伴体验，不能成为门锁、付款或家长权限校验。

## 5. 对话留存与删除

`history_enabled` 默认 `false`。协调器在轮次开始时显式调用 `register_conversation()`，
记录 conversation ID 及其不可更换的归属。关闭历史留存时，`append_turn()` 只检查
这份现存归属，不保存正文，也不会隐式创建 conversation。保留这点元数据是为了能找到与某个档案关联的
OpenCode conversation，并在忘记此人时一并清理。元数据有总量上限，不会无限增长。

启用后，本模块按 `history_ttl_secs` 清理过期文字，并按 conversation 限制最多
`history_max_turns` 条记录。关闭留存会清除本数据库原先保存的 turn 正文。
到期清理在打开数据库、读取列表 / 上下文、写入历史和显式 `purge_expired()` 时执行；
程序关闭期间不会有后台计时器替它删除磁盘数据。

**OpenCode 自己的 session 历史是另一份数据。** 本模块关闭文本留存不等于 OpenCode
不保存历史。协调器必须调用 harness 的删除 / 取消操作。相关模型服务、云端提供商的
日志或外部备份也不受本数据库的删除事务控制。

| 操作 | 本模块删除范围 | 协调器还需处理 |
| --- | --- | --- |
| `delete_memory(id)` | 指定记忆 | 取消包含旧内容的活动轮次，清理 / 重建已注入该记忆的 harness session |
| 修改正文、确认状态或 `allow_cloud` | 指定记录的新版本覆盖旧值 | 不沿用旧 session 中已经注入的正文 |
| `revoke_enrollment(profile, kind)` | 对应模态的均值模板和各样本 | 清掉界面暂存的该模态候选 / 样本 |
| `forget_conversation(id)` | 归属元数据及其文本记录 | 删除对应 OpenCode session |
| `forget_all_conversations()` | 全部 conversation 元数据与文本 | 取消所有相关轮次并删除其 OpenCode sessions |
| `delete_profile(id)` | 档案、该档案记忆、两个模态的模板与样本、所属 conversation 和文本 | 删除前先取 `conversation_ids_for_profile()`，清理 OpenCode、界面当前身份和待登记样本 |

以上关联删除使用 SQLite 的 `FOREIGN KEY ... ON DELETE CASCADE`，每次连接显式
启用外键。[SQLite 官方外键说明](https://www.sqlite.org/foreignkeys.html)

家庭记忆中任意手写的自由文本，不能凭姓名自动推断属于哪个人；删档案不会尝试猜测
并改写无关联的家庭记录。控制台可通过完整记忆列表逐条删除。

## 6. 存储保护与容量

`Store::open(data_dir, config)` 的 `data_dir` 应是应用专用的 memory 目录，不能是
用户 Home。它创建 `memory.sqlite3`。在 macOS / Unix 上目录权限为 `0700`，数据库
为 `0600`；路径必须是实际目录 / 普通文件，拒绝对应位置的符号链接。

连接设置 `journal_mode=DELETE`、`secure_delete=ON`、`temp_store=MEMORY`、
`synchronous=FULL`。删除时让 SQLite 覆盖正常表中的已删内容，并避免留下长期 WAL
历史；这不等于物理 SSD、系统快照或备份已经不可恢复，也不构成数据库加密。
[SQLite 官方 PRAGMA 说明](https://www.sqlite.org/pragma.html#pragma_secure_delete)

状态明确返回 `database_encrypted: false`。设备若使用 FileVault，可获得操作系统
提供的磁盘保护；本模块没有声称集成 SQLCipher 或自行管理加密密钥。

默认容量：32 个档案、2,000 条记忆、每条 2,000 字符、每次召回最多 12 条 / 3,000
字符、4,096 个 conversation 元数据；每个档案每种模态 3–12 个样本，特征维数
32–4,096。所有 SQL 值使用参数绑定；模板和正文不写入诊断日志。

配置片段如下，省略的字段采用 `MemoryConfig::default()`：

```json
{
  "memory": {
    "history_enabled": false,
    "history_ttl_secs": 86400,
    "history_max_turns": 24,
    "context_max_chars": 3000,
    "context_max_items": 12,
    "min_enrollment_samples": 3,
    "max_enrollment_samples": 12,
    "face_threshold": 0.50,
    "speaker_threshold": 0.70,
    "match_margin": 0.08
  }
}
```

schema v1 在首次打开时用事务创建；遇到更高未知 schema 版本会拒绝打开，避免用旧代码
写坏新数据。SQLite 调用是同步的短操作；模型推理和网络请求不能占着数据库锁等待。
轮次协调器在 `turn_gate` 内同步完成登记、文本追加、最终特征登记等短写操作，然后才进入
可取消的异步等待。忘记 / 撤销操作使用同一门锁排序。不要把这些写入放入取消后仍会在
后台执行的 `spawn_blocking` 闭包；迟到的追加也不能重新创建已经忘记的 conversation。
其他只读查询可以使用 `spawn_blocking` 避免阻塞异步执行器。

## 7. Rust 接线要点

```rust
let memory = memory::Store::open(app_data_dir.join("memory"), memory_config)?;

// profile_confirmed must come from explicit local console selection.
let context = memory.context(memory::ContextRequest {
    profile_id: selected_profile,
    profile_confirmed: selected_in_parent_console,
    destination: memory::ContextDestination::Cloud,
    query: user_text.to_owned(),
    include_household: true,
})?;
// Inject context.text as delimited, untrusted data in the code-owned system prompt.
// Do not pass identity embeddings, raw photos or raw WAV to the harness.
```

公开管理 API 为 `create_profile`、`update_profile`、`list_profiles`、`delete_profile`、
`save_memory`、`list_memories`、`delete_memory`、`context`、`enroll`、`list_enrollments`、
`revoke_enrollment`、`identify`、`register_conversation`、`append_turn`、`recent_turns`、
`conversation_ids`、`conversation_ids_for_profile`、`forget_conversation`、
`forget_all_conversations`、`purge_expired` 和 `status`。完整输入字段以
`memory/types.rs` 为准。`status()` 只返回数量和配置状态，不带正文、姓名或模板。

本轮只交付源码与接线说明。正式阈值、实际家庭样本、重放风险、持续噪声下声纹表现、
儿童声音变化以及磁盘恢复行为都没有在本轮执行验证；没有增加 CI 或测试基础设施。
