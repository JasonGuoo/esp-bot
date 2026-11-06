# bot_audio：小呆本地音频运行时

本组件面向 **ESP32-S31-Korvo-1 V1.1 / ESP32-S31**。它把官方 BSP、`esp_codec_dev` 和 ESP-SR 接成实际的采集、AFE、WakeNet、VAD 与可取消播放链路。没有自行实现唤醒算法，没有引入第二个麦克风读取者，也没有把 ESP32-S3 的板级配置套到 S31 上。

**交付状态：源码与文档已编写；遵照用户要求，未编译、未运行测试、未刷机，硬件验证尚未进行。** 官方 API 的接线依据和版本见文末。自定义“小呆小呆”声学模型尚未提供，默认报告 `WAKE_MODEL_MISSING`；这不会阻止 codec 和手动会话路径启动。

## 1. 已实现的链路与边界

| 环节 | 实现 |
| --- | --- |
| 板级初始化 | 复用 `bsp_i2c_init()`、`bsp_audio_init()`、`bsp_audio_get_codec_itf()` 与 BSP 引脚定义 |
| codec | 唯一 ES8389 `ESP_CODEC_DEV_TYPE_IN_OUT` 实例，ADC/DAC 共用它 |
| 采集 | 唯一采集任务读取 16 kHz、双时隙 PCM16，取第一个 ADC 时隙作为 mono |
| 本地检测 | 官方 `afe_config_init()` → `create_from_config()` → 独立 feed/fetch；使用官方 WebRTC VAD |
| 唤醒 | 加载配置的精确模型名，并要求模型的完整关键词元数据恰好为 `小呆小呆` |
| 会话隔离 | 四字段 token、内部 permission epoch、RX DMA 丢弃窗口与 AFE/VAD 重置 |
| PCM 输出 | 只有会话及传输权限都开放才调用 `on_pcm`；输出原始第一 ADC 通道，带真实能力标记 |
| 播放 | 有界 PCM 队列、begin/write/end、独立取消锁存、立即关闭功放、静音清理 DMA |
| 健康状态 | 麦克风读取、AFE 结果、播放错误、丢帧与唤醒状态计数及单调时钟 |
| 夜间 | 屏幕熄灭无需停止音频 owner；保持本地检测 |

本组件不包含 ASR、TTS、Opus/MP3 解码、重采样、网络、磁盘录音、声纹身份识别。Mac/harness 输出音频时，应在对端转换为本组件接受的格式。识别人不是 WakeNet 的功能，`voice_identity_available` 保持 false。

### codec 与总线所有权

`bot_audio_create()` 限制板上只有一个音频 owner。主程序/平台可以提前初始化共享 I2C，但不能再调用 `bsp_audio_codec_speaker_init()`、`bsp_audio_codec_microphone_init()`，也不能同时读取或写入同一 I2S。图形与摄像头继续使用各自 BSP 接口。

BSP 的 I2S/data interface 是进程生命周期的静态对象，当前官方 BSP 没有与 `bsp_audio_init()` 配对的公开 deinit。本组件停止时关闭自己创建的 ES8389 与 codec 控制对象，保留 BSP 所有的 I2S/data interface 和共享 I2C；重新启动先在静音状态清除 TX 残留。必须由本组件完成第一次 `bsp_audio_init()`，因为官方 BSP 在已有 TX/RX handle 时直接返回，不会替其他调用者重新配置音频格式。

板级时钟及引脚来自 S31 BSP：MCLK 未连接（`GPIO_NUM_NC`），BCLK/LRCK/DOUT/DIN 使用 BSP 的 3/4/5/6 定义，功放使能使用 BSP 的 GPIO7。代码继承官方 ES8389 的从机、`use_mclk=false` 配置；没有宣称第二时隙一定是第二只麦克风或回声参考。

## 2. 唤醒与降级行为

`BOT_AUDIO_WAKE_READY` 表示精确模型已加载且 AFE 已创建；是否当前有可用的麦克风读取还要看 `wake_available` 和 `BOT_AUDIO_EVENT_HEALTH`。启动初期，在第一次成功读取麦克风之前，health 与 wake_available 均为 false。

