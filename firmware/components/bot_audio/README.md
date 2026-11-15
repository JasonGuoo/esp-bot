# bot_audio：小呆本地音频运行时

本组件面向 **ESP32-S31-Korvo-1 V1.1 / ESP32-S31**。它把官方 BSP、`esp_codec_dev` 和 ESP-SR 接成采集、AFE、WakeNet/MultiNet7、VAD 与可取消播放链路。声学推理由官方引擎完成；没有引入第二个麦克风读取者，也没有把 ESP32-S3 的板级配置套到 S31 上。

**首次实板接入（2026-11-06）：用户已明确要求本机构建、测试和烧录；已完成构建和烧录，并观察到麦克风非零采样。** 当时各项验收以 [实板记录](../../../docs/board-bringup-2026-11-06.md) 为准。**2026-11-13 新增默认离线完整“小呆小呆”MultiNet7 路径和独立本地应答播放；本次没有 ESP-IDF 工具链或实板，尚未构建、烧录或声学验收这些新增代码。** 默认使用固定 ESP-SR 2.6.1 自带的 `mn7_cn`，无需另找定制权重；有效的专用 WakeNet 仍优先。旧板需要同时更新生成的 model 镜像，单独 app-flash 不会安装新模型。**11 月 15 日已恢复本机构建与实板验收，后续结果以 [本次验收记录](../../../docs/review-acceptance-2026-11-15.md) 为准；上述“未验证”描述保留其历史日期。**

## 1. 已实现的链路与边界

| 环节 | 实现 |
| --- | --- |
| 板级初始化 | 复用 `bsp_i2c_init()`、BSP 引脚定义与官方板级双麦 TDM 示例；本组件持有 I2S/data interface |
| codec | 唯一 ES8389 `ESP_CODEC_DEV_TYPE_IN_OUT` 实例，ADC/DAC 共用它 |
| 采集 | 唯一任务读取 16 kHz PCM16；四时隙 TDM 中选择 0/2 两路麦克风，第一路作为 mono |
| 本地检测 | 官方 `afe_config_init()` → `create_from_config()`；单个采集/AFE worker 顺序 reset/feed/fetch，使用官方 WebRTC VAD |
| 唤醒 | 优先精确的专用 `小呆小呆` WakeNet；否则 `mn7_cn` 只安装一条完整 `xiao dai xiao dai` 命令并持续本地识别 |
| 会话隔离 | 四字段 token、内部 permission epoch、RX DMA 丢弃窗口与 AFE/VAD 重置 |
| PCM 输出 | 只有会话及传输权限都开放才调用 `on_pcm`；输出原始第一 ADC 通道，带真实能力标记 |
| 播放 | 本地应答/会话播放共享同一个有界 PCM 队列；独立取消锁存、立即关闭功放、静音清理 DMA；BEGIN 至尾音后 RX/AFE 清理完成均阻断识别和上行 |
| 健康状态 | 麦克风读取、AFE 结果、播放错误、丢帧与唤醒状态计数及单调时钟 |
| 夜间 | 屏幕熄灭无需停止音频 owner；保持本地检测 |

本组件不包含开放词汇 ASR、TTS、Opus/MP3 解码、重采样、网络、磁盘录音、声纹身份识别。MultiNet 是有限命令识别器：待机只识别完整唤醒词；明确授权的收听会话可增加下文九条固定控制命令。上层本地应答或 Mac/harness 提供符合格式的 PCM。`voice_identity_available` 保持 false。2026-11-13 新增运行时声音设置、会话内起音缓存、固定命令、试听来源与播放进度；这些新增路径尚无 IDF 构建或实板验收。

### codec 与总线所有权

`bot_audio_create()` 限制板上只有一个音频 owner。主程序/平台可以提前初始化共享 I2C，但不能再调用 `bsp_audio_codec_speaker_init()`、`bsp_audio_codec_microphone_init()`，也不能同时读取或写入同一 I2S。图形与摄像头继续使用各自 BSP 接口。

本组件自行创建并持有 TDM I2S/data interface，停止时关闭 ES8389 并释放这些对象；共享 I2C 仍由 BSP 持有。重新启动先在静音状态清除 TX 残留。其他组件不能初始化或占用同一 I2S。

