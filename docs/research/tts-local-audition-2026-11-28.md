# 2026-11-28 本地 TTS 试听：温暖女声与小机器人

用户偏好是温暖自然的女声，并希望另有童声或小机器人可比较。本次优先候选为 **Qwen3-TTS CustomVoice / Serena**；对照为 Vivian、现有 Tingting，并生成一个 Vivian 的小机器人风格版本。这里只完成本机试听实验，尚未接入 Bot 的 TTS worker 或更改默认声音，真人听感和 S31 声学仍未验收。

官方预设说明将 Serena 描述为温和女声、Vivian 描述为较明亮的女声，均以中文为母语。CustomVoice 支持语气指令；VoiceDesign 是另一种可描述新音色的模型，本次没有把 CustomVoice 的语气变化称作原生童声。[Qwen 官方模型与音色](https://github.com/QwenLM/Qwen3-TTS)、[MLX 维护者的实现与流式示例](https://github.com/Neywa-Labs/mlx-audio/blob/main/mlx_audio/tts/models/qwen3_tts/README.md)

## 实际样本与环境

- 模型：`mlx-community/Qwen3-TTS-12Hz-1.7B-CustomVoice-8bit`，固定 revision `41d3337e8b7f2843a75841595fc14e4b9a7a4b96`。模型卡标为 Apache-2.0；保留来源卡和下载元数据，并核对两份 safetensors 的 LFS SHA-256。[模型卡](https://huggingface.co/mlx-community/Qwen3-TTS-12Hz-1.7B-CustomVoice-8bit)
- 本机 M4 Pro / 48 GB，独立 `~/Library/Caches/esp-bot/tts-audition/venv`，`mlx-audio==0.6.8`；完整实际依赖固定记录在该目录的 `requirements.lock`，没有升级 Bot Server 已有 Python 环境。
- 语气指令：“自然温暖、轻快亲切的日常聊天语气。”每条设置 seed 7、temperature 0.7、max_tokens 400，流式间隔 0.32 秒。权重加载完成后离线推理。
- 同一文本：①“我在呢。我们慢慢来，我陪你一起想办法。”②“现在是下午三点半，十分钟后提醒你喝水。Apple 是苹果。”这是合成试听文字，没有设置提醒。
- 原始采样率 24 kHz；复用项目已经回归的抗混叠重采样，保存 16 kHz 单声道 PCM16 WAV。检查有界时长、有限样本、非静音和容器格式。

[Serena 试听](/Users/jason/Library/Caches/esp-bot/tts-audition/samples/serena-comparison.wav) · [Vivian 试听](/Users/jason/Library/Caches/esp-bot/tts-audition/samples/vivian-comparison.wav) · [小机器人风格试听](/Users/jason/Library/Caches/esp-bot/tts-audition/samples/little-robot-comparison.wav) · [Tingting 对照](/Users/jason/Library/Caches/esp-bot/tts-audition/samples/tingting-comparison.wav)

小机器人版本采用已有 Vivian 样本，经本机 FFmpeg Rubber Band 轻微提高音高并增加短延迟音色；这是合成后期处理，不是儿童录音或儿童声纹。可重复命令：

```sh
ffmpeg -i vivian-comparison.wav \
  -af 'rubberband=pitch=1.122462:formant=preserved:transients=smooth,aecho=0.9:1.0:6:0.08,alimiter=limit=0.92:level=false' \
  -ar 16000 -ac 1 -c:a pcm_s16le little-robot-comparison.wav
```

## 实测与限制

| 样本 | 第一音频块 | 合成总耗时 | 音频长度 |
| --- | ---: | ---: | ---: |
| Serena 第一条，首次生成 | 2.949 s | 4.226 s | 4.000 s |
| Serena 第二条，已预热 | 0.161 s | 1.759 s | 4.880 s |
| Vivian 第一条，已预热 | 0.153 s | 1.465 s | 4.080 s |
| Vivian 第二条，已预热 | 0.157 s | 2.074 s | 5.920 s |
| Tingting 第一条，完整 WAV 返回 | 1.587 s | 1.587 s | 4.079 s |
| Tingting 第二条，完整 WAV 返回 | 0.892 s | 0.892 s | 6.063 s |

Qwen 模型加载另花 3.608 s；MLX 报告峰值约 3.41 GiB。表中是单模型进程、两条固定文本的小样本，不能推导 p95、并发表现或 Bot 整轮首音。Qwen 流式首块与 Tingting 的完整 WAV 返回不是同一指标。现有 Bot 的逐句 WAV 接口还没有使用这里的 Qwen 流式输出。

再用已安装的本地 MLX Whisper 对四个合并样本实际转写，主要内容、时间、提醒和 Apple/苹果均可还原。Serena 的一处“你”转写成“您”，未将其武断归因于 TTS 或 ASR；繁简体和标点差异保留。这个检查只能帮助发现漏句/不可懂，不能证明自然度或真人喜欢。[实际转写记录](/Users/jason/Library/Caches/esp-bot/tts-audition/samples/asr-check.json)、[生成计时与参数](/Users/jason/Library/Caches/esp-bot/tts-audition/samples/metrics.json)

可重复生成入口保留为 `~/Library/Caches/esp-bot/tts-audition/audition.py`，使用同目录 `venv/bin/python` 执行；权重路径、输入、参数、依赖和结果都保留。生成过程有 300 秒外部期限，超时回收整个所属进程组。本次实际完成约 17.3 秒，PCM 校验通过。

## 后续选择与接入

先试听 Serena 与小机器人版本，确认声线和语速。较简单的另一条路线是复用现有 Kokoro/sherpa-onnx adapter，新增接线较少，但本次没有对它生成试听或比较音质。[官方中文/英语 Kokoro 模型与示例](https://k2-fsa.github.io/sherpa/onnx/tts/pretrained_models/kokoro.html)

若将 Qwen 接入 Bot，要在同一变更补真实 worker 的后端选择、失败回退、PCM/时长边界、取消及迟到结果回归，评估原有 worker/播放孪生，并运行 `./tools/check-functional-regression.sh`。还需实测首句与后台任务争用、服务停止、音量、已有离线唤醒短音与新音色的一致性，以及 S31 实际扬声器听感。当前 ACC-04 保持未完成，不因这些样本产生而计为整机通过。


## 已选择并接入：2026-11-28

Owner 随后明确选择「先使用小机器人风格的语音」。已接入 `qwen3_robot`，使用与
上述试听一致的 Vivian、语气指令和 robot-v1 音效。前文的「尚未接入」是试听阶段
状态；现已通过真实 worker 验收，仍不等于 S31 听感通过。

正式本机环境位于 `/Users/jason/小呆 Server/engines/qwen3-tts-python`；独立固定依赖
记录为 `server/bot-server/local-models/requirements-qwen3.lock`。模型位于
`/Users/jason/小呆 Server/models/qwen3-custom-1.7b-8bit`，权重不在仓库或 App 包内。
每个实际 TTS 进程首次加载核对两份权重 SHA256，保持离线。

真实 JSONL worker 的两段音频分别长 4.086 / 5.926 秒，均为 16k/mono/PCM16，
backend 为 `qwen3_mlx_robot`，没有回退。首次新环境运行第一段完整 WAV 耗时
37.636 秒，第二段 2.114 秒；不再并行构建的复测，新进程第一段 4.290 秒、
第二段 2.124 秒。初次包加载、文件缓存和并行负载影响不能从两个样本中分离，
因此保留两次记录，不把较快一轮当作延迟保证。

- [首次实际 worker 报告](/Users/jason/Library/Caches/esp-bot/robot-acceptance-20261128-01/report.json)
- [复测实际 worker 报告](/Users/jason/Library/Caches/esp-bot/robot-acceptance-20261128-02/report.json)
- [本机完整回归](/Users/jason/Library/Caches/esp-bot/regression/20261128-robot-02/report.md)：70 自动化场景通过，19 外部验收/缺口/暂缓项仍 not_run。

新增 MED-09/10 覆盖 PCM/音效、截断/静音/超预算、缺模型、完整文本回退、首句
独立、真实子进程取消与旧 generation。`LocalSpeech` 孪生穷举 1,274 个状态通过。
所有 17 个孪生和关联实现测试均通过；Rust server 215、core 90、Python 25、
console 6、firmware 17 个主机程序通过，既有显式 ignored 保留。

模型内部流式生成仍在完整句 WAV 检查后交付，未改跨端流式协议、音量、唤醒
阈值或半双工。设备内原有离线唤醒短音仍是原资源；没有板，未替换/烧录它们。


安装版 `/Users/jason/小呆 Server/ESP Bot Server.app` 已更新为本地签名 release，
brain.json 已选择 qwen3_robot。旧 App 和配置保留在
`/Users/jason/小呆 Server/runtime/backups/robot-voice-20261128-104250/`。
未读取/迁移旧 Keychain；API key 仍是既有 App 会话生命周期。

在真实安装版控制台中检查到「小机器人 · Vivian / 环境就绪」，发送勾选朗读的
“现在几点？”，得到本地 typed-query 回答“现在是10点43分。”，完整回合约 4.0 秒；
HTML 音频控件实际进入播放并完成，随后健康查询显示该 Qwen 模型已加载。
这是 App/本机播放路径证据，不是声卡外录、真人音质或 S31 扬声器验收。
服务保持运行，主人音量与 ASR 配置未变。
