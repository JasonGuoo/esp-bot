# 在 S31 实板上测试「小呆小呆」及本地应答

**后续声音扩展：** 完整拉取流程还包括 [声音设置、离线指令与 Mac 语音回答](voice-interaction.md)。
该文说明新声音页、实际就绪提示、下一轮起音缓冲和可选 Mac TTS。本文的模型备份与应用构建步骤仍适用。

这份流程面向 **ESP32-S31-Korvo-1 V1.1、16 MB Flash / 16 MB PSRAM、SC101IOT 相机**。
拉取本次代码后，使用项目原有 ESP-IDF v6.1 环境构建即可取得官方 MultiNet7 模型；
不需要自行训练唤醒模型、寻找权重或生成应答音频。104 条应答的预制语音随代码
提供，构建时编入应用；要检查实际应用镜像仍在 8 MiB 应用分区内。这里的「即可测试」是指源码、
配置和资源路径已经提供，**不代表本次版本已经编译、烧录或通过实板声学验收**。
上一次交付中物理扬声器播放仍待验收，本次也要实际听见声音才能记为成功。

本次已完成的离板检查：104 条真实音频全部按 320 样本块解码，长度和索引匹配且
非静音；最长条目与 FFmpeg 逐样本一致；全部字幕字符有现有字形。现有 runtime
主机用例已扩展覆盖唤醒授权、排队前闭麦、尾音期间继续闭麦、表情保持、断线、
过期事件、取消与超时恢复，并使用地址/未定义行为检查运行。另核对了新播放器
以及真实 ESP-SR 2.6.1 MultiNet 接口的主机语法。平台声明替身不替代 IDF 构建、
链接与真实硬件验收。

## 1. 本次用什么识别唤醒词

当前没有经过专门训练的 WakeNet「小呆小呆」模型。本次备用实现使用项目固定的
**ESP-SR 2.6.1 / 中文 MultiNet7 `mn7_cn`**，在设备端持续识别一条完整拼音命令：

```text
xiao dai xiao dai
```

这是一条完整的四音节命令，不是累计两个单独的「小呆」，也不靠声音能量、
人脸或云端文字匹配触发。初始化及待机词表只注册这一条；明确唤醒后的监听会话另启用固定控制短句，回到待机就移除。识别超时、
播放抑制和持续静音会清除部分识别状态。单独「小呆」以及隔很久才重复的两次
「小呆」都是必须实测的负例，不能仅凭这段状态处理就声称不会误唤醒。

实现借鉴小智的连续 MultiNet 自定义唤醒方式，使用官方 ESP-SR 接口。
MultiNet 的 CPU、内存、耗电和误唤醒特性不能视为专用 WakeNet 的等价结果。
默认分数阈值 **0.80** 是初始配置，不表示已测得 80% 的准确率；默认窗口为
**3000 ms**。完整原理和上游依据见[研究记录](research/esp32-s31-standalone-agent-feasibility-2026-11-13.md)。

麦克风仍由原来的 `bot_audio` 单独持有。普通唤醒前音频不保存、不上传。
夜间关屏或关相机不应关闭本地关键词识别。当前播放期及播放尾段抑制识别，
因此本轮验收不包含「一边放音一边用语音打断」；触摸停止仍应工作。

## 2. 拉取代码并准备现有配置

在仓库根目录执行，保留原有本机 SDK、私有配置和构建目录：

```sh
git pull --ff-only
python3 tools/prepare_wake_test.py --dry-run
python3 tools/prepare_wake_test.py
./firmware/idf.sh reconfigure
./firmware/idf.sh build
```

脚本只修改本机 `firmware/sdkconfig` 的以下音频选择，并清除相互冲突的中文、
英文 MultiNet 和 VAD choice；其他编译、相机、屏幕、网络等设置原样保留。
尤其不要用整个 `sdkconfig.defaults` 覆盖已经在实板上工作的私有配置。

```ini
CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK=y
CONFIG_BOT_AUDIO_MULTINET_THRESHOLD_PERCENT=80
CONFIG_BOT_AUDIO_MULTINET_WINDOW_MS=3000
CONFIG_SR_MN_CN_MULTINET7_QUANT=y
# CONFIG_SR_MN_CN_NONE is not set
CONFIG_SR_MN_EN_NONE=y
CONFIG_MODEL_IN_FLASH=y
# CONFIG_MODEL_IN_SDCARD is not set
CONFIG_SR_VADN_WEBRTC=y
CONFIG_BOT_AUDIO_WAKE_MODEL_NAME=""
CONFIG_BOT_AUDIO_MODEL_PARTITION="model"
```