板级时钟及引脚来自 S31 BSP：MCLK 未连接（`GPIO_NUM_NC`），BCLK/LRCK/DOUT/DIN 使用 BSP 的 3/4/5/6 定义，功放使能使用 BSP 的 GPIO7。代码继承官方 ES8389 的从机、`use_mclk=false` 配置；按乐鑫该板 DOA 示例选择四时隙中的 0 和 2 作为两路麦克风。AFE 只使用第一路，第二路用于声源方向，不宣称是已验证的回声参考。

## 2. 唤醒与降级行为

`BOT_AUDIO_WAKE_READY` 表示一种完整关键词后端及 AFE 已初始化；`wake_backend` 明确区分 `WAKENET` 与 `MULTINET7`，`model_name` 是实际选中的条目。是否有健康麦克风读取还要看 `wake_available` 和 `BOT_AUDIO_EVENT_HEALTH`。初始化成功不等于误唤醒率通过验收。启动初期，在第一次成功读取麦克风之前，health 与 wake_available 均为 false。

| 情况 | 状态与行为 |
| --- | --- |
| 专用模型名、完整关键词元数据均匹配，AFE 创建成功 | `READY / WAKENET`；若仍打包 `mn7_cn`，同一 MN 实例仅在授权会话中用于固定命令，待机不做 MN 推理 |
| 专用模型未配置、缺失或元数据不符，但默认 `mn7_cn` 可加载 | `READY / MULTINET7`，只注册完整四音节命令 |
| 分区不存在、空白、目录损坏或两个可用后端都缺失 | `BOT_AUDIO_WAKE_MODEL_MISSING`；保留 codec/manual/VAD，不把损坏目录交给模型 loader |
| `mn7_cn` 格式、分配或命令注册失败 | `BOT_AUDIO_WAKE_UNAVAILABLE`；保留已创建的 AFE/VAD 与手动会话 |
| AFE 初始化或运行失败 | `BOT_AUDIO_WAKE_UNAVAILABLE`；启动时的 AFE 失败允许保留原始采集与手动交互路径 |
| codec 初始化失败 | `bot_audio_start()` 返回错误；主程序使用触摸/UI 降级路径 |

专用 WakeNet 路径精确比较模型条目名和整个 UTF-8 关键词。官方 `esp_srmodel_get_wake_words()` 用分号拼接多关键词，因此多个关键词或别名都不通过；只接受 `WAKENET_DETECTED`、model index 1、word index 1。

默认 MultiNet 路径复用小智 `CustomWakeWord` 已采用的持续命令检测方式，但只选择 `mn7_cn`，不跨语言回退。初始化会清空内置命令，用官方 `esp_mn_commands_add(1, "xiao dai xiao dai")` 和 `esp_mn_commands_update()` 安装**唯一一条完整命令**。只接受最高候选的 command ID 1 / phrase ID 0，以及有限、达到默认 0.80 的模型分数。没有单独 `xiao dai` 命令，也没有两次短名字计数器。**VAD 无论多强、多长都不能产生唤醒事件。**

检测、原生 3000 ms 超时、新 token/关闭、命令图变化、播放抑制和官方 AFE 的语音→静音边界会清理 MultiNet 状态；同 token 的网络就绪或 WAITING→收听不会重置进行中的识别。全部帧进入有界 staging，保留 VAD 确认前的起音，不再额外用固定 −48 dBFS 截断较轻的语音。接受的完整唤醒默认至少间隔 2000 ms，此唤醒冷却不阻止已授权会话中的固定命令。`data_volume` 仍为官方 pre-AGC 元数据，不把它当另一个识别器。0.80 为未经统计校准的模型分数门限，不能据此承诺零误触发：有限词表模型仍可能把普通声音、单独“小呆”或相似读音误判为完整短语，必须做相应负例验收。

### 明确唤醒后的固定离线命令

只有 `set_session_ex(..., local_commands=true)`、新 token 的 RX/AFE 清理完成、且没有自播屏蔽时，capture owner 才安装并运行包含下列命令的图。退出收听后重新安装单条完整唤醒图。最高候选必须匹配当前逐项验证词表的稳定 command ID 与独立的默认 0.85 命令分数门槛；官方内部 phrase ID 不是词表数组索引。没有开放词汇转写，也不接受任意云端新增命令。

