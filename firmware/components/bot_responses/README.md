# 小呆本地唤醒回应

本组件提供 **104 条中文回应、对应的真实离线合成语音、场景选择器与流式解码器**。
固件直接播放随应用嵌入的音频，不等待 Mac、联网、云 TTS 或板上 TTS 模型。
它是内容模块，**没有唤醒、会话、表情或音频设备的控制权**。

源文本和表情建议在 [`shared/wake-replies.json`](../../../shared/wake-replies.json)。
`generated_catalog.inc`、`wake_replies.adpcm` 和 `assets-manifest.json` 由同一个工具生成，必须保持匹配。
语音已经生成并纳入源代码交付；这不代表本次已在真实扬声器上试听、编译或验证固件。

## 职责与调用

只允许在 `bot_runtime` 接受**完整声学唤醒“小呆小呆”**之后选择一句回应。
有脸、注视、声音能量、普通交谈、变暗或时间变化都不能触发它。
如未来用于触摸唤醒，应另建不声称“听见呼唤”的触摸回应集合。

```c
bot_responses_state_t state;
bot_responses_init(&state, esp_random());

/* runtime supplies fresh observations and the state before waking. */
const bot_response_entry_t *entry = bot_responses_select(&state, &context);
if (entry) {
    /* runtime decides whether to display entry->text / entry->expression. */
    bot_response_audio_stream_t stream;
    if (bot_responses_audio_open(&stream, entry)) {
        int16_t frame[320];
        size_t count;
        while ((count = bot_responses_audio_read(&stream, frame, 320)) != 0) {
            /* Playback worker applies relative gain, checks the original token,
             * handles bot_audio backpressure and permits cancellation. */
        }
        /* stream.failed distinguishes malformed audio from a normal end. */
    }
}
```

此代码片段仅展示数据接口。实际播放必须由已有 `bot_audio` 单一输出通路完成，
不能在 runtime 控制任务里循环等待音频、打开第二个 I2S、直接操作 LVGL，
也不能把本地应答音频当成用户输入送去 ASR。

### 选择规则

| 场景 | 数量 | 资格 |
|---|---:|---|
| 通用回应 | 16 | 只依赖已经接受的声学唤醒 |
| 早晨 | 12 | 有效本地时间 05:00–10:59 |
| 白天 | 12 | 有效本地时间 11:00–16:59 |
| 晚间 | 8 | 有效本地时间 17:00–21:59 |
| 夜间 | 12 | runtime 夜间策略，或有效时间 22:00–04:59 |
| 刚醒 | 12 | 唤醒前确实处于睡眠/困倦状态 |
| 暗光 | 8 | 新鲜环境亮度观测确认光线暗 |
| 聊天服务离线 | 8 | 明确知道 brain 服务不可用；不等于断网 |
| 重复呼唤 | 8 | runtime 判定近期有再次明确唤醒 |
| 未看到人脸 | 8 | 当前可用的视觉观测没有人脸 |

夜间优先，其次为重复呼唤、刚醒、暗光、服务离线、未看到人脸、时段。
夜间和暗光固定使用各自轻声回应池；其他场景有四分之一概率使用中性回应，
避免不停强调“没看见你”或服务未连接。

选择器记住最近 8 条内容并跨场景按文字去重。某个小集合全部用过后先放宽为
不重复上一条；只有集合无其他选项才进一步放宽。随机源由应用提供种子，
选择器本身不调用系统时钟、不访问硬件、不分配内存。

`time_valid=false` 不猜早晚；摄像头未开/夜间噪声不算“没看到人”；
`brain_status_valid=false` 不猜在线状态。没有一句声称因 Wi-Fi 连接就能调用大模型。

### 音量、表情和时效

`gain_percent` 是**当前用户音量的相对比例**，不是设置新的设备音量。
夜间默认 30%，暗光 45%；用户静音始终保持静音。不要在解码和播放两处重复缩放。
这只是数字幅度策略，不能当成经过声压测量的音量安全上限。

表达式只返回现有 Face 的字符串 ID，由 runtime 仲裁后显示，不控制眼睛位置，
不绕过 attention 的注视目标。字幕建议时长由语音实际长度和文案建议合成；
停止、设置页面、新 session/turn 或新的回答可以提前撤销。

## 音频格式与空间

`wake_replies.adpcm` 连续保存标准 **Microsoft IMA ADPCM WAV 单声道 data 块**，
去掉每个 WAV 的 RIFF 头。采样率 16 kHz，每块 256 bytes，可解出 505 个样本：

- 前 2 bytes 为 little-endian 有符号预测值。
- 接下来 1 byte 为 0–88 的步长索引，1 byte 必须为 0。
- 后续 252 bytes 按低半字节、高半字节顺序解码。
- 每条的 `pcm_samples` 是填充前的精确长度，最后一块多余样本不播放。

播放器每次可以读取 320 个 PCM16 样本，匹配现有 20 ms 音频接口。
解码器只使用调用者的小状态结构和输出帧，无整句 PCM 分配，也不需要额外音频库。
CMake 将资源嵌入应用分区；本组件不更改分区表、不覆盖 `model` 或 `storage`。
准确音频字节数、总长度、逐条 SHA-256 和模型来源见 `assets-manifest.json`。