| 情况 | 状态与行为 |
| --- | --- |
| 未配置模型名 | `BOT_AUDIO_WAKE_MODEL_MISSING`，继续 codec/manual/VAD 初始化 |
| 分区不存在、空白、目录损坏、精确条目不存在 | `BOT_AUDIO_WAKE_MODEL_MISSING`，不把损坏目录交给官方模型 mmap loader |
| 模型元数据不是一个完整的“小呆小呆” | `BOT_AUDIO_WAKE_MODEL_MISSING`，没有默认模型替代 |
| 模型正确且 AFE 创建成功 | `BOT_AUDIO_WAKE_READY`；健康采集后才 `wake_available=true` |
| AFE 初始化或运行失败 | `BOT_AUDIO_WAKE_UNAVAILABLE`；启动时的 AFE 失败允许保留原始采集与手动交互路径 |
| codec 初始化失败 | `bot_audio_start()` 返回错误；主程序使用触摸/UI 降级路径 |

匹配是模型条目名和完整 UTF-8 关键词的两次精确比较。官方 `esp_srmodel_get_wake_words()` 会用分号拼接多关键词；因此多个关键词、别名或两次“小呆”都不通过。只接受 `WAKENET_DETECTED`、model index 1、word index 1。**VAD 无论多强、多长都不能产生唤醒事件。**

分区目录检查只做边界与基本格式保护，不是模型签名验证；修改元数据不能替代真实训练好的声学权重。模型来源及真实性由模型部署流程负责。

## 3. 后续怎样安装真正的“小呆小呆”模型

以下步骤是硬件与模型到位后的操作说明，**本次没有执行这些命令**。

1. 从 Espressif 的定制唤醒流程获得 **ESP32-S31 / 当前 ESP-SR 兼容**、完整“小呆小呆”关键词的 WakeNet 权重包。保留官方实际给出的模型目录名、`_MODEL_INFO_` 与权重文件。不要使用 ESP32-S3 的导出包代替；外部模型复制路径不会替你做目标架构转换。
2. 把官方已准备好的模型目录放在一个外部模型根目录下。该根目录可以包含子目录；官方 `movemodel.py` 递归查找非空 `_MODEL_INFO_`，以其所在目录的 basename 作为模型条目名。最小部署只放本次需要的定制 WakeNet，避免无关模型占用分区。
3. 在真实 `sdkconfig` 中设置下面四项。路径、模型名必须替换为实际值；示例中的模型名不是随仓库交付的权重。

```ini
CONFIG_MODEL_IN_FLASH=y
CONFIG_ESP_SR_EXTERNAL_MODEL_PATH="/absolute/path/to/vendor-export"
CONFIG_BOT_AUDIO_MODEL_PARTITION="model"
CONFIG_BOT_AUDIO_WAKE_MODEL_NAME="ACTUAL_VENDOR_MODEL_DIRECTORY_NAME"
```

`CONFIG_MODEL_IN_FLASH`、`CONFIG_ESP_SR_EXTERNAL_MODEL_PATH` 是 **ESP-SR 2.6.1 的真实配置项**。`CONFIG_BOT_AUDIO_*` 是本组件配置。官方 WebRTC VAD 的选择为 `CONFIG_SR_VADN_WEBRTC=y`；代码使用 `vad_model_name=NULL`，不要求额外 VadNet 神经模型。不要为了让编译输出出现模型而启用一个通用“小爱同学”等库存唤醒词。本组件不会接受它。

4. 保持项目自定义 partition table 中有名为 **`model`** 的 data 分区。主项目为 16 MB flash 预留 `model,data,spiffs,0x810000,0x600000`，即 6 MiB。这里保存官方 SR 模型二进制，**不把该分区当 SPIFFS 文件系统挂载或格式化**。模型镜像必须适合真实分区容量。正式刷写以项目实际 partition table 为准，不在业务代码硬编码 flash 地址。
5. 后续正常构建时，ESP-SR 的 CMake 会调用官方打包器，把镜像生成到 `build/srmodels/srmodels.bin`。需要单独重新生成时，在 `firmware/` 下使用同一个官方入口：

```sh
python managed_components/espressif__esp-sr/model/movemodel.py \
  -d1 sdkconfig \
  -d2 managed_components/espressif__esp-sr \
  -d3 build
```

脚本会重建 `build/srmodels/`，不要把唯一的原始模型文件放在那里。该命令只是官方打包步骤，并不证明目标硬件可运行模型。