| 说法 | ID | 官方中文命令 API 的拼音 |
| --- | --- | --- |
| 声音大一点 | 10 | `sheng yin da yi dian` |
| 声音小一点 | 11 | `sheng yin xiao yi dian` |
| 安静一点 | 12 | `an jing yi dian` |
| 停止 | 13 | `ting zhi` |
| 晚安 | 14 | `wan an` |
| 换成蓝色 | 15 | `huan cheng lan se` |
| 开心一点 | 16 | `kai xin yi dian` |
| 再说一遍 | 17 | `zai shuo yi bian` |
| 恢复声音 | 18 | `hui fu sheng yin` |

同意图的短说法见 [声音交互约定](../../../docs/voice-interaction.md)；别名不进入被动唤醒图。

命中后先锁闭该 token 的 PCM 回调并清除缓存，再发 `LOCAL_COMMAND`，由 runtime 决定动作。重复 reconcile 同 token 不会解除锁存。识别完成之前已经传给 Mac 的句首无法收回，runtime 必须取消旧轮，防止云端继续处理同一句；不能把这条机制描述成整句从未上传。播放时命令识别也被屏蔽，“停止”不是免唤醒或声学打断词，播放中的立即停止仍使用触摸取消。关闭 MultiNet fallback 可以节省模型资源，同时也会关闭这些本地命令。

分区目录检查只做边界与基本格式保护，不是模型签名验证；修改元数据不能替代真实训练好的声学权重。模型来源及真实性由模型部署流程负责。

## 3. 默认模型打包与可选专用 WakeNet

项目默认配置实际选择 ESP-SR 2.6.1 提供的中文 MultiNet7；不是模型文件占位：

```ini
CONFIG_MODEL_IN_FLASH=y
CONFIG_SR_VADN_WEBRTC=y
CONFIG_SR_MN_CN_MULTINET7_QUANT=y
CONFIG_SR_MN_EN_NONE=y
CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK=y
CONFIG_BOT_AUDIO_MULTINET_THRESHOLD_PERCENT=80
CONFIG_BOT_AUDIO_MULTINET_WINDOW_MS=3000
CONFIG_BOT_AUDIO_MODEL_PARTITION="model"
CONFIG_BOT_AUDIO_WAKE_MODEL_NAME=""
```

空的 `BOT_AUDIO_WAKE_MODEL_NAME` 只表示没有指定专用 WakeNet。官方 `movemodel.py` 会把 `mn7_cn` 和必需的 `fst` 资源打入 `build/srmodels/srmodels.bin`。固定版本中模型文件共 2,671,387 bytes，`fst` 共 9,648 bytes，加上 316 bytes 目录，最小镜像约 **2,681,351 bytes / 2.557 MiB**；现有 6 MiB model 分区可容纳这一组合。增加其它模型后按实际镜像重新核对。`fst` 本来没有 `_MODEL_INFO_`，本组件目录检查允许这一官方资源目录，同时继续检查全部文件的名称与分区边界。

已有 checkout 的 `sdkconfig` 不会自动被 `sdkconfig.defaults` 覆盖。按 [本地唤醒测试准备](../../../docs/wake-test.md) 合并配置并使用项目既有 S31 环境构建。首次安装该默认模型需要更新生成的 model 镜像；只刷应用会继续读旧分区。完整 `idf.py flash` 包含模型且会覆盖之前的 model 内容；已有专用模型时保留其外部模型配置，按生成的分区表刷写，不 erase-flash、不重建 Wi-Fi/NVS 凭据。

MultiNet 的实际运行 RAM、CPU 占用、与视觉并行的余量尚未实测；上述数值是 flash 文件预算，**不是运行内存**。初始化日志打印内存前后读数供上板观察，但不能代替峰值和持续负载测量。官方原生模型使用默认 PSRAM/flash 混合加载。本组件另有一个最多 4096 样本（8 KiB）、16 字节对齐的 PSRAM 重分帧块，使 AFE 和 MultiNet 的不同帧长也能接续；它及时清零、不上传、不作为唤醒前录音。运行日志报告双方真实帧长，不把未实测的 512 假定写死。

### 后续部署专用“小呆小呆” WakeNet

以下步骤是硬件与模型到位后的操作说明，**本次没有执行这些命令**。

