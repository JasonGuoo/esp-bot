# Mac 本地语音与人脸能力

`server/bot-server/local-models` 现提供按需加载的 Python 模型进程，由 Rust/Tauri `bot-server` 通过受限 JSONL 管理。详细安装步骤、操作协议、质量门槛、来源和失败码见 [本地模型服务 README](../server/bot-server/local-models/README.md)。

**2026-11-08 更新：** 已增加可配置的 RMA Qwen3-ASR 0.6B 8-bit 接口，复用 RMA
优化后的常驻 worker、权重校验与预热；Whisper 保留为可选后备。本机合成中文语音已在
两种模型上识别成功。RMA 方案目前依赖这台机器上的 RMA 运行环境和模型路径，移动或删除
它们后需要重新配置；尚未验证真人麦克风和机器人端到端语音。

本次使用同一段约 3 秒的合成中文音频：RMA worker 经小呆适配器首次加载、预热并转写约
1.5 秒，常驻后的再次转写约 0.1 秒，文本正确。这是单个短样本的本机结果，不代表真人
语音准确率或完整对话延迟。控制台的音频输入已进入后续对话阶段；本地 TTS 已生成
16 kHz 单声道 WAV，尚未做扬声器听感验收。应用签名在加载模型后重新校验通过。

## 实现边界

| 能力 | 本轮代码 | 边界 |
| --- | --- | --- |
| 语音识别 | RMA Qwen3-ASR 优化 worker 或 MLX Whisper small，本地 WAV → 文字 | Qwen 自动检测语言；当前为完整话语识别，尚未提供逐帧 ASR |
| 声纹 | SpeechBrain 官方 ECAPA 接口，本地 WAV → 192 维特征 | 登记/匹配/未知判断在 Rust；声纹不是授权凭证 |
| 人脸 | OpenCV 官方 YuNet 检测 + SFace 对齐/128 维特征 | 单人输入，拒绝低质量；没有活体或儿童精度验证 |
| 语音输出 | macOS 已安装声音 → 16 kHz 单声道 PCM16 WAV | 不克隆真人，不下载声音；播放由上层控制 |
| 模型准备 | 固定 revision 的下载器、文件大小/SHA256、安装收据 | 只有显式准备命令联网；请求过程没有隐式下载 |

ASR、说话人特征和人脸特征是不同能力。ESP 侧已有音频处理及唤醒相关组件，但专用唤醒模型尚未提供，也不具备完整中文 ASR。Mac 可以在板端没有转写时对获准的一次录音做本地转写；若板端未来提供可靠转写，上层可以直接提交文字，跳过这一环。

## 会话与个人资料

设备设计上负责完整 **小呆小呆** 的本地唤醒和本地行为；专用唤醒模型仍未交付。模型进程不会因为检测到脸或声音而开启聊天，不打开传感器，不缓存环境音，也不把素材发给云提供商。识别输出不能成为家庭成员授权或监护人权限的依据。

照片/声纹的长期关联需要本地明确登记。向量包含固定 `model_id` 与维度，避免更换模型后混用特征；素材筛选通过只代表 `quality_ok`，不宣称身份可靠。多人、低质量或相似候选应保留未知/歧义状态；音频中是谁与画面里是谁不能自动强绑。

原始输入照片/音频只存在于进程内存。公开模型缓存与家庭记忆库分开。TTS 的合成 WAV 会短暂写入权限受限的临时目录，读回后删除；强制断电等情况下无法承诺物理擦除。日志只包含请求 ID、操作、错误码、异常类型和耗时。

## 接线要求

Rust supervisor 必须限制请求大小、一次执行一个请求、分配 ID、关联当前 turn，并在取消/超时时终止旧 worker，避免迟到结果进入新会话。内置 `say_guard.py` 用存活管道处理父进程死亡，收回语音合成子进程。`health` 只读配置和文件，不预热全部模型。

音频输入为 16 kHz 单声道 PCM16 WAV，最长 30 秒；声纹登记每份至少 3 秒。图像输入为单帧 JPEG/PNG，压缩大小最多 3 MiB。最大 JSON 行为 8 MiB。详细字段与错误处理以 [README 协议表](../server/bot-server/local-models/README.md#3-jsonl-协议) 为准。

旧的 LAN 发现/软配对不是音视频鉴权。本地文件选择或明确的 Mac 录制入口可以调用这些能力；板端私有媒体必须在后续具备实际认证、加密和会话绑定的连接中传输，不能仅凭 mDNS 或设备 UUID 放行。

## 交付状态

最初交付仅为源码；2026-11-08 在本机补做了合成语音的 ASR 和 TTS 测试，具体限制见上方。
没有真人语音准确率、内存峰值、麦克风距离或机器人端到端语音的验收结论。固定模型和版本
来源记录在 [models.json](../server/bot-server/local-models/models.json) 与 [NOTICE](../server/bot-server/local-models/NOTICE.md)。

## Kokoro TTS lane (2026-11-18)

`models.tts_backend` is `auto` (default), `kokoro` or `say`. Auto uses Kokoro
(sherpa-onnx, `kokoro-multi-lang-v1_1`, speaker `zf_001` = sid 3) only when its
model is at `<data>/engines/kokoro-multi-lang-v1_1` (or `models.kokoro_model_dir`)
and the TTS Python (`<data>/engines/kokoro-python/bin/python` or
`models.tts_python`) has `requirements-kokoro.txt`. Otherwise, and after any
Kokoro load or synthesis failure, the worker uses macOS `say`. Kokoro output
(24 kHz) is resampled in numpy to the 16 kHz PCM the device plays.

A seventh worker lane, `tts_first`, serves the listening acknowledgement,
progress lines and each answer's first sentence, so they never queue behind
the rest of an answer's sentences.

Measured on the M4 Pro Mac, warm process, 10 Mandarin sentences (4–20 chars),
time to the whole sentence's audio (sherpa-onnx does not stream within a sentence):

| Engine | Threads | Median | Max | 听到了。 |
|---|---|---|---|---|
| Kokoro fp32 | 8 | 482 ms | 771 ms | 182 ms |
| Kokoro fp32 | 4 (default) | 650–658 ms | 985–1147 ms | 250 ms |
| Kokoro fp32 | 2 | 1058 ms | 1692 ms | 434 ms |
| Kokoro int8 | 4 | 1381 ms | 2207 ms | — |
| say Tingting, rate 180 | — | 707–787 ms | 935 ms | 689–844 ms |

Through `worker.py` JSONL (6 sentences, fp32, 4 threads): Kokoro warm median
477 ms (first call 1176 ms including model load and warm-up) versus the say
path's 909 ms (first call 1325 ms). Kokoro skips the quote characters “”「」 as
out-of-vocabulary; that drops no spoken words. CoreML is not usable: the
CoreML EP rejects Kokoro's dynamic-shape TopK.
