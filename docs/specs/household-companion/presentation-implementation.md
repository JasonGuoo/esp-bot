# Mac 任务、图文呈现、媒体与打印实现说明

更新：2026-11-16。对应 HC-05、HC-07、HC-08、HC-12。

本文记录本次写入的服务代码和配置契约。**开发完成，待 Mac 和真实设备测试**：本次没有执行 Rust/macOS 构建、模型推理、媒体转码、真实笔顺数据适配、CUPS 打印或 ESP32 实机验收，也没有安装工具、下载模型/笔顺数据或运行 CI。Python 文件只做语法解析；这不代表媒体、字体或打印运行验收通过。

## 已写入的职责和调用路径

Bot 继续使用已配对 Mac，不运行媒体检索、视频解码、打印或长任务 Agent。前台 ASR/对话可以提交后台任务；后台任务不占前台 turn gate，输出需要另外获取当前设备的 output lease。

| 服务 | 实现范围 | 关键边界 |
| --- | --- | --- |
| `tasks.rs` | SQLite 任务、事件、attempt、来源引用、呈现版本、输出 lease、实际回执；排队、取消、恢复核对、补参数 | 对话结束不等于任务取消；未知打印结果不会自动重交 |
| `presentation.rs` | 冻结语义段、语音/图片依赖、停留时间、fallback、素材绑定 | TTS 与图片准备使用同一已冻结语义；素材增加形成新 revision |
| `artifacts.rs` | SHA-256 内容文件、独立来源记录、大小/容量限制、分块读取、校验、删除 | 同一组字节可被多个记录引用；删除最后一个有效引用才删除文件 |
| `capabilities.rs`、`media_prepare.py` | Pillow 图文模板、PDF、真实笔顺数据、FFmpeg 媒体准备 | 固定 argv、受限输入输出、进程组取消；没有任意脚本执行协议 |
| `capabilities_print.rs` | `lp` 提交、IPP/CUPS 状态核对、`cancel` 取消 | 已提交与已完成分别记录；CUPS 完成回执仍不是纸张传感器证据 |

OpenCode 仍是唯一 Agent 引擎。`agent.work` 的 handler 由 Brain 注册，使用独立工作 session；任务服务只负责生命周期和状态，不另造一套工具推理循环。模型的 `tasks` 是意图提议，`source_quote` 是用户请求的引用；打印授权由可信 host 判断，模型的声明、声纹结果和人脸候选都不提供操作权限。

## 配置与依赖

实际配置位置以控制台显示的 `config_path` 为准，新安装通常使用 `~/小呆 Server/brain.json`。以下是需要合并到现有配置的 `tasks` 示例，绝对路径占位符要替换成自己的路径：

```json
{
  "tasks": {
    "max_concurrent": 2,
    "max_tasks": 10000,
    "max_asset_bytes": 67108864,
    "timeout_seconds": 300,
    "python": "/Users/你的用户名/esp-bot/server/bot-server/local-models/.venv/bin/python",
    "media_script": "/Users/你的用户名/esp-bot/server/bot-server/local-models/media_prepare.py",
    "ffmpeg": "ffmpeg",
    "ffprobe": "ffprobe",
    "allowed_media_hosts": [],
    "default_printer": null,
    "print_monitor_seconds": 120,
    "stroke_data_path": "/Users/你的用户名/小呆 Server/models/hanzi/graphics.txt"
  }
}
```

打包应用会解析 `media_prepare.py` 的资源路径；开发源码运行时可填绝对路径。Pillow 复用现有 [local-models/requirements.txt](../../../server/bot-server/local-models/requirements.txt)，不要把 `tasks.python` 指向另一个没有该依赖的 Python。FFmpeg/ffprobe 不随本功能自动安装；配置的程序名会先在 PATH 查找，默认名称还会检查 Mac 常见的 `/opt/homebrew/bin` 和 `/usr/local/bin`。自选安装位置可直接填绝对路径。

`allowed_media_hosts` 是独立的、精确匹配的 HTTPS 域名名单，默认空。服务还要求当前真实工具结果里存在那个素材 URL，随后将来源记录为 server 生成的 receipt。下载时重新检查 DNS、公网地址、每次重定向、内容类型和大小；FFmpeg 只处理已经存入本机的受限文件，不直接联网。来源必须实际可访问；失败返回具体 unavailable/failed 状态。

能力状态中的 `script_present` / `python_present` / `ffmpeg_present` 只是文件或程序是否存在。Pillow、解码器、字体、CUPS scheduler 是否真正可用在执行时确认，不以存在检查冒充功能验收。

## 图文与教学模板

`presentation.render` 的 `input.visual` 支持：

| kind | 必要内容 | 当前生成物 |
| --- | --- | --- |
| `text` | `title`、`body`，可选 `steps` | 480×272 JPEG、完整语义 JSON、可打印 PDF |
| `spelling` | `word`，可选 `gloss`、`example` | 单词与逐字母拼写卡、完整文本 PDF |
| `bars` | `series`、`unit` | 数值柱形图和 PDF；前台图表仍检查本轮真实数据证据 |
| `diagram` | `diagram.type` | 分数、数轴、概念/步骤/比较节点图和 PDF |
| `stroke` | `character` 单个汉字 | 有真实数据时生成逐笔素材与练习页；缺数据时返回明确标注的字形预览 |