1. 从 Espressif 的定制唤醒流程获得 **ESP32-S31 / 当前 ESP-SR 兼容**、完整“小呆小呆”关键词的 WakeNet 权重包。保留官方实际给出的模型目录名、`_MODEL_INFO_` 与权重文件。不要使用 ESP32-S3 的导出包代替；外部模型复制路径不会替你做目标架构转换。
2. 把官方已准备好的模型目录放在一个外部模型根目录下。该根目录可以包含子目录；官方 `movemodel.py` 递归查找非空 `_MODEL_INFO_`，以其所在目录的 basename 作为模型条目名。保留默认 MultiNet 可作缺失时回退；确认专用模型可靠后也可显式关闭 fallback 并选择中文 MultiNet None，以节省 flash。
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

主程序使用 `bot_audio_set_session_ex(audio, token, &options)`。`token!=NULL` 保留当前轮次的播放授权；收听权限由 `deliver_pcm`、`retain_start`、`local_commands` 显式区分。`deliver_pcm` 必须等待同 token 的认证 WSS/音频协议就绪；旧 LAN v1 诊断连接不能授权 PCM。THINKING/REPLY_PRESENT 可以保留 token 并将三项都关闭，随后仍能接收本轮 SESSION 播放。NULL token/options 关闭会话。兼容接口 `set_session` 不自动开启缓存或固定命令。

当前主程序仅在认证加密 WSS 控制连接可用、可以协商本轮媒体时开启 retain；它与实际 PCM transport-ready 分开。Mac 离线时继续 `local_commands=true`，但 retain 为 false，避免有限上传缓存超时打断较长的离线控制短句。此处加密指控制连接，PCM 保留项目既有的媒体传输策略。

新 token 或关闭会话增加 gate epoch。新采集窗口丢弃至少官方 I2S DMA 容量加一个采集块，再重置 AFE/VAD；完成后才发 `CAPTURE_READY.available=true`。它是本地收音条件，不等于网络已就绪。同 token 改网络就绪或 waiting 模式不会重置 RX/AFE，也不会把正在说的首字再次丢掉。完整 token、请求选项不变时调用严格幂等。

运行时在进入 WAITING **之前**预分配下一 turn token，可以提前协商同轮网络；此时设置 `waiting_for_followup=true`、`retain_start=true`、`deliver_pcm=false`。只有官方 AFE 报语音，且授权、RX fence 之后的原始 ADC 帧 RMS 达到 −42 dBFS，才发追问 START。原始能量证据仅保留 320 ms 的时间戳，以对齐 AFE 的 256 ms 起音/延迟及一个输出帧；不依赖 AGC 后的增益或仅凭 VAD 元数据启动追问。播放清理、权限 fence、AFE 恢复与读失败会撤回这条证据，已开始的语音由 VAD 判断结束。START 消费预分配的同一 token，不再分配新 token；随后在传输条件满足时开启 deliver。被动环境声音仍不能开启会话。

`retain_start` 明确授权 RX/AFE fence **之后**的 RAM 缓存。默认按完整采集帧保留最多 800 ms，PCM 上限 25,600 bytes，另有每帧元数据；范围 300..1500 ms，不写盘、不上传未授权音频。初始收听和 WAITING 在尚无 VAD START 时都只保留最新窗口，静默等待网络不会因时间超过窗口而失败。START 后按本地观察时刻及配置的 256 ms VAD onset/delay 余量保护起音；若传输仍未就绪并将覆盖受保护部分，立即清空并锁闭该 token，发 `BUFFER_OVERFLOW`，runtime 取消并提示重说。VAD 不可用或漏检时无法保证首字完整，不能把该 RAM 窗口当作录音或可靠转写缓存。

网络就绪后按原顺序释放缓存，每次 capture 最多回调四个历史帧，然后接当前帧；每块保留原 token、sequence 和 read/observed 时刻。每次回调前重新核对权限。关闭、换 token、retain 撤销、已有发送权限撤销、播放 BEGIN、命令消费、读取失败与停止都会清零；不重贴旧 token，也不输出 ESP-SR 的 `vad_cache`。`buffered_drops` 统计已发现会影响受保护起音的样本丢弃，正常尚未开口的滚动覆盖不计入。