`sdkconfig` 已存在时，脚本先将完整原文件备份到
`firmware/.bringup/wake-config/`，备份文件仅当前用户可读写；配置和备份都可能
包含私有网络信息，不要提交到 Git。不存在时才以当前 `sdkconfig.defaults`
初始化。重复运行且内容相同时不会再次写文件或产生额外备份。脚本本身不联网、
不编译、不打开串口、不烧录。

已有非空 `CONFIG_BOT_AUDIO_WAKE_MODEL_NAME` 时，脚本默认停止且不修改配置：
固件会优先选择满足完整短语校验的专用 WakeNet。若明确要在这次测试改用
MultiNet，可以执行 `python3 tools/prepare_wake_test.py --use-multinet`；
它会先备份，再清空专用条目名称。已有 WakeNet 模型选择和外部模型目录仍被保留，
因此模型包可能大于下面的最小体积。**配置备份不是 Flash 中专用模型的备份**，
烧录前仍须执行第 4 节。若模型分区标签曾被改成其他值，脚本会要求先核对实际
分区表，不擅自套用本页的地址。

独立配置文件也可准备：

```sh
python3 tools/prepare_wake_test.py \
  --sdkconfig /absolute/path/to/sdkconfig.wake \
  --defaults /absolute/path/to/sdkconfig.defaults \
  --backup-dir /absolute/path/to/private-backups
```

随后需让 IDF 实际使用该文件，例如给 `./firmware/idf.sh` 传入
`-D SDKCONFIG=/absolute/path/to/sdkconfig.wake`。本页其余命令默认使用常规
`firmware/sdkconfig` 和 `firmware/build/`。新机器先按
[ESP-IDF 开发环境](esp-idf-development.md)安装对应 S31 工具链；不要为了此次
配置迁移执行 `set-target`、`fullclean` 或整片擦除。

## 3. 确认官方模型已打包

ESP-SR 的 CMake 会读取重新生成的 `sdkconfig`，调用其官方 `movemodel.py`，
生成 `firmware/build/srmodels/srmodels.bin`。首次组件解析可能下载固定版本依赖；
已有完整组件缓存后不需要另外寻找模型。正常打包报告应包含 **`mn7_cn` 和 `fst`**，
不能是 `No speech models loaded`。

如需单独重建模型包，在启用项目 IDF Python 环境后，从仓库根目录调用同一官方脚本：

```sh
python firmware/managed_components/espressif__esp-sr/model/movemodel.py \
  -d1 firmware/sdkconfig \
  -d2 firmware/managed_components/espressif__esp-sr \
  -d3 firmware/build
```

务必先 `reconfigure`，再打包；不要直接把尚未规范化的 defaults 文件传给打包器。
最小中文 MN7 + FST 包约 **2,681,351 字节**，当前 `model` 分区为
**6,291,456 字节（6 MiB）**。保留其他语音模型或外部模型时以本次实际文件大小
为准。Flash 包体积不等于运行时 PSRAM 占用，也不证明它能与摄像头、LVGL、AFE
和 TLS 同时稳定运行。

```sh
python3 - <<'PY'
from pathlib import Path
root = Path("firmware/build/srmodels")
for name in ("mn7_cn", "fst"):
    if not (root / name).is_dir():
        raise SystemExit(f"缺少 {name}；先检查 sdkconfig 并重新构建")
image = root / "srmodels.bin"
size = image.stat().st_size
if not 0 < size <= 0x600000:
    raise SystemExit(f"模型包大小无效或超过 model 分区：{size} 字节")
print(f"模型包：{image}，{size} / {0x600000} 字节")
PY
```

## 4. 首次升级同时写入应用和 model；先备份 model

**旧的 `app-flash` 只更新应用，不会安装 MultiNet 权重。** 仅看编译成功或刷入
新应用，仍可能在启动时得到模型缺失。当前项目分区为：

| 分区 | 起始地址 | 长度 | 本次是否写入 |
| --- | --- | --- | --- |
| NVS | `0x9000` | `0x6000` | 否，保留配网及配对数据 |
| factory 应用 | `0x10000` | `0x800000` | 是，写本次 `esp_bot.bin` |
| model | `0x810000` | `0x600000` | 先完整读出备份，再写本次 `srmodels.bin` |
| storage | `0xe10000` | `0x1f0000` | 否 |

先对照本机 `firmware/partitions.csv`、构建出的 `flasher_args.json` 和当前实板的
分区记录；若地址或布局不同，不要套用下面的固定地址。此流程适用于已完成
bring-up、现有分区与本表一致的板子，不负责给布局未知的新板安装引导程序。
不要使用 `erase-flash`、`erase-region` 或整片镜像覆盖。

