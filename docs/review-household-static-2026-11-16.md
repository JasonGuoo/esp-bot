# 家庭陪伴代码第二轮静态审查与修正

日期：2026-11-16。审查基线是 [PR #29](https://github.com/JasonGuoo/esp-bot/pull/29)
的合并提交 `969cca1739753b170f38853675031070d62a18af`。

**状态：本页所列源码缺陷已修正，等待构建与实际测试。** 按用户要求，本轮只阅读源码、
核对调用路径与官方接口、编辑代码、交叉复审和检查 Git 差异。没有编译 Rust、TypeScript
或 ESP-IDF，没有执行 SQL/应用/模型/打印/实板测试，没有安装依赖或运行 CI。
静态分析可以确认下列错误路径存在，不能证明修正后所有运行路径都通过。

## 1. 家庭身份、来源与记忆

### R01 — 选择说话人会意外进入个人私密范围（P1）

`companion.ts::sendTurn` 原来仍把所选成员直接填入 `profile_id`。新后端把这个字段解释
为明确的个人范围，因此家庭页面所说的“标注人物”实际会切换个人模型会话和记忆。

现在选择成员只写 `speaker_person_id`；“使用所选成员的个人私密范围”是独立、默认未勾选
的选择。共同范围换说话人保留当前互动；明确切换个人范围才清理当前界面并更换互动 ID。
关联 #15、#24。入口：[控制台](../server/bot-server/src/companion.ts)、
[页面模板](../server/bot-server/src/console-layout.ts)。

### R02 — “忘记这段”使用旧 ID，新的家庭记录可能没有删除（P1）

控制台发送的是 activation ID，OpenCode 与来源存储使用家庭日/个人日会话 ID。
旧 RPC 直接按 activation 删除模型和旧 conversation，无法命中新家庭记录。

现在先解析该 activation 涉及的模型会话并清除其缓存，再只删除这个 activation 的来源
及其派生依赖。同日其他互动的原文保留，后续上下文可从仍合法的来源重建。
关联 #15、#17、#24。入口：[RPC](../server/bot-server/src-tauri/src/brain/rpc.rs)、
`household_activation_conversations` / `forget_conversation_data`
（[家庭存储](../server/bot-server/src-tauri/src/memory/household.rs)）。

### R03 — 非法设置或旧修订被拒绝前，已经清理了有效作品（P1）

原顺序是预读来源、异步删除上下文/作品、最后提交设置或来源修改。预读并没有验证全部
设置；例如开启留存后提交 `history_enabled=false` 与非法时区，可能先删除作品再报错。
预读与提交之间的身份写回也可能使修订号变化，导致无效请求仍使任务失效。

现在先在 SQLite 事务中完成实际校验和修改，成功后先阻断对应任务/素材，再处理原生
上下文清理；来源 outbox 保留后续恢复工作。身份作业 abort 后等待其退出，不让最后一次
同步来源写入越过修改前的边界。关联 #15、#18、#24。
入口：[RPC](../server/bot-server/src-tauri/src/brain/rpc.rs)、
[Brain 取消](../server/bot-server/src-tauri/src/brain/mod.rs)。

### R04 — 修改另一轮话语会让独立保留的长期事实消失（P2）

原 `revision_changed` 使整个 episode 的派生物失效。已确认且原话到期的事实无法重新
抽取，因此同一 episode 的另一轮修订就可能使它永久隐藏。

现在保留所有仍有效的引用版本都一致、未被显式遗忘或抑制的已确认到期事实；真正依赖
被修改来源的事实继续失效。重新抽取也会移除已经无效的残留项。关联 #15、#23。
入口：[家庭来源修订](../server/bot-server/src-tauri/src/memory/household.rs)、
[记忆整理提交](../server/bot-server/src-tauri/src/memory/household_jobs.rs)。

### R05 — 正常回答阶段覆盖了 ASR 的时间和质量依据（P2）

`journal_finish` 直接替换 metadata，进入 running/completed 后丢失接收时间基准、
模型、质量与片段时间范围。现在用 `json_patch` 合并状态字段。原递归净化继续移除
嵌套转写正文、录音和 embedding；关闭归档时不会借此保留 ASR 文字。关联 #16、#24。
入口：[journal_finish](../server/bot-server/src-tauri/src/memory/household.rs)。

### R06 — Graphiti 删除重试丢失 UUID，阻塞后续索引（P1）

成功的 delete job 清空 payload 后再次入队，原 UPSERT 只改状态，没有恢复 UUID。
obsolete ingest 还会重复触发此路径。跨 episode 依赖的修订分支另有“先清空 payload，
后排删除”的顺序错误，可能漏掉已经发到图后端的 ingest。

现在先持久化远端 UUID 的删除任务，再清空旧 ingest；delete job 再次入队会恢复原 UUID。
主 episode 和依赖 episode 使用一致顺序。关联 #22、#23。
入口：[revision_changed](../server/bot-server/src-tauri/src/memory/household.rs)、
[图任务队列](../server/bot-server/src-tauri/src/memory/household_jobs.rs)。

## 2. 任务、取消与打印

### R07 — 取消、完成和服务停止存在并发窗口（P1）

原取消读取状态后释放锁，再写取消状态；期间调度器可以启动它，完成回调也可把已经取消
的任务改回成功。打印 checkpoint 会覆盖刚写入的取消意图。服务停止只发出 scheduler
abort，尚未等待退出就收集 runners，可能漏掉最后注册的工作。

任务取消现在与调度认领采用相同的 active → DB 锁顺序；最终状态在数据库事务中重新
判断，外部动作保留取消意图和实际回执。停止时先等待 scheduler 退出，再收集 runners。
前台轮次的取消检查和移出 active 表也放在同一临界区，避免返回已取消的成功结果。
关联 #18、#25。入口：[任务服务](../server/bot-server/src-tauri/src/brain/tasks.rs)、
[Brain::turn](../server/bot-server/src-tauri/src/brain/mod.rs)。

### R08 — 停止互动误伤仍应继续的后台模型工作（P2）

“停止互动”原来广播全局模型代次取消，后台任务正在使用的 TTS/图片工作也会被中断。
现在普通 `parent_stop` 只结束前台持有的 worker exchange 和当前输出；全局卸载与服务
停止仍能释放所有模型。持久任务继续由任务页单独取消。关联 #7、#18。
入口：[cancel_scoped](../server/bot-server/src-tauri/src/brain/mod.rs)。

### R09 — 打印对账会被饿死、提前触发或被旧结果覆盖（P1/P2）

原自动对账先取最新 32 个通用任务，再过滤打印，旧打印可能永远轮不到；手动对账可以
在尚未提交时改写状态；手动和自动查询并发时，迟到的排队/错误结果还会覆盖已确认终态。

现在先按打印能力筛选，再按上次更新时间从旧到新轮询。手动对账要求已存在提交记录且
无 active runner。真实 IPP job-state 7/8/9 确认的取消/中止/完成不能被迟到状态倒退；
没有真实终态的任务仍保持 unknown/partial。UI 同步限制对账按钮出现条件。
关联 #25。入口：[任务服务](../server/bot-server/src-tauri/src/brain/tasks.rs)、
[任务控制台](../server/bot-server/src/household.ts)。

### R10 — 任务提议被拒绝，却仍回答“正在处理”（P2）

来源引用、媒体核验、续填范围或提交失败原来只写 warnings；没有任何任务入队时，
模型的原始肯定应答仍可能播放。现在统计实际接纳情况，全部失败明确说明未启动，部分
失败明确说明只有部分接纳；不再使用被拒绝提议附带的呈现内容。关联 #18、#19。
入口：[前台协调](../server/bot-server/src-tauri/src/brain/orchestration.rs)。

### R11 — 后台 Agent 再次转交任务或提问，却被标成成功（P2）

后台 `agent.work` 原来使用鼓励 tasks 的前台 schema，但没有执行返回的子任务；同时
忽略 `needs_input` / `partial`。现在明确执行已接纳工作，不再接受递归转交；缺参数进入
原任务的 awaiting_input，未完成保留 partial。关联 #18。
入口：[agent.work handler](../server/bot-server/src-tauri/src/brain/orchestration.rs)。

### R12 — “可打印”与否定语句可能被当成立即打印意图（P1）

原判断只要求引用中含 `打印` / `print`，会把“生成可打印的练习纸”或 “printable
worksheet” 当作物理打印授权。现在要求明确动作表达，英文使用独立 print 动词，并处理
否定词与动作被对象/副词隔开的情况。无法确认的表达不会提交打印，可在 Mac 明确操作。
这仍是有限的宿主意图识别，不宣称覆盖所有自然语言；首次测试应包含下表的正反例。
关联 #25。入口：[explicit_print_request](../server/bot-server/src-tauri/src/brain/orchestration.rs)。

## 3. 呈现与素材

### R13 — 复用旧图片时丢失来源依赖（P1）

新呈现引用旧 artifact/poster 后复制出新 JPEG，原来只绑定本轮文本召回来源，遗忘旧来源
可能漏掉复制件。现在准备素材前继承旧 artifact、任务和 presentation 的完整来源依赖。
关联 #20、#21。入口：[bind_presentation_sources](../server/bot-server/src-tauri/src/brain/tasks.rs)。

### R14 — 文件删除失败后，重试错误地认为已经删除（P1）

原 artifact 删除先清空 manifest，再 unlink；unlink 失败会让后续清理找不到来源。
现在在持锁状态下先删除最后一个有效引用的文件，再标记 manifest。失败保持可追踪、
可重试；相同内容仍有其他合法引用时保留文件。关联 #15、#20。
入口：[ArtifactStore::forget](../server/bot-server/src-tauri/src/brain/artifacts.rs)。

### R15 — 静默呈现按轮次停止，设备却未收到终止（P2）

前台已完成但还在显示或等待持久任务时，Turn 范围取消原来只丢弃服务端 Delivery。
现在发送绑定原 runtime 的终止事件，撤销输出许可并清理对应音频/显示通道；任务本身
保持独立。关联 #7、#19、#20。入口：[设备网关](../server/bot-server/src-tauri/src/brain/gateway.rs)。

### R16 — 图片实际失败后仍播放引用它的旁白（P2）

固件只检查 image_done，失败也满足这个条件。现在失败段不启动 codec，先汇报未开始
音频的失败，再汇报整段失败，避免缺图却说“看这里”。关联 #20、#21。
入口：[playback_prepare](../firmware/components/brain_link/brain_secure.c)。

### R17 — 完成回执的队列顺序使机器人无法继续追问（P2）

transport 已收到真实音频 DONE，但 runtime 尚未消费该事件时，最终 PRESENTATION_DONE
原来会被拒绝且不再重试。现在保留终结意图，等同一播放的真实 DONE 被消费才准备追问；
失败、取消、新轮次和新段都会清除它，不提前开麦。关联 #19、#20、#21。
入口：[bot_runtime](../firmware/components/bot_runtime/bot_runtime.c)。

### R18 — Mac 长回复只朗读第一段（P2）

后端已经拆分多个语音素材，控制台仍只读取兼容字段 `audio.wav_base64`。现在按计划
逐段读取语音，检查分块边界、MIME、SHA-256，并使用同一个播放器继续下一段。每次异步
读取后检查轮次与播放器归属，停止或新轮次后旧读取不能恢复播放；旧单 WAV 路径保留。
关联 #20、#21、#24。入口：[控制台播放](../server/bot-server/src/companion.ts)、
[共享素材读取](../server/bot-server/src/household.ts)。

### R19 — 宿主否定“已完成”后，模型分段仍可覆盖否定结果（P2）

原效果校验仅替换 Scene.speech，但 PresentationPlanner 优先使用模型提供的 segments。
现在效果未经确认时同时舍弃旧结构化呈现，前台和后台使用一致的宿主失败说明。
关联 #20、#25。入口：[呈现准备调用](../server/bot-server/src-tauri/src/brain/orchestration.rs)。

### R20 — 笔顺海报重影与模型卸载遗漏（P3）

真实笔顺渲染已画标题，通用渲染又在偏移位置重复描绘，现在记录已绘制标题；缺数据的
字形降级仍画正常标题。Python worker 的显式 unload 也清除新增 diarization_pipeline。
关联 #21、#26。入口：[媒体准备](../server/bot-server/local-models/media_prepare.py)、
[模型 worker](../server/bot-server/local-models/worker.py)。

## 4. 官方接口与静态复核

本轮继续采用已有唯一 codec owner、ESP-SR/BSP 接线和 OpenCode 原生服务，没有引入
新的 Agent 循环或任务框架。核对了固定版本 OpenCode v2.2.5 的
[session API](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/protocol/src/groups/session.ts)、
[session events](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/schema/src/session-event.ts)
与 [message projection](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/schema/src/session-message.ts)。
本次未发现可仅凭这些源码确认的 compact API 字段错误，未据此宣称在线运行通过。

对修复分别进行了另一轮源码交叉检查，重点跟踪锁顺序、drop/abort、旧 token、来源修订、
数据库事务和实际回执。`git diff --check` 通过。没有执行编译器、测试脚本、应用或模型。

## 5. 拉取后优先人工验收（本轮均未执行）

| 场景 | 应观察结果 |
| --- | --- |
| 家长/孩子轮流选择说话人 | 同一家庭互动继续，只有勾选个人范围才使用私密会话 |
| 同日建立两段互动，再忘记其中一段 | 只清理选中来源与依赖，另一段原文仍在；模型不能再引用被删内容 |
| 关闭留存同时填写非法时区/保留天数 | 请求被拒绝，原设置、任务与素材保持有效 |
| 一条已确认事实的原话到期，再修订另一轮 | 无关已确认事实保留；依赖被修订内容的事实失效 |
| 连续修订来源，图删除完成后再次修订 | 删除 job 仍有原 UUID，后续 ingest 不永久堵塞 |
| 任务刚启动/刚完成时取消，再停止服务 | 可撤销任务不复活；服务停止后没有遗漏的新 runner |
| 后台准备语音时点击“停止互动” | 当前互动停止，独立持久任务继续；任务页取消仍有效 |
| 32 个新 partial 渲染与旧打印并存 | 老打印仍被对账；迟到排队结果不覆盖真实完成回执 |
| “请打印这份 PDF” / “print this worksheet” | 有合法素材、打印机和来源时建立一次打印任务 |
| “生成可打印练习纸” / “make a printable worksheet” / “不要把这张图打印出来” | 不因字面词汇提交物理打印 |
| 任务引用或参数被拒绝、后台缺参数 | 不播报虚假的已接纳/已完成；缺参数保留原任务 awaiting_input |
| 三段 Mac 语音，在第二段读取或播放时停止 | 顺序完整；停止后第三段不自动播放，旧异步请求不恢复播放器 |
| 设备图片下载/解码失败 | 不启动依赖画面的旁白；错误回执不冒充成功 |
| 固件最终完成命令先于 runtime DONE 消费 | 真实硬件完成后进入追问，取消/新轮次不能继承旧完成意图 |
| 复用旧图后遗忘其来源；模拟文件无法删除 | 复制件一起失效；删除失败保留待重试来源，恢复条件后可继续清理 |

所有关联 issues 继续保持 **开发完成，待测试 / open**。
总体配置与迁移仍见[首次交付说明](specs/household-companion/implementation-2026-11-16.md)，
固件细节见[集成记录](../firmware/HOUSEHOLD_INTEGRATION.md)。