会话切换自动取消旧 SESSION 播放；LOCAL_RESPONSE/PREVIEW 分别由 runtime 的原操作/试听 lease 显式撤销。换 token 总会建立新边界，可能丢弃当时已在 DMA 中的音频；连续追问应使用上述同 token 预备流程，不能靠事后补贴标记绕过边界。

回调可能与撤销权限并发。接收方必须复制原始 token 并在真正发送/消费前再次比较当前 token；不得把旧回调改贴为新 token。`samples`、`event.reason` 只在回调期间有效。回调来自不同音频任务，必须线程安全、短且非阻塞，通过有界队列转交主运行时/传输线程；不能在回调里直接读写网络、文件、调用 start/stop/destroy。

## 5. 播放与取消

输入格式固定为 **16 kHz、mono、signed PCM16 little-endian**。`bot_audio_playback_write()` 每次接受 1..320 个样本，并复制到队列；不保留调用者指针。默认 12 个队列槽，满帧约 240 ms；END 标记也占一个槽。`ESP_ERR_TIMEOUT` 是背压，发送方应在当前 token 仍有效时重试；不能当作成功发送。

操作顺序：`playback_begin(token,&id)` → 多次 `playback_write(id,...)` → `playback_end(id)`。BEGIN 到首块之间保持功放关闭；未静音时首块前开启功放，首块实际 codec write 成功后才发 STARTED。后续仅在真实 PCM write 成功后每约一秒发 `PLAYBACK_PROGRESS`，带原 token/id/source；缺帧补零、DMA 排空、收到网络字节都不会制造进度。正常 END 推入足够零样本，让此前 DMA 中的音频离开队列，随后关闭功放并发 DONE。ES8389 1.5.11 的 codec `set_mute` 实际写 ADC 静音位，因此正常播放结束不调用它，避免关闭麦克风。API 的 DONE 是本地提交/排空完成，不是经过外部麦克风测量的实际声学结束。

本地预置应答使用 `bot_audio_playback_begin_local(audio,&runtime_token,&id)`，然后复用 write/end/cancel。它不要求网络音频 session，也不开放麦克风权限；四字段 token 由 runtime 提供，`epoch/session_id` 必须非零。STARTED/DONE/CANCELLED/FAILED 的 `playback_source` 为 `BOT_AUDIO_PLAYBACK_LOCAL_RESPONSE`；原 begin 的事件为 `BOT_AUDIO_PLAYBACK_SESSION`。所有路径仍只允许一个播放 reservation，竞争的 begin 返回 `ESP_ERR_INVALID_STATE`。runtime 根据 source/token/id 处理真正的音频完成，不能用推测时长切到下一状态；也必须在原本地操作失效时 cancel，不能依赖网络 session 切换代替撤销。

声音设置页的显式试听调用 `bot_audio_playback_begin_preview(audio, preview_epoch, &id)`，source 为 `PREVIEW`、token 为 `{epoch,0,0,0}`。它不授权麦克风，不伪造对话 session；runtime 独立核对试听 lease，关闭设置/停止时取消。三种来源复用同一播放器和屏蔽边界。

`playback_cancel(id)` 或 `playback_cancel(0)` 不经过播放队列，不会因队列已满而丢失取消。它锁存取消并直接关闭功放，worker 随后静音、清理 TX DMA、丢弃队列并报告 CANCELLED。没有关闭 ADC。若 codec 写入/静音/排空失败，会报告 FAILED；排空失败后锁住新播放，必须成功停止并重启 owner 后恢复，避免下一次解除静音播放残留数据。

开始后 2 秒没有新的排队数据会结束为超时失败；已开始播放但暂时缺帧时补零，并增加 underrun 计数。上层应在 begin 前准备好首块，避免把 begin 当成长时间资源预订。

GPIO 关闭是当前固件的及时静音路径，不能据此声称已经测得固定声学停止时延。官方 codec I/O 本身有底层超时；`stop(timeout_ms)` 的 0..10000 ms 参数只限制等待 worker 退出的时间，不涵盖后续 codec 清理的全部硬件耗时。若返回 TIMEOUT，实例、缓冲和模型仍然有效，稍后重试 stop；不能强制 free。

## 6. AEC、打断与时间的真实含义