6. 后续刷写官方生成的完整镜像后，重新启动：检查 `WAKE_STATUS` 为 READY、麦克风 health 可用，再验证实际关键词检测。当前 `start()` 重新加载模型；热更新需要先成功 stop，再 start，不支持在 worker 运行时替换 mmap 中的权重。

### 必须注意官方自动刷写行为

ESP-SR 2.6.1 的 CMake 在 `CONFIG_PARTITION_TABLE_CUSTOM` 开启且找到 `model` 分区时，会给 `flash` target 添加 `srmodels.bin`。**完整 `idf.py flash` 会同时写 `model` 分区。** 先手工刷好定制模型、随后用一个没有外部模型配置的完整构建刷机，会覆盖之前的模型；“不选库存唤醒词”不等于保留板上模型。

官方 CMake 查询的分区名固定为 `model`，并不读取本组件的 label 配置。因此当前项目应保持 `CONFIG_BOT_AUDIO_MODEL_PARTITION="model"`。更改 label 必须同时提供对应的模型打包/刷写集成，不能只改 runtime 配置。

另外，官方生成规则的显式 dependency 是 `sdkconfig`；如果只替换外部权重、没有改变配置，旧的 `srmodels.bin` 可能被复用。需要重新运行上面的官方打包器，或删除生成的 `build/srmodels/srmodels.bin` 后再构建。不要删除原始外部模型目录。每次完整刷写前都应使用预期的外部模型路径及生成镜像。

## 4. 会话、隐私与事件契约

启动不会开启对话，也不会授权 PCM 外发。待机麦克风只喂给本地 AFE；不调用网络或文件 API，不输出 AFE 的 `data`、`raw_data` 或 `vad_cache`。本地检测需要短暂 DMA/AFE RAM 缓冲，这不等于保存录音。本组件不把前唤醒缓冲追加给后续对话。

`bot_audio_set_session(audio, token, deliver_pcm)` 有两个独立含义：

- `token!=NULL`：允许这个运行时会话产生本地 VAD 事件。
- `deliver_pcm=true`：另外允许 `on_pcm` 接收会话后采集的数据。只有上层已经确认授权、配对、认证、加密传输后才应设置；当前纯文本 LAN 连接必须保持 false。

任何 token 字段变化、传输权限变化、关闭或重新开启会话都会增加内部 gate epoch。新的采集窗口先丢弃至少官方 I2S DMA 容量加一个采集块的数据，再重置 AFE ring buffer 与 VAD 状态。重置返回值不成功时，检测被关闭，不会假装完成隔离。相同 token/权限重复调用是幂等操作，可用于主程序每 20 ms 的 reconcile。

会话切换还会取消旧 token 的播放。VAD 的 speech 状态按 epoch 重建：如果孩子持续说话且上层更换 turn token，新 epoch 在拿到新的语音结果后重新发 START，然后用该 token 发 END。因此运行时在 WAITING 收到 START 后建立新轮次的模式可以接续。DMA 丢弃和检测重置可能截断轮次开头的少量音频，这是当前显式授权边界的取舍；没有偷偷补发旧 `vad_cache`。

回调可能与撤销权限并发。接收方必须复制原始 token 并在真正发送/消费前再次比较当前 token；不得把旧回调改贴为新 token。`samples`、`event.reason` 只在回调期间有效。回调来自不同音频任务，必须线程安全、短且非阻塞，通过有界队列转交主运行时/传输线程；不能在回调里直接读写网络、文件、调用 start/stop/destroy。

## 5. 播放与取消

输入格式固定为 **16 kHz、mono、signed PCM16 little-endian**。`bot_audio_playback_write()` 每次接受 1..320 个样本，并复制到队列；不保留调用者指针。默认 12 个队列槽，满帧约 240 ms；END 标记也占一个槽。`ESP_ERR_TIMEOUT` 是背压，发送方应在当前 token 仍有效时重试；不能当作成功发送。

操作顺序：`playback_begin(token,&id)` → 多次 `playback_write(id,...)` → `playback_end(id)`。BEGIN 到首块之间保持静音；首块前解除 codec 静音并开启功放，再发 STARTED。正常 END 推入足够零样本，让此前 DMA 中的音频离开队列，随后静音并发 DONE。API 的 DONE 是本地提交/排空完成，不是经过外部麦克风测量的实际声学结束。

