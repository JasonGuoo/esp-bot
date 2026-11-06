# 小呆 Mac 本地模型服务

这部分是 Rust/Tauri `bot-server` 管理的独立 Python 进程。它提供 **本地 ASR、声纹特征、人脸特征和 macOS 语音合成**，没有 HTTP 端口，不自行打开麦克风或摄像头。Rust 负责显式交互、会话权限、超时、取消、家庭成员登记及记忆；模型进程只处理已经获准的一次输入。

**交付状态：代码已写入，未安装依赖、下载模型、编译、运行模型或执行测试。** 以下命令供之后在 Mac 上准备环境使用，本次没有执行。硬件仍未到货，不能据此推断板端音视频链路已经打通。

## 1. 复用哪些上游

| 能力 | 实际调用 | 固定来源 | 本项目负责的部分 |
| --- | --- | --- | --- |
| 中文语音识别 | Apple MLX example 的 `mlx_whisper.transcribe()` | `mlx-whisper==0.4.3`；[官方 example](https://github.com/ml-explore/mlx-examples/tree/796f5b53cab69a3d48a44233ce21aae889e94a08/whisper) | WAV 边界、离线目录、中文默认语言、结构化返回 |
| 声纹特征 | SpeechBrain `EncoderClassifier.from_hparams()` / `encode_batch()` | `speechbrain==1.1.2`；[官方模型卡](https://huggingface.co/speechbrain/spkrec-ecapa-voxceleb/tree/0f99f2d0ebe89ac095bcc5903c4dd8f72b367286) | 最短音频、能量/削波筛选、L2 归一化、模型标识 |
| 人脸特征 | OpenCV `FaceDetectorYN`、`FaceRecognizerSF.alignCrop()` / `.feature()` | `opencv-python-headless==4.13.0.92`；[官方 Zoo](https://github.com/opencv/opencv_zoo/tree/48b10fa505aeb51fbd127fc60a5c660040e823b7/models) | 图片边界、单人筛选、清晰度筛选、原图坐标 |
| 本地 TTS | 系统 `/usr/bin/say`，只用已安装声音 | [Apple speech synthesis 示例](https://developer.apple.com/documentation/avfaudio/creating-a-custom-speech-synthesizer)；Mac 本机 `man say` | 文本边界、固定 WAV 格式、子进程取消、返回音频 |

ASR 默认是多语言 Whisper small 的 MLX 转换，权重 revision 为 `45f3915923c7a79a5a5b5a7d909d39aeb0e5630e`。这是 MLX Community 的转换，不称为 Qwen 官方 ASR，也不把模型名字写成已验证的性能结论。模型文件、完整 URL、固定 revision、部分上游公布的 SHA256 和许可说明都在 [models.json](models.json)。其余文件下载后的 SHA256 写入本地安装收据，第一次加载时检查。

没有复制整套模型框架，没有启用远程识别 API，也没有使用人声克隆。OpenCode 的 Qwen/GLM/DeepSeek 路由与这里的本地 ASR 是独立能力。

## 2. Mac 环境准备

使用原生 Apple Silicon 的 Python 3.11 或 3.12，避免在 Rosetta 的 x86_64 Python 中安装 MLX。在仓库的 `server/bot-server/local-models` 目录执行：

```bash
python3.11 -m venv .venv
.venv/bin/python -m pip install -r requirements.txt

XIAODAI_MODEL_DIR="$HOME/Library/Application Support/ai.reasonia.espbot.server/models"
.venv/bin/python provision_models.py --model-dir "$XIAODAI_MODEL_DIR" --only asr speaker face
```

`requirements.txt` 固定三个模型适配库的版本，其他直接依赖给出兼容范围。它不是已解析的完整环境锁；PyTorch 与 torchaudio 需要由安装器解析为匹配版本。没有为用户的 Mac 声称依赖已安装成功。Whisper 的 NumPy 输入绕过文件解码器，因此本适配路径不需要为了输入 WAV 另装 ffmpeg。

模型路径必须与控制台 `brain.json` 的 `models.model_dir` 相同；上例对应当前应用标识的默认 Mac 路径。把 `models.python` 配成这个 venv 中 Python 的**绝对路径**，不能继续指向另一个没有安装依赖的 `python3`。开发时 `models.script` 指向本目录 `worker.py` 的绝对路径；打包时可使用应用已解析的资源路径。

下载器仅在上述显式命令中联网。下载到固定 revision，先写临时文件，检查大小及已有上游 SHA256，再原子替换文件，最后写 `installation.json`。它不运行网络返回的模型代码或音视频样例。已有文件只有与固定摘要或安装收据一致才会复用。更新模型前停止本地模型进程；不要并发运行两个下载器。

模型目录包含公开模型与安装收据，**不包含家庭照片、麦克风录音、姓名或声纹登记库**。声纹/人脸登记数据由 Rust 记忆模块管理。模型下载器会保留来源卡与面部模型许可证。

启动参数供 Rust supervisor 配置；无需另开服务端口：

```bash
.venv/bin/python -u worker.py \
  --model-dir "$XIAODAI_MODEL_DIR" \
  --tts-voice Tingting \
  --tts-rate 180
```

`Tingting` 是默认配置值；是否已经安装以当前 Mac 为准。`voices` 操作查询系统已安装的声音，默认声音缺失时返回 `tts_voice_unavailable`，不会自行下载或暗中改用其他语言。也可在 macOS 系统设置中安装所需普通话声音，再配置其准确名称。

## 3. JSONL 协议

stdin 和 stdout 都是一行一个 UTF-8 JSON 对象，最大 **8 MiB / 行，包含换行**。每个请求恰有一个响应；一次只执行一个推理请求，没有内部无界队列。`id` 为 1–128 个 `A-Za-z0-9_.:-` 字符，由 Rust 分配，不可把原始用户文本作为 ID。外层允许 `id`、`op`、`payload`，不接受路径、URL 或模型下载指令。

```json
{"id":"request-1","op":"health","payload":{}}
```

成功与失败结构：

```json
{"id":"request-1","ok":true,"result":{"protocol_version":1}}
{"id":"request-2","ok":false,"error":{"code":"model_missing","message":"Run the explicit model provisioner first."}}
```

| `op` | `payload` | 主要返回 |
| --- | --- | --- |
| `health` | `{}` | `models.{asr,speaker,face,tts}`、依赖版本、平台、限制；不加载模型 |
| `transcribe` | `wav_base64`，可选 `language`，默认 `zh`；`auto`/`null` 自动识别 | `text`、`language`、`segments`、`model_id`、`quality` |
| `speaker_embed` | `wav_base64` | `embedding`、`dimension:192`、`model_id`、`quality_ok`、`quality` |
| `face_embed` | `image_base64` | `embedding`、`dimension:128`、`model_id`、`quality_ok`、`face_count:1`、`bbox`、`image_size`、`quality` |
| `tts` | `text`，可选 `voice` / `rate`；`voice:null` 使用默认声音 | `wav_base64`、`sample_rate:16000`、`channels:1`、`format:pcm_s16le`、`container:wav`、`duration_ms` |
| `voices` | `{}` | 已安装系统声音及语言；不下载声音 |
| `unload` | `{}` | 释放当前模型引用/MLX cache；下一次按需加载 |

`health.available` 表示平台、包与安装收据满足发起调用的基本条件，**不表示模型实际运行过**。`installed` 只检查文件/收据/大小，第一次加载才核对内容摘要；`integrity_checked` 与 `loaded` 分开报告。TTS 的声音清单在第一次 `voices`/`tts` 时读取，不能仅凭 `tts.available` 推断特定声音已存在。

### 音频

只接受 **16 kHz、单声道、PCM16 little-endian WAV**，不是裸 PCM，也不是 WebM/Opus/MP3。根 JSON 中的 base64 不带 `data:` 前缀。输入上限 30 秒；ASR 最短 250 ms，声纹样本最短 3 秒。声明帧数与实际样本数必须相符，过长文件在推理前拒绝。

输入统一移除 DC 偏移，不做增益归一化，不把削波后的失真“修好”。ASR 使用较宽的静音筛选；声纹使用较严格的能量与削波筛选。`active_energy_fraction` 是 20 ms 窗口的能量比例，**不是 VAD、说话者分离或单人说话证明**。多人重叠讲话、背景电视和成长中的儿童嗓音仍需要实际校准；有疑问应返回未知身份，不据此扩大权限。

ASR 是一次完整话语转写，返回片段时间与 Whisper 原始分数作为诊断，不把分数当成校准后的识别概率。它没有承担唤醒：设备仍只由完整 **小呆小呆** 或明确触摸唤起，环境声音不能因为 ASR 有文字而变成会话。

### 人脸

JPEG/PNG 压缩输入最多 3 MiB，最多 4,194,304 像素，每边不超过 4096。先读图片头检查尺寸，再解码；拒绝动画。按 EXIF 转正后，以最长边 960 的图做 YuNet 检测及 SFace 对齐。`bbox` 回到转正后的原图坐标。

检测不到人脸、多个人脸、部分脸出界、脸太小、关键点无效、过暗/过曝/模糊时均不返回特征。通过筛选后才返回 `quality_ok:true`。当前阈值是初始工程参数，**没有在这个摄像头、这个家庭或儿童人脸上校准**。相片翻拍也可能得到特征；没有活体检测，`liveness_verified:false` 始终如实返回。

模型进程不返回姓名、不推断年龄、性别、情绪或其他个人属性，不调用云视觉。家庭成员与特征的关联由本地登记流程明确建立。一次检测到脸不等于知道是谁，也不意味着可以主动打招呼。

### 声纹与人脸的共同约定

向量只包含有限数，按 L2 归一化。`model_id` 包含固定权重来源和预处理版本；更换权重/预处理必须换 ID、重新登记。`quality_ok` 仅代表本次素材通过上述工程筛选，不是身份置信度、活体证明或用户授权。

Rust 侧用多份明确登记的素材建立档案，实施同模型/同维度检查、相似度阈值和第一/第二候选间距，并处理 `unknown` / `ambiguous`。照片与声纹分别给出证据，不能凭同一时刻拍摄就擅自把一个声音绑定给画面里的某个人。

## 4. 取消、内存与日志

Rust 为每个请求设超时并关联当前 turn。原生模型推理不能被一条排在它后面的 JSON 消息及时抢占，因此中止正在推理的请求通过终止 worker 实现；下次重新启动，模型按需加载。不要在同一个进程上先发送取消，再继续消费旧请求的结果。`unload` 是空闲期释放内存的方法，不是运行中取消。

TTS 输出到权限受限的临时目录，转换完成读回后删除。`say_guard.py` 监视父进程存活管道：Rust 强制结束 worker 时，管道关闭，guard 停止并回收 `say`，避免遗留的语音合成任务继续运行。上限 120 秒音频 / 120 秒合成时间。默认语速 180，允许 100–260；最好由上层把长回复按句分段。文本中的 `[[...]]` 指令括号转换为全角，不作为系统语音控制指令执行。

这里不会把声音直接播放到 Mac 扬声器；TTS 返回 WAV，播放端由 Rust/前端选择。正常完成、超时及可处理的终止会清理临时输出；突然断电等情况仍可能留下私有临时合成文件，不能宣称所有 OS 终止情形都实现了物理擦除。**输入录音和照片从不落盘**，Python 释放引用也不等于安全擦除物理内存。

stdout 保留独立文件描述符用于协议，第三方库的 stdout/stderr 被抑制。受控 stderr 日志只有请求 ID、操作名、错误码、异常类型和耗时，不记录转写、语音文本、图像、特征、凭据或任意第三方异常消息。进程没有联网入口；`HF_HUB_OFFLINE` 等环境变量加上本地路径和 SpeechBrain `FetchConfig(allow_network=False)` 阻止适配器在请求过程中下载模型。

## 5. 常见返回码

| 错误码 | 下一步 |
| --- | --- |
| `model_missing` / `model_bundle_invalid` | 停止 worker，用下载器补齐或重新写入对应模型包 |
| `model_checksum_mismatch` | 检查磁盘或手工修改，重新准备固定模型包 |
| `dependency_unavailable` | 确认 supervisor 使用的是同一个 venv；检查 requirements 与 CPU 架构 |
| `unsupported_platform` | ASR 使用原生 Apple Silicon；TTS 需要 macOS |
| `unsupported_audio` | 在采集层生成 16k 单声道 PCM16 WAV |
| `audio_too_short` / `audio_too_quiet` / `audio_clipped` | 重新录制清楚、单人、适量音量的素材 |
| `no_face` / `multiple_faces` / `face_quality_low` | 重新采集单人、完整、清晰、均匀光照的脸部图片 |
| `tts_voice_unavailable` | 通过 `voices` 选择已安装的声音 |
| `tts_timeout` / `tts_output_too_large` | 将回复拆为更短的语句 |
| `request_too_large` | 请求被拒绝且进程退出；缩小输入并重新启动 |

这轮没有增加测试脚本、测试框架、CI 或示例模型运行。代码与文档完成后，仍需在用户的 Mac 和到货后的真实设备上确认依赖、模型加载、音质、中文识别、身份阈值及取消时序。