当前只有第一 ADC lane 的读取与官方 AFE 接线，没有实测的同步扬声器参考、延迟校准或回声抑制效果。AEC、SE、NS 仍关闭；识别分支使用官方 WebRTC AGC（compression 12 dB、target −9 dBFS），没有声学回声参考。该配置的 native feed=160、fetch=512，单 owner 按已接受的采样量积累、非阻塞获取完整输出，重置时一并清理计数，连续 4 秒无输出才判故障。原始第一 ADC 通道的上行不经过这条 AGC；`aec_available=false`、`automatic_barge_in=false`、PCM `enhanced=false`、`echo_cancelled=false`。不能把输出 PCM 简单复制到一个“参考通道”就宣称 AEC 可用。

从 BEGIN 排队阶段开始，播放中及默认 800 ms 尾音窗口会抑制唤醒、VAD、环境声音反应与 `on_pcm`，即使上层已经授权传输。尾音结束后继续丢弃 RX DMA 容量加一个采集块，并由同一采集 owner 清理 AFE/VAD/MultiNet，才重新放行，避免把缓冲中的自播当成新语音。本地触摸/停止按钮始终可取消播放。夜间显示状态无需关闭麦克风，但真正进入深睡眠当然不能继续运行软件识别器，上层不能把 UI 睡眠直接等同于芯片深睡眠。

Wake 事件的 `echo_safe=true` 只表示当时自播屏蔽窗口及其缓冲清理已结束；不保证环境里没有外部扬声器、电视或其他回声。VAD 事件始终 `echo_safe=false`，不得用于自动打断。WakeNet 事件没有可用分数，因此 `confidence_available=false`；MultiNet 事件携带清理模型状态前保存的最高候选 `prob`，并设 `confidence_available=true`。它只是未校准的模型分数，便于调门槛，**不能显示为实测准确率或“正确概率”**。

所有时间来自 `esp_timer_get_time()`：`read_started_us` 是进入 codec read 的时刻，`observed_us` 是读取或事件被观察到的时刻。它们不是 I2S 硬件采样时间，也不是与 Mac 同步的墙上时钟。`hardware_capture_timestamps=false`。序号在一次 start 生命周期内递增，丢弃帧也消耗序号；下游应接受间隔。

### 运行时设置、元数据诊断与有限恢复

`bot_audio_set_controls()` 只复制 `bot_voice_settings_t` 请求并递增 revision，允许 create 后、start 前调用；相同请求严格幂等，即使上一轮失败也不会在主循环里反复尝试。capture owner 串行应用 codec 音量和模型门槛，发 `CONTROLS_APPLIED`，状态同时保留 requested/applied/revision/error。主程序先读取 NVS，再以 `muted ? 0 : volume` 创建 codec 配置、提交完整设置、启动；启动状态清理不会丢掉预提交设置。

音量范围 0..60，mute 独立保存原音量。静音通过 PA GPIO7 和零输出帧实现，**不调用会影响 ES8389 ADC 的 runtime `set_out_mute`**。播放任务每块检查请求/已应用静音状态，capture owner 负责 codec 变化；UI 不接触 codec。唤醒门槛范围 40..99，既更新官方 native threshold，也更新 MN 最高候选门槛；WakeNet 使用官方 AFE `set_wakenet_threshold(afe,1,value)`。`night_gain_percent` 和 `spoken_answers` 只作为完整设置保留，具体夜间 PCM 缩放、Mac 语音选项由上层实现。修改后的成功写入 NVS也由平台负责，音频任务不执行闪存持久化。

状态只记录数字：双麦峰值/均值、capture/read errors、AFE feed 拒绝、MN runs/timeouts/rejections/最后候选分数、缓存水位和受保护丢样、播放队列水位/underruns、有限恢复次数，以及最后 wake/capture-ready/playback begin/首块/完成的时刻。`read_us` 包含阻塞读取；`afe_us` 是 feed/fetch、模型推理及事件处理的墙钟区间；`keyword_us` 是最近一次 native detect 调用；`processing_us` 是 read 完成后的完整本轮处理墙钟区间。各有 max；processing 超过单采集块时长时增加 `capture_overruns`。这些不是独立 CPU 占用、I2S 硬件丢帧量或声学端到端延迟。