`playback_cancel(id)` 或 `playback_cancel(0)` 不经过播放队列，不会因队列已满而丢失取消。它锁存取消并直接关闭功放，worker 随后静音、清理 TX DMA、丢弃队列并报告 CANCELLED。没有关闭 ADC。若 codec 写入/静音/排空失败，会报告 FAILED；排空失败后锁住新播放，必须成功停止并重启 owner 后恢复，避免下一次解除静音播放残留数据。

开始后 2 秒没有新的排队数据会结束为超时失败；已开始播放但暂时缺帧时补零，并增加 underrun 计数。上层应在 begin 前准备好首块，避免把 begin 当成长时间资源预订。

GPIO 关闭是当前固件的及时静音路径，不能据此声称已经测得固定声学停止时延。官方 codec I/O 本身有底层超时；`stop(timeout_ms)` 的 0..10000 ms 参数只限制等待 worker 退出的时间，不涵盖后续 codec 清理的全部硬件耗时。若返回 TIMEOUT，实例、缓冲和模型仍然有效，稍后重试 stop；不能强制 free。

## 6. AEC、打断与时间的真实含义

当前只有第一 ADC lane 的读取与官方 AFE 接线，没有实测的同步扬声器参考、延迟校准或回声抑制效果。因此 AEC、SE、NS、AGC 在此配置中均关闭；`aec_available=false`、`automatic_barge_in=false`、PCM `enhanced=false`、`echo_cancelled=false`。不能把输出 PCM 简单复制到一个“参考通道”就宣称 AEC 可用。

播放中及默认 800 ms 尾音窗口会抑制唤醒和 VAD 事件；本地触摸/停止按钮始终可取消播放。夜间显示状态无需关闭麦克风，但真正进入深睡眠当然不能继续运行软件 WakeNet，上层不能把 UI 睡眠直接等同于芯片深睡眠。

Wake 事件的 `echo_safe=true` 只表示当时未处于自身播放或尾音屏蔽窗口；不保证环境里没有外部扬声器、电视或其他回声。VAD 事件始终 `echo_safe=false`，不得用于自动打断。AFE 没有提供校准后的唤醒置信概率，所以 `confidence_available=false`，不要显示伪造百分比分数。

所有时间来自 `esp_timer_get_time()`：`read_started_us` 是进入 codec read 的时刻，`observed_us` 是读取或事件被观察到的时刻。它们不是 I2S 硬件采样时间，也不是与 Mac 同步的墙上时钟。`hardware_capture_timestamps=false`。序号在一次 start 生命周期内递增，丢弃帧也消耗序号；下游应接受间隔。

## 7. 配置和主程序接线

| 配置 | 默认值 | 含义 |
| --- | --- | --- |
| `CONFIG_BOT_AUDIO_WAKE_MODEL_NAME` | 空字符串 | 必须替换成真实定制模型条目名 |
| `CONFIG_BOT_AUDIO_MODEL_PARTITION` | `model` | 与官方自动打包/flash 路径一致 |
| `CONFIG_BOT_AUDIO_OUTPUT_VOLUME` | 30 | codec 百分比，上限 60；不是声学分贝限值 |
| `CONFIG_BOT_AUDIO_MIC_GAIN_DB` | 24 | 输入增益，范围 0..42 dB |
| `CONFIG_BOT_AUDIO_WAKE_DEBOUNCE_MS` | 2000 | 接受的完整唤醒之间的最小间隔 |
| `CONFIG_BOT_AUDIO_PLAYBACK_TAIL_MS` | 800 | 本机播放后检测抑制窗口 |
| `CONFIG_BOT_AUDIO_PLAYBACK_QUEUE_FRAMES` | 12 | 有界播放队列槽数 |
| `CONFIG_BOT_AUDIO_WORKER_PRIORITY` | 8 | 三个应用音频任务的优先级 |

主程序使用 `bot_audio_default_config()`，设置 `on_event` 与 context 后 create/start。需要启用 PCM 回调时再接入 `on_pcm` 并提供对应的加密传输授权；当前主程序保留 `on_pcm=NULL`、`deliver_pcm=false`。VAD、wake、playback 回调与运行时状态的映射见 [INTEGRATION.md](INTEGRATION.md)。

