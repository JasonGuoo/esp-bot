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
`firmware/dependencies.lock` 是本地生成文件，不应误认为已在 Git 中固定。

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
