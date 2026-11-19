# 小呆中文语音候选：2026-11-19

## 当前实现与结论

正式 App `0.2.2` 的 `models.tts_backend` 未单独配置，按默认 `auto` 运行。机器上没有项目约定的 Kokoro 模型和 Python 路径，正在运行的 TTS worker 因而使用 macOS `say`，默认声音是 `Tingting`。本次未更换正式 App、配置或设备音量。

优先在 Mac 端合成，让 S31 继续通过已认证会话接收和播放 PCM。S31 固件当前最小 app 分区只剩 33,904 字节；虽然当前 ESP-SR 包含 S31 的中文 TTS 库，直接增加本地通用 TTS 需要另外核算镜像、内存、分区和并发。乐鑫 TTS 是拼接式合成，不能仅凭能运行就认为其更自然。参见[乐鑫 TTS 说明](https://docs.espressif.com/projects/esp-sr/en/latest/esp32/speech_synthesis/readme.html)。

本机测试机为 M4 Pro / 48 GB，所有新模型仅在 `firmware/.bringup/tts-compare-20261119/` 隔离评估，没有部署。此目录中的试听文件和测试脚本不随仓库提交；主模型约 1.6 GiB，Kokoro 模型约 419 MiB。

## 候选比较

| 方案 | 实际情况 | 复杂度和风险 |
| --- | --- | --- |
| Kokoro v1.1-zh + sherpa-onnx | 项目已有可选后端；隔离测试通过真正的 JSONL worker，产出 16 kHz 单声道 PCM。默认音色为 `zf_001`。 | 接入复杂度较低；仍需本人试听、实机播放和响应时延验收。现有配置没有暴露其他 Kokoro 音色的选择。 |
| Qwen3-TTS 0.6B CustomVoice 4-bit + MLX | 在本机实际生成了 Serena、Vivian 和 Dylan 中文试听，输出 24 kHz；用项目重采样函数另生成了 16 kHz 样本。 | 音色选择较多；需新增受控后端、模型管理和试听选择。短句出现异常长静音，不能直接设为正式默认。 |
| 设备端 ESP-TTS | 已有 S31 库可供研究。 | 中文拼接式方案需要独立的镜像、内存、分区和声学验证；当前不建议优先改动设备。 |

Kokoro 中文模型卡与 Qwen 0.6B 原版模型卡均标记 Apache-2.0；发布时仍需分别核对具体权重转换、运行库和随附资源的许可与 NOTICE。来源：[Kokoro v1.1-zh](https://huggingface.co/hexgrad/Kokoro-82M-v1.1-zh)、[Qwen3-TTS 0.6B CustomVoice](https://huggingface.co/Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice)、[MLX Audio](https://github.com/Blaizzy/mlx-audio)。本次 Qwen 使用 `mlx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice-4bit`，修订 `08c72cad5e2fd0f41730c8bd1f28149585e46361`，`mlx-audio==0.6.6`。

## 测试结果

同一句「嗨，我在呢。你慢慢说，我会认真听。今天有什么想和我分享的？」：

- 当前 `say`：3 次真实 worker 合成用时分别为 1.56、0.89、0.89 秒，音频约 6.57 秒。
- Kokoro：首次含加载和预热 3.85 秒，热状态 1.34 秒；返回设备所需的 16 kHz PCM。另一次独立 JSONL 进程完成健康检查和两条真实合成，结果均报告 `kokoro_sherpa_onnx` 而非回退。
- Qwen Serena：首次模型加载 3.51 秒，随后首次生成首片段 2.56 秒；同进程再次生成首片段 0.115 秒、完整音频 2.14 秒。热状态短句/长句的 12 次试验首片段 0.09–0.105 秒，但完整音频需要 0.62–2.75 秒，峰值 MLX 分配约 2.27 GB。
- 目前正式 TTS worker 返回完整的 WAV，网关按冻结的语义段和设备回执发送；尚没有把 Qwen 这些音频片段流式接入现有协议。模型的首片段时间不是 S31 首次出声延迟。两个 TTS 独立进程可能分别加载模型，内存和并发影响还需要集成测试。
- Qwen 随机采样的 12 次测试中，短句「嗯，我在」有一次生成 10.4 秒，包含两段约 2.3 和 2.7 秒的静音。一次 `temperature=0` 尝试生成 40.96 秒音频，其中约 38.35 秒低于 `-38 dB`；该轮达到所设输出长度上限附近，不能单靠改为确定性采样解决。继续的测试主动终止了这个隔离进程。
- 使用本机 Qwen ASR 对回放文件做了文字冒烟检查：三个 Qwen 试听和 Tingting 被转写为预期文本，Kokoro 样本的开头被转写错。ASR 结果不能证明发音有错，也不能替代真人听感或小呆扬声器实听。

建议先让用户比较试听。如果现有 Kokoro 音色满意，它是改动和时延更小的方案；如偏好 Qwen，需要先增加预热、分段和异常静音/最长时长保护，并在真实设备测中断、播放回执及最终听感，再决定是否切换正式 App。无论使用哪一项，都不应自动改变设备现有音量。