要求项目配置 `CONFIG_CODEC_ES8389_SUPPORT=y`、`CONFIG_MODEL_IN_FLASH=y`、可供官方 AFE 使用的 PSRAM，以及 ESP32-S31 BSP 所需配置。`CONFIG_BSP_ERROR_CHECK=n` 允许可恢复的 BSP 错误返回上层，而不是在 BSP 宏内 abort。运行时仍必须处理 start 返回错误和健康状态变化。

## 8. 官方来源与版本

组件管理器依赖固定为 BSP **1.1.0**、`esp_codec_dev` **1.5.11**、ESP-SR **2.6.1**。1.5.11 符合 BSP 的 `~1.5` 依赖范围。项目基线为 **ESP-IDF >=6.1 / esp32s31**。仓库中没有复制官方推理二进制、训练权重或自建 DSP 算法。

| 来源 | 本组件复用的内容 |
| --- | --- |
| [ESP32-S31-Korvo-1 官方硬件指南](https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32s31/esp32-s31-korvo-1/user_guide.html) | 板型与硬件接线背景 |
| [esp-bsp `bsp_audio.c`](https://github.com/espressif/esp-bsp/blob/67f41a169401d1f23e827188dd08fb351cb0febe/bsp/esp32_s31_korvo_1/src/bsp_audio.c)、[板级头文件](https://github.com/espressif/esp-bsp/blob/67f41a169401d1f23e827188dd08fb351cb0febe/bsp/esp32_s31_korvo_1/include/bsp/esp32_s31_korvo_1.h)、[manifest](https://github.com/espressif/esp-bsp/blob/67f41a169401d1f23e827188dd08fb351cb0febe/bsp/esp32_s31_korvo_1/idf_component.yml) | 官方 I2S/data interface、I2C/GPIO/ES8389 构造模式；此处合并为唯一 IN_OUT owner |
| [esp-adf codec 1.5.11 manifest](https://github.com/espressif/esp-adf/blob/531054ea3b68c5f4ba1879b9be738ccb79866e99/components/esp_codec_dev/idf_component.yml)、[codec API](https://github.com/espressif/esp-adf/blob/531054ea3b68c5f4ba1879b9be738ccb79866e99/components/esp_codec_dev/include/esp_codec_dev.h)、[ES8389 配置](https://github.com/espressif/esp-adf/blob/531054ea3b68c5f4ba1879b9be738ccb79866e99/components/esp_codec_dev/device/include/es8389_codec.h) | codec 创建、音量/增益、全双工读写、静音/关闭 |
| [ESP-SR S31 AFE 配置](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/include/esp32s31/esp_afe_config.h)、[AFE 接口](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/include/esp32s31/esp_afe_sr_iface.h) | 官方 S31 配置、feed/fetch、WakeNet/VAD 结果及 reset 接口 |
| [ESP-SR 模型 loader](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/src/model_path.c)、[Kconfig](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/Kconfig.projbuild)、[CMake](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/CMakeLists.txt)、[movemodel.py](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/model/movemodel.py) | 精确模型加载、关键词元数据、外部模型打包和 flash 集成 |
| [esp-skainet AFE wake 示例](https://github.com/espressif/esp-skainet/blob/97885d425d951b41a50747e2c33743d0e233a3e3/examples/wake_word_detection/afe/main/main.c)、[VAD 示例](https://github.com/espressif/esp-skainet/blob/97885d425d951b41a50747e2c33743d0e233a3e3/examples/voice_activity_detection/main/main.c) | 独立 feed/fetch 的官方任务模式与检测事件读取 |

上述链接固定到实际阅读的 commit。官方依赖保留各自许可证；本组件的会话授权、token、取消和降级策略属于项目代码。没有引入官方 VAD 示例里的录音保存路径。

### 硬件到位后的待验证事项

主要待验证点是 S31 工具链与固定组件版本的实际兼容性、第一 ADC 时隙是否对应目标麦克风、实际输入增益、无 MCLK 时的 ES8389/I2S 工作、DMA 几何与读取行为、内存与任务负载、真正定制唤醒模型的准确率、播音音量与停止时延。双向同时工作的 codec 能力及共享 I2C 仲裁也需要上板观察。

AEC 和自动打断需要另行测量回声参考通道及同步关系后再实现；这份代码没有将其标记为已完成。当前 30% 默认电气音量不代表儿童耳边的安全声压，必须结合实际扬声器、外壳与距离确定使用音量。以上都是尚未执行的硬件验证项，不是本次验证结果。
