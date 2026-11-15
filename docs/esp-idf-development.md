# 小呆 ESP-IDF 开发环境

当前硬件是 **ESP32-S31-Korvo-1 V1.1**。本机使用官方 ESP-IDF **v6.1**，
提交 `fff9895c82d744c7237be8847347bdd1b07c6643`，S31 命令需要 `--preview`。
不要替换成 ESP32-S3 目标。项目内的本机安装不进入 Git：

```text
firmware/.toolchains/esp-idf/    官方 SDK 与子模块
firmware/.toolchains/idf-tools/ 编译器、调试工具和 Python 虚拟环境
firmware/idf.sh                 为本项目配置环境并调用 idf.py
```

本机环境通过 Espressif 的 `install.sh esp32s31` 安装；Python 3.12 依赖
本机 Python 安装。系统更新或迁移后应重新检查工具链，而不是假设还能运行。
新机器可在上述目录取得同版本官方 SDK（包含子模块），设置 `IDF_TOOLS_PATH`
后运行其 `install.sh esp32s31`。实际依赖版本由组件管理器决定；
`firmware/dependencies.lock` 已取消忽略并纳入本次源码交付，用于固定实际解析的 S31 组件版本；升级依赖时应一并审阅它的差异。

**2026-11-13 唤醒更新：** 拉取后先按 [wake-test.md](wake-test.md) 准备已有
`sdkconfig`，再执行构建；这一次还需要备份并更新 `model` 分区。104 条本地应答
语音已经入库，普通构建不运行 TTS，也不下载语音合成模型。runtime 的本机检查
现在还需链接 `bot_responses.c` 及其嵌入音频符号；使用目标工具链的正式构建会由
组件 CMake 完成这些依赖，不要把平台头文件替身检查当成完整 IDF 编译。

```sh
./firmware/idf.sh --version
./firmware/idf.sh build
# 断开 USB 预览程序后，才可选择单独运行监视器：
./firmware/idf.sh -p /dev/cu.usbserial-210 monitor
```

`idf.sh` 不会自动烧录。手动 `app-flash` 前必须明确核对端口、供电、分区、
镜像和备份；只烧录应用不能代表已经测过实机。监视器、USB 预览和烧录工具
不能同时占用同一个串口。串口日志可能包含敏感信息，先写入受限的
`firmware/.bringup/`，检查后再分享；不要提交私有 `sdkconfig` 或完整镜像。

## 哪些测试需要写入开发板

| 要验证的行为 | 本机入口 | 是否需要烧录 |
| --- | --- | --- |
| 固件触摸唤醒、会话状态、收到 Server 确认后进入思考 | 用 Mac Clang 编译现有的 [runtime 测试](../firmware/components/bot_runtime/tests/test_runtime_near_face.c)、`bot_runtime.c` 和 `attention_manager.c`。测试提供内存总线，使用 IDF 的 POSIX FreeRTOS 头文件和单核设置 | 直接在 Mac 执行 C 代码 |
| 回答后等待下一句的起音判断 | 用 `cc -std=c11` 编译并运行[追问起音用例](../firmware/components/bot_audio/tests/test_followup_vad.c)。覆盖官方 AFE 的语音标记、安静帧、阈值边界和轻声收尾 | 先离板运行，之后用 Mac 朗读、S31 麦克风复核实际起音 |
| Mac 网关处理正确、错误凭据，文字限长和图片分块 | `cargo test --manifest-path server/bot-server/src-tauri/Cargo.toml --lib brain::gateway::tests` | 在 Mac 运行 |
| 正式 App 公布的服务、端口、TLS 身份 | 用 `dns-sd -B _espbot-brain._tcp local.` 浏览，再用 `dns-sd -L` 解析实例。用配对 CA 校验解析到的端口 | 在 Mac 运行 |
| 中文排版、静态图及阅读时长 | 在 Mac 用仓库锁定的 LVGL 版本构建 800×480 显示，渲染正式 face 和字库；用上面的 runtime 测试检查超时后收音停止且结果保留 | 日常修改先做本机验证。实板复核用正式 Mac App 的 USB 帧和旁边 Mac 的系统朗读 |
| 已运行的固件能否发现新端口、断线后重连 | 保持同一个串口监视器打开，只重启或更换 Mac 服务端口。核对认证日志、单调运行时间和 Mac 的已建立连接 | 沿用板上镜像 |
| 新固件是否可编译，实际无线回包、驱动和堆是否正常 | 用 `./firmware/idf.sh build` 增量构建。需要执行新板端代码时才写应用分区，用同一份 ELF 和串口日志检查 | 编译可离板，运行新板端代码需更新应用 |

项目固定的 ESP-IDF v6.1 的 `idf.py qemu` 目标表没有 ESP32-S31。排查 TCP 时先对照
SDK 的 `examples/protocols/sockets/tcp_client` 和同目录树下的
`scripts/run_tcp_server.py`。在本板上重新打开 USB 串口曾触发复位，测试持续运行或
热重连时应保持监视器打开。[实板换端口记录](specs/server-voice-tasks/acceptance.md)

## 分步排查

1. 核对芯片、SDK、实际相机型号和已烧录镜像，保留该镜像对应的 ELF 和配置。
2. 观察上电日志、Wi-Fi 和诊断连接；逐项确认屏幕预览、相机预览与断线重连。
3. 让真实人脸进入画面，分别记录模型输出、被过滤数量和显示框，不把摄像头
   可见等同于检测成功。必要时核对方向、像素格式、光照、置信度和结果时效。
4. 单独命令眼睛左、中、右并观察实际屏幕；最后再启用并验证自动跟随。
5. 复现触摸故障时保留完整 panic、寄存器和匹配的 ELF，用 `idf.py monitor`
   或该工具链的 `riscv32-esp-elf-addr2line` 解码；在定位前不声称根因已修复。

用户要求的是尽量耐受日常断电，正常运行不应写应用分区。当前还是单应用分区，
刷写中断可能导致无法启动，不能声称任意时刻断电都无损。