关闭占用串口的监视器和 Bot Server USB 屏幕预览。在仓库根目录启用同一套 IDF
环境，并把端口改成实际 S31 端口；用户首板记录是 `/dev/cu.usbserial-210`。
已有记录采用 **230400 baud 和 `--no-stub`**，见[首次实板记录](board-bringup-2026-11-06.md)。

```sh
export IDF_TOOLS_PATH="$PWD/firmware/.toolchains/idf-tools"
. firmware/.toolchains/esp-idf/export.sh
```

下面的块在子 shell 内执行：任何读出、检查或写入错误都会停止后续步骤。
它只写 `factory` 和 `model` 两个区段；不更新引导程序、分区表或 NVS。

```sh
(
set -e
umask 077
BOT_SERIAL_PORT=/dev/cu.usbserial-210
BOT_WAKE_BACKUP_DIR="firmware/.bringup/wake-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BOT_WAKE_BACKUP_DIR"

# 先检查将要写入的文件和构建分区映射。
python3 - <<'PY'
import json
from pathlib import Path
build = Path("firmware/build")
files = json.loads((build / "flasher_args.json").read_text())["flash_files"]
mapping = {int(address, 0): path for address, path in files.items()}
for address, name, capacity in (
    (0x10000, "esp_bot.bin", 0x800000),
    (0x810000, "srmodels/srmodels.bin", 0x600000),
):
    if mapping.get(address) != name:
        raise SystemExit(f"构建映射不符：{address:#x}，请核对本机分区")
    size = (build / name).stat().st_size
    if not 0 < size <= capacity:
        raise SystemExit(f"文件大小超出分区：{name}，{size} 字节")
    print(f"将写入 {address:#x}: {name} ({size} 字节)")
PY

# 先读出完整 6 MiB model 分区；这一步成功后才允许写新模型。
python -m esptool --chip esp32s31 --port "$BOT_SERIAL_PORT" \
  --baud 230400 --no-stub read-flash --flash-size 16MB \
  0x810000 0x600000 "$BOT_WAKE_BACKUP_DIR/model-before.bin"

python3 - "$BOT_WAKE_BACKUP_DIR/model-before.bin" <<'PY'
import hashlib
from pathlib import Path
import sys
path = Path(sys.argv[1])
data = path.read_bytes()
if len(data) != 0x600000:
    raise SystemExit("model 备份不完整；停止，不写入")
print(f"model 备份：{path}")
print(f"长度：{len(data)}；SHA256：{hashlib.sha256(data).hexdigest()}")
PY

# 保存将要使用的配置和调试文件，方便将来对照日志。
cp firmware/sdkconfig "$BOT_WAKE_BACKUP_DIR/sdkconfig-after"
cp firmware/build/esp_bot.elf "$BOT_WAKE_BACKUP_DIR/esp_bot.elf"

# 只写应用和模型；esptool 成功校验后再复位进入新应用。
python -m esptool --chip esp32s31 --port "$BOT_SERIAL_PORT" \
  --baud 230400 --no-stub write-flash --flash-size 16MB \
  0x10000 firmware/build/esp_bot.bin \
  0x810000 firmware/build/srmodels/srmodels.bin
)
```

保存终端输出中的备份目录。备份模型、旧配置和本次 ELF 是不同用途：模型备份
恢复 Flash 资源，脚本备份恢复本机配置，ELF 用于解码本次崩溃。若需要退回旧固件，
应使用与旧模型和配置相匹配的旧应用镜像，不能把还依赖 MN7 的新应用与空模型
组合当作正常回退。初次确认模型已正确安装后，后续只改应用的版本才继续使用
常规 `./firmware/idf.sh -p /dev/cu.usbserial-210 -b 230400 app-flash`。

## 5. 启动后先检查这些证据

```sh
./firmware/idf.sh -p /dev/cu.usbserial-210 monitor
```

下面是**源码预期的日志格式，不是本次实板输出**；数值由实际运行填入：

```text
MultiNet7: one complete 'xiao dai xiao dai' command; model=mn7_cn, threshold=0.80, window=3000 ms
MultiNet initialization free heap: internal <before> -> <after>, PSRAM <before> -> <after> bytes
xiaodai: Embedded wake responses ready: 104
xiaodai: Full wake phrase detected: backend=2 score_available=1 score=<score>
wake_response: id=<id> scene=<scene> expression=<expr> gain=<percent> samples=<n> playback=<id>
```

音频状态应报告 `model_name=mn7_cn`，后端为 `MULTINET7`（日志枚举值 `2`），并明确
声学准确率及 CPU/RAM 尚待实测。如果采用专用 WakeNet 优先路径，状态应对应其实际模型，不能
要求它显示 MN7。若只有 `WAKE_MODEL_MISSING` 或初始化错误，先核对新模型是否真的
刷入、模型名称、分区标签及剩余堆空间，不要用调低阈值掩盖加载失败。