连续 20 次 AFE fetch 错误/超时，或官方 feed/reset 错误，会停用本地检测。默认最多自动尝试两次已有 AFE 的 buffer/VAD reset，间隔至少 1.5 s，且避开自播屏蔽窗口；成功后重新建立 RX/AFE 边界。`bot_audio_retry_afe()` 可显式排队一次同类恢复；它不重新加载模型、不重启 I2S、不释放工作中的内存，初始 AFE 根本未创建时拒绝。失败保留 manual/raw 采集路径并明确报告状态。它不是硬件故障自动修复保证。

## 7. 配置和主程序接线

| 配置 | 默认值 | 含义 |
| --- | --- | --- |
| `CONFIG_BOT_AUDIO_WAKE_MODEL_NAME` | 空字符串 | 可选专用 WakeNet 条目名；空时走默认 MultiNet7 |
| `CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK` | y | 完整四音节中文命令作为离线唤醒 |
| `CONFIG_BOT_AUDIO_MULTINET_THRESHOLD_PERCENT` | 80 | 内部模型分数门槛，待实板标定 |
| `CONFIG_BOT_AUDIO_MULTINET_WINDOW_MS` | 3000 | 原生检测窗口；超时清理后继续识别 |
| `CONFIG_BOT_AUDIO_LOCAL_COMMAND_THRESHOLD_PERCENT` | 85 | 授权会话的固定命令门槛，未校准 |
| `CONFIG_BOT_AUDIO_START_BUFFER_MS` | 800 | 显式授权同 token 的 PSRAM 窗口，300..1500 ms |
| `CONFIG_BOT_AUDIO_AFE_RECOVERY_LIMIT` | 2 | 每次启动最多自动重置 AFE 的次数 |
| `CONFIG_BOT_AUDIO_MODEL_PARTITION` | `model` | 与官方自动打包/flash 路径一致 |
| `CONFIG_BOT_AUDIO_OUTPUT_VOLUME` | 30 | codec 百分比，上限 60；不是声学分贝限值 |
| `CONFIG_BOT_AUDIO_MIC_GAIN_DB` | 36 | 双麦输入增益，范围 0..42 dB；沿用官方该板 DOA 示例 |
| `CONFIG_BOT_AUDIO_WAKE_DEBOUNCE_MS` | 2000 | 接受的完整唤醒之间的最小间隔 |
| `CONFIG_BOT_AUDIO_PLAYBACK_TAIL_MS` | 800 | 本机播放后检测抑制窗口 |
| `CONFIG_BOT_AUDIO_PLAYBACK_QUEUE_FRAMES` | 12 | 有界播放队列槽数 |
| `CONFIG_BOT_AUDIO_WORKER_PRIORITY` | 8 | 采集/AFE 与播放两个应用音频任务的优先级 |

主程序使用 `bot_audio_default_config()`，设置 `on_event`、`on_pcm` 与 context 后 create、提交已保存的 controls，再 start。仅在明确会话、本地 capture-ready、设备认证和音频通道就绪时允许 PCM 回调；未经唤醒或触摸时无 session、无 retain。VAD、wake、playback 回调与运行时状态的映射见 [INTEGRATION.md](INTEGRATION.md)，较早的实板验证状态见 [整合记录](../../../docs/server-integration-2026-11-08.md)。

要求项目配置 `CONFIG_CODEC_ES8389_SUPPORT=y`、`CONFIG_MODEL_IN_FLASH=y`、可供官方 AFE 使用的 PSRAM，以及 ESP32-S31 BSP 所需配置。`CONFIG_BSP_ERROR_CHECK=n` 允许可恢复的 BSP 错误返回上层，而不是在 BSP 宏内 abort。运行时仍必须处理 start 返回错误和健康状态变化。

## 8. 官方来源与版本

组件管理器依赖固定为 BSP **1.1.0**、`esp_codec_dev` **1.5.11**、ESP-SR **2.6.1** 和 `esp_apa_doa` **1.0.0**。1.5.11 符合 BSP 的 `~1.5` 依赖范围。项目基线为 **ESP-IDF >=6.1 / esp32s31**。APA 的预编译库由组件管理器获取，使用乐鑫产品专用的 modified MIT 许可，分发时须保留许可。