### 本次实际验证范围

在临时目录用主机 `cc` 直接编译本组件的真实 `bot_responses.c` 并链接真实嵌入
音频，按每次 320 样本解码全部 104 条：总计 **2,826,704 个样本（176.669 秒）**，
每条均与索引长度相符、`stream.failed=false`，且非静音。音频包实际大小为
**1,446,400 bytes（1.3794 MiB）**，最长一条 2.684 秒。

最长条目 `offline_05` 的 **42,942 个有效样本**与 FFmpeg 对原始 IMA WAV 的解码
逐样本完全一致。检查中修正了 IMA 增量分项移位产生的整数舍入偏差，改为 WAV
参考实现使用的乘法后统一右移。另直接读取现有 30 px 字体的实际 cmap、glyph
和 bitmap 索引，确认 104 条字幕中的 **145 个不同字符全部有有效字形**。

以上为有针对性的主机检查，不是 ESP-IDF 固件构建、板上运行、实体扬声器试听、
音色主观评价或声学唤醒验收。没有新增 CI 或仓库测试脚手架。

## 语音来源和许可

本次使用 [hexgrad/Kokoro-82M-v1.1-zh](https://huggingface.co/hexgrad/Kokoro-82M-v1.1-zh)
的公开中文音色 **`zf_001`（speaker ID 3）**，通过
[sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) 2.0.1 的 CPU ONNX 推理，
在开发电脑上逐条合成，再用 FFmpeg 重采样和编码。
没有付费 API、云语音请求或特定真人/影视角色的仿声。

- 上游 Kokoro 模型公开许可为 **Apache-2.0**，随包的许可原文保存于
  [`licenses/Kokoro-Apache-2.0.txt`](licenses/Kokoro-Apache-2.0.txt)。
- sherpa-onnx 上游项目为 Apache-2.0；这里固定使用 2.0.1。其 2.0 版本 Kokoro
  前处理使用词典，不要求安装/分发 espeak-ng 运行时。
- 固件只携带由项目原创文案生成的音频，不携带 82M 模型、训练数据、原始录音、
  sherpa-onnx 或 FFmpeg 可执行程序。模型许可与生成语音来源分开记录，不冒称
  合成结果是真人录制。
- `assets-manifest.json` 记录实际模型和声音文件的 SHA-256、音色、引擎版本、
  生成位置、编码格式以及每条音频哈希。公开模型许可并不等于对未来任意下载
  的第三方音色或真人录音作授权，因此替换录音入口要求单独提供 provenance。

来源核查：[官方模型说明](https://huggingface.co/hexgrad/Kokoro-82M-v1.1-zh/blob/main/README.md)、
[sherpa 模型包和音色编号](https://k2-fsa.github.io/sherpa/onnx/tts/all/Chinese-English/kokoro-multi-lang-v1_1.html)。
Piper 的公开 huayan 音色卡将数据许可标成 Unknown，本次没有选用它。

## 重生成与更换音色

这是开发时工具，不在 ESP32 上运行。准备 Python 3.12、FFmpeg，并在独立虚拟环境安装：

```bash
python -m pip install sherpa-onnx==2.0.1 numpy
curl -fL -o kokoro-int8-multi-lang-v1_1.tar.bz2 \
  https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-int8-multi-lang-v1_1.tar.bz2
tar xf kokoro-int8-multi-lang-v1_1.tar.bz2
python firmware/components/bot_responses/tools/generate_assets.py \
  --model-dir /absolute/path/kokoro-int8-multi-lang-v1_1 \
  --work-dir /absolute/path/temporary-wake-wavs \
  --speaker-id 3 --speaker-name zf_001 --threads 2
```

模型包下载完后，合成过程无需联网。工具缓存每条源 WAV，文案、语速、音色、模型
或引擎指纹变化才重新合成；编码和索引始终重新生成。模型与临时 WAV 不提交到仓库。
调整音色时，`--speaker-id` 与 `--speaker-name` 必须对应官方同一版本编号表。

已有授权的人声录音可以用同样的条目 ID 批量替换：

```bash
python firmware/components/bot_responses/tools/generate_assets.py \
  --import-wavs /absolute/path/authorized-recordings \
  --provenance-json /absolute/path/voice-provenance.json \
  --work-dir /absolute/path/temporary-wake-wavs
```

输入文件例如 `general_01.wav`，工具会统一转为 16 kHz mono IMA。
provenance 文件应说明录音来源、使用许可、允许分发范围和音色身份；不要把未知许可
的网络音频或影视台词混进来。这个入口同样要求完整 104 条，缺文件即失败。

## 与官方板端 TTS 的关系

[ESP-SR 中文 TTS](https://docs.espressif.com/projects/esp-sr/en/latest/esp32s31/speech_synthesis/readme.html)
也能在 S31 上将中文转为流式 16 kHz mono PCM，需要相应 `voice_data` 资源和运行时集成。
本组件的即时固定回应使用已合成音频，不隐含 `voice_data` 已烧录或额外 TTS 模型可用。
后续可把官方 TTS 用于动态短句；那是独立能力，不能把文本表本身算成可播放语音。