小屏正文可能截断为摘要，原文保留在语义 JSON 和 PDF。`spelling` 当前针对单个英文字词；`diagram` 使用有边界的声明式结构，不接受 HTML、JavaScript 或任意绘图程序。模板不替代知识来源核验，特别是具体事实、数值、图表单位仍须与本次对话和工具证据相符。

例如：

```json
{
  "capability": "presentation.render",
  "input": {
    "visual": {
      "kind": "spelling",
      "word": "apple",
      "gloss": "苹果",
      "example": "I have an apple."
    }
  },
  "source_quote": "做一张apple的拼写卡"
}
```

这段 JSON 只是模型意图格式；真正的 `NewTask` 由 host 绑定家庭、日会话、本次输入、幂等键及来源依赖后提交。

## 汉字笔顺：使用预装真实数据

适配器依据 [Make Me a Hanzi 官方格式](https://github.com/skishore/makemeahanzi#format)读取 `strokes` 和同序 `medians`。官方原始图形文件名是 **`graphics.txt`**，内容为每行一个 JSON 对象。把同一 JSONL 文件命名为 `graphics.json` 也可；也支持一个包含 `字.json` 文件的目录。目录模式适用于同格式的单字数据。大型整体 JSON 数组/映射和 pretty-printed 的多字 JSON 不是本版输入格式。

准备方式是自行取得上游图形数据，保留上游版权/许可文件，放在自己的只读模型/资料目录，然后填写 `tasks.stroke_data_path`。本仓库不捆绑第三方字形数据；启动、第一次显示或遇到缺字都不会偷偷下载。默认 `null`，缺文件、缺字或数据无效时明确降级，不能从字体轮廓或 LLM 坐标猜笔顺。

来源与许可需区分：上游 [COPYING](https://github.com/skishore/makemeahanzi/blob/master/COPYING) 将 `graphics.txt` 归于 **Arphic Public License**；字典文件使用另外的许可，本适配器不读取字典。生成素材的来源清单保留项目链接、许可链接、数据文件摘要、字记录摘要及笔顺体系。该数据以 PRC 笔顺为目标，不把它描述成所有地区唯一写法。[Hanzi Writer 的本地数据/原始 SVG 文档](https://hanziwriter.org/docs.html#loading-character-data)提供了同源数据的现有使用方式；这里复用数据格式，在 Mac 准备素材供薄终端播放。

具体实现：

- 最多 48 笔，原始文件最多 128 MiB，单字记录最多 512 KiB；逐行查找并校验有界路径/中线数据。
- 严格使用上游笔画数组顺序和坐标变换 `screen_y = 900 - source_y`。普通 SVG `M/L/H/V/Q/C/Z` 路径转为 Pillow 图形；遇到超出该子集的曲线命令时使用上游中线引导，并在 metadata 标注 `source_medians`。
- 只接收数值路径语法，保留本地 SVG 文件供完整矢量显示，不加载 SVG 内脚本、链接或外部资源。
- `semantic_speech_with_tasks` 在 TTS/图像准备前读取笔数，写入 `_stroke_record_sha256` 并冻结语义。随后资料变化会报错，避免旧口头说明配新笔顺。
- 输出 `preview.jpg`、`strokes.png`、`strokes.mjpeg`、`frames.json`、`glyph.svg`、`stroke-source.json`、语义 JSON 和 PDF。PDF 附逐笔练习页。
- MJPEG 是逐笔示意帧：一笔一帧，加开始/完成帧；5 fps 时间索引，最多 50 帧。红点取自真实 median 起点。它不是笔压仿真或对孩子书写动作的评分模型。

调用示例：

```json
{
  "capability": "presentation.render",
  "input": {
    "visual": {
      "kind": "stroke",
      "character": "永",
      "title": "永字的笔顺"
    }
  },
  "source_quote": "演示永字的笔顺"
}
```

只有字形预览时任务返回 `partial`，不能把静态字体字形称作已完成的笔顺演示。实际数据覆盖率、笔画轮廓、字体与逐笔播放效果尚待真实 Mac 数据和 Bot 屏幕验收。

## 音频、视频与字幕

`media.prepare` 接受已有 `artifact_id`，或 host 用真实搜索/浏览器工具证据生成的 `source_receipt_id`。常规自然语言流程可先由 OpenCode 找到可访问的素材，再提出 `source_url`，host 核对后换成 receipt；模型自己填写一个 receipt ID 不构成来源证据。

媒体准备使用 [FFmpeg 官方命令行](https://ffmpeg.org/ffmpeg.html)和 ffprobe：

- 单段最长 120 秒，输出音频是 16 kHz、单声道 PCM WAV。
- 图片/视频帧为不超过 480×272 的 JPEG，每帧最多 65,536 字节。
- 视频准备 1–10 fps 的原始 MJPEG 和 `frames.json`，索引记录偏移、大小、时间和对应音频采样位置。
- 用户提供的有界字幕可烧录到 JPEG 帧，并生成 WebVTT；程序没有把字幕合成能力伪装成新 ASR 模型。
- 同时保留 poster 和可访问的原始媒体 artifact，便于静态降级和 Mac 查看。

当前适配器需要可用的媒体文件/直链；它不承诺把任意视频网站页面解析成可播放视频，也不处理 DRM 或网站播放器的专有授权协议。准备出的 MJPEG 是给 Bot/帧序播放器使用的有索引素材，不是通用浏览器 MP4 封装。

设备网关首版以有回压的分段 JPEG/音频片段发送：媒体预览上限 12 秒、最多 60 帧，逐段等待设备实际回执。完整文件保留在 Mac。资料时间索引和真实硬件播放延时是两回事，不能宣称已达到连续实时视频帧率或无缝音画同步；这些是后续设备验收指标。

## 打印与补参数

复用 [CUPS `lp`](https://www.cups.org/doc/man-lp.html)、[`lpstat`](https://www.cups.org/doc/man-lpstat.html)与 [IPP Job 状态](https://www.cups.org/doc/spec-ipp.html)。`print.submit` 接受不可变 PDF artifact；也可先从 `visual` 生成真正的 PDF，再提交。打印机采用本次明确选择、可信 host 的默认配置或 Mac 系统默认打印机。

流程是校验 PDF 和参数 → 核对目的打印机 → 持久化提交 fence → 调用 `lp` → 保存实际 job ID → 查询打印队列回执。打印作业标题使用随机 task ID，不把家庭内容写进队列标题。服务重启时核对已有 job ID/标题，不重新调用 `lp` 来“猜测重试”。

状态明确区分排队、挂起、处理中、暂停、已取消、中止、队列报告完成和未知。IPP 不可用时，`lpstat` 只能帮助确认仍在队列；缺少记录不能推断已成功打印。自动核对以 15 秒为周期，最长追踪创建后 7 天；更老的未确定记录保留，仍可手动核对。

缺少内容或默认打印机时返回 `awaiting_input` 和具体 `missing_fields`。`tasks.resume(task_id, input_patch, effect_authorized)` 仅补充该状态的原任务，保留 task/intent ID；进入过提交 fence 的打印不能通过 resume 重交。语音提议中的 `resume_task_id` 也须由 host 核对家庭和任务状态。已经提交的实体打印不会因普通声纹修正或对话切换被重打/撤销；显式取消会调用 CUPS 并等待取消事实。

## 来源撤回、历史关闭与重放

SQLite 同时保存任务输入来源和 `presentation_sources` 映射。来源 ID 可以是版本化的 `source-ref:<ct>:v<revision>`，不把更正前后版本混为一个永久禁用的记忆。任务执行期间新增的检索来源用 `bind_sources` 绑定；冻结后、启动素材前的呈现来源用 `bind_presentation_sources` 绑定。外部引用上限 2048，输入 JSON 和呈现+引用仍有字节预算，超过预算明确失败而不静默丢掉依赖。

来源更正会收回旧输出、停止引用旧来源的可撤销工作，并让旧 plan/asset 不可重放。引用闭包也覆盖已经存在的下游派生物；后续的新来源 revision 可以重新使用。已提交的打印保留真实队列核对。来源遗忘还会等待 producer 退出、擦除相应内容/回执/素材；打印仅保留核对所需的最小外部作业事实，不保留被遗忘的 PDF 或文本。

关闭 history 时，普通前台呈现通过 `schedule_ephemeral` 设短期 TTL，重启后继续清理，防止语音/图片素材成为旁路对话档案。明确用户任务可以保留必要作品；到期清理不会删除仍被明确任务或其他计划引用的素材。过期 presentation 有 fence，慢 TTS/图片不能在清理后重新写回。服务启动还清理崩溃留下的临时渲染文件和没有 manifest 引用的内容文件。

重放使用已有不可变版本和字节，另建 playback 和当前 output lease；不会重新跑 OpenCode，也不会再次执行打印等动作。回执区分准备、实际显示、实际音频进度、完成与失败。失效 lease 下的晚到真实回执可以作为历史事实保存，但不能推进下一段或重新取得设备输出权限。

## 回到 Mac 后的验收重点

先检查 Python/Pillow、CJK 字体、FFmpeg/ffprobe 和数据路径，再验证一个普通文字卡、单词拼写、分数/数轴和一个有真实数据的汉字。分别验证无数据/缺字、过大媒体、无效域名和断网时的明确失败/降级。

任务侧重点是：唤醒后的新问题不被后台工作阻塞；取消后旧结果不抢屏/发声；App 重启不会重复打印；补打印参数复用同一任务；更正/遗忘一个来源后其派生计划和字节不可再读；history 关闭后的普通前台素材按期清理。

实际打印应在确认打印机、纸张、份数和 PDF 内容后人工发起，核对系统队列与出纸；不要把阅读这份文档、代码合并或静态检查作为真实打印/实机验收通过。