| 来源 | 本组件复用的内容 |
| --- | --- |
| [ESP32-S31-Korvo-1 官方硬件指南](https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32s31/esp32-s31-korvo-1/user_guide.html) | 板型与硬件接线背景 |
| [esp-bsp `bsp_audio.c`](https://github.com/espressif/esp-bsp/blob/67f41a169401d1f23e827188dd08fb351cb0febe/bsp/esp32_s31_korvo_1/src/bsp_audio.c)、[板级头文件](https://github.com/espressif/esp-bsp/blob/67f41a169401d1f23e827188dd08fb351cb0febe/bsp/esp32_s31_korvo_1/include/bsp/esp32_s31_korvo_1.h)、[manifest](https://github.com/espressif/esp-bsp/blob/67f41a169401d1f23e827188dd08fb351cb0febe/bsp/esp32_s31_korvo_1/idf_component.yml) | 官方 I2S/data interface、I2C/GPIO/ES8389 构造模式；此处合并为唯一 IN_OUT owner |
| [esp-adf codec 1.5.11 manifest](https://github.com/espressif/esp-adf/blob/531054ea3b68c5f4ba1879b9be738ccb79866e99/components/esp_codec_dev/idf_component.yml)、[codec API](https://github.com/espressif/esp-adf/blob/531054ea3b68c5f4ba1879b9be738ccb79866e99/components/esp_codec_dev/include/esp_codec_dev.h)、[ES8389 配置](https://github.com/espressif/esp-adf/blob/531054ea3b68c5f4ba1879b9be738ccb79866e99/components/esp_codec_dev/device/include/es8389_codec.h) | codec 创建、音量/增益、全双工读写、静音/关闭 |
| [ESP-SR S31 AFE 配置](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/include/esp32s31/esp_afe_config.h)、[AFE 接口](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/include/esp32s31/esp_afe_sr_iface.h) | 官方 S31 配置、feed/fetch、WakeNet/VAD 结果及 reset 接口 |
| [ESP-SR 模型 loader](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/src/model_path.c)、[Kconfig](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/Kconfig.projbuild)、[CMake](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/CMakeLists.txt)、[movemodel.py](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/model/movemodel.py) | 精确模型加载、关键词元数据、外部模型打包和 flash 集成 |
| [S31 MultiNet API](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/include/esp32s31/esp_mn_iface.h)、[命令注册实现](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/src/esp_mn_speech_commands.c)、[中文命令样例](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/model/multinet_model/fst/commands_cn.txt)、[模型文件](https://github.com/espressif/esp-sr/tree/b6774a31118de04742e51ab57dd68c9001fa1091/model/multinet_model/mn7_cn) | 官方中文拼音格式、完整命令注册、detect/clean/timeout、真实模型预算 |
| [小智 CustomWakeWord](https://github.com/78/xiaozhi-esp32/blob/67ca62d4bb7d91e514d7ccd5e6977745002f41f4/main/audio/wake_words/custom_wake_word.cc) | 已有项目采用的持续 MultiNet 自定义唤醒模式；本组件不复制录音缓存和上传功能 |
| [esp-skainet AFE wake 示例](https://github.com/espressif/esp-skainet/blob/97885d425d951b41a50747e2c33743d0e233a3e3/examples/wake_word_detection/afe/main/main.c)、[VAD 示例](https://github.com/espressif/esp-skainet/blob/97885d425d951b41a50747e2c33743d0e233a3e3/examples/voice_activity_detection/main/main.c) | 独立 feed/fetch 的官方任务模式与检测事件读取 |

上述链接固定到实际阅读的 commit。官方依赖保留各自许可证；本组件的会话授权、token、取消和降级策略属于项目代码。没有引入官方 VAD 示例里的录音保存路径。

### 尚待完成的声学验证

11 月 13 日源码阶段只完成接口语法与主机运行时检查。11 月 15 日已经在真实 S31 工具链构建并烧录，完成完整唤醒、九类固定命令的多次声学尝试，以及 Mac ASR/GLM/TTS 与硬件播放回执检查；详情和未通过/未测边界见 [当前验收记录](../../../docs/review-acceptance-2026-11-15.md)。这不等于统计召回率、误唤醒率、真实扬声器自播声压或追问首字时延已完成测量。

AEC 和自动打断需要另行测量回声参考通道及同步关系后再实现；这份代码没有将其标记为已完成。当前 30% 默认电气音量不代表儿童耳边的安全声压，必须结合实际扬声器、外壳与距离确定使用音量。这些声学指标仍未测量，不能用 codec 写入成功替代。