依次确认「模型加载 → 完整短语事件 → runtime 接受 → 表情/字幕 → 实际扬声器
出声」。先接好与开发板匹配阻抗的扬声器，并确认用户音量未设为静音；程序不会
绕过静音设置。看到屏幕文字不能证明音频播放成功；听到声音也不能证明识别器只接受
完整短语。记录从四音节结束到动画出现、到第一声应答的时间，并保留启动前后
内部 RAM、PSRAM 及运行中的错误日志。

`local_response_active` 期间不会启动远端会话收音；先播完本地短应答，收到硬件
播放结束通知且尾音、RX/AFE 清理结束后才转入正常监听。上面的 `wake_response` 开始日志不等于硬件已经
播完，需同时观察实际声音和随后恢复的监听状态。Mac 服务在线时也不应抢播、
叠播或把自己的应答重新上传为用户讲话。

## 6. 实板手动场景

先以舒适的小音量，在近处用本人自然声音测试；再换说话人、距离和噪声环境。
下表是待完成的验收项，不是已经取得的测试结果。每项分别记录次数、成功/误唤醒、
距离和环境，不能以一段成功演示代替误唤醒观察。

| 场景 | 应观察到的行为 |
| --- | --- |
| 安静环境，一口气说「小呆小呆」 | 一次唤醒，一次本地应答，表情与字幕匹配；声音完整，无崩溃 |
| 只说一次「小呆」 | 不新建会话、不播放唤醒应答 |
| 说「小呆」，静候至少 2 秒，再说「小呆」 | 不把两次分离呼叫拼成完整唤醒 |
| 相近发音、聊天、电视、人声和拍手 | 不把一般声音、人脸、注视或声源方位作为唤醒依据 |
| 换人、轻声、正常声、不同距离 | 记录命中与漏检；无需先做声纹注册 |
| 关闭 Mac 服务或网络不可用 | 关键词和本地短应答继续工作；不把它写成自由问答或大模型离线可用 |
| 已进入 NIGHT_DARK，摄像头和屏幕不工作 | 仍能用完整短语唤醒；醒来选择安静应答和相应表情 |
| 本机正在播放应答及其尾段 | 不自我唤醒；此期间的声学唤醒受抑制；触摸停止可用 |
| 取消当前会话后再次呼叫 | 旧播放和旧识别状态不串到新一次；本地应答不会叠播 |
| 持续相机预览/人脸检测、屏幕动画及已配对 WSS | 检查音频掉帧、延迟、堆余量和复位，确认并发资源能承受 |
| 不说唤醒词，只改变时间、光照或走入镜头 | 不主动开口；情境只影响已经授权的应答选择 |

本地应答表见 [`shared/wake-replies.json`](../shared/wake-replies.json)，其中每条
包含文字、表情、显示时长和相对音量。测试早晨、白天、晚间、夜间、昏暗、
刚睡醒、重复呼叫、服务离线和无人脸等分支时，应使用实际有效的时间和新鲜
传感器状态；不会因为相机停用就声称「没看到你」，也不在时间未同步时假报
「早上好」。单次选择带有变化，不要求每次呼叫都固定命中同一句。

本轮重点是完整唤醒词、本地预制语音、字幕与表情。已有 Mac OpenCode、
GLM Coding Plan → DeepSeek Flash 顺序继续按原配置工作；这些测试不代表已经
把完整云端 Agent 或开放域 ASR/TTS 迁移到了 S31。

## 7. 上游依据与版本

- [ESP-SR 2.6.1 的固定源码提交](https://github.com/espressif/esp-sr/tree/b6774a31118de04742e51ab57dd68c9001fa1091)：本项目组件固定版本。
- [该版本 Kconfig](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/Kconfig.projbuild)：S31 的中文 MultiNet7、模型位置和 VAD 选择。
- [官方模型打包器](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/model/movemodel.py)及 [CMake 集成](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/CMakeLists.txt)：选取 `mn7_cn` + `fst`，生成 `srmodels.bin`；完整 `flash` 目标包含模型，`app-flash` 不等价。
- [小智连续 MultiNet 唤醒实现](https://github.com/78/xiaozhi-esp32/blob/67ca62d4bb7d91e514d7ccd5e6977745002f41f4/main/audio/wake_words/custom_wake_word.cc)：备用路径的复用依据，不是本项目声学实测。
- [Espressif esptool 基本命令](https://docs.espressif.com/projects/esptool/en/latest/esp32s31/esptool/basic-commands.html)：指定区段读出和写入。
- [本项目开发环境](esp-idf-development.md)和[首次实板记录](board-bringup-2026-11-06.md)：SDK、芯片、串口速率、备份与尚未完成的验收。
