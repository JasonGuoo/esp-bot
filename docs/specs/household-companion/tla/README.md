# 分段播报、停止与追问的本机验证

[TurnPlayback.tla](TurnPlayback.tla) 描述一次已获授权的回答如何经过屏幕、扬声器和最终呈现回执，进入下一次可追问状态。改变分段、取消或回执语义时，可以在 Mac 上重跑模型和对应的 C、Rust 测试。

## 模型范围

- 每轮有一到两段，每段可仅显示，也可显示后播报。设备收齐本段所需的实际回执，Mac 才能推进下一段。模型把末段标记和实时内容的最终封口合并为一次完成通知。
- 音频硬件完成事件与最终呈现完成命令进入两个独立的 FIFO。每个通知可重复一次，两队列可按任意相对顺序消费。停止、触屏补话和新唤醒都可与收尾交错，撤销或换轮时保留队列中的旧通知。模型以轮次代表完整的新令牌，并在不同轮次重用相同的播放编号。
- 检查五条性质。进入追问要有当前回答的全部完成证据。停止释放播放权。运行时仍登记着播放时，只有处理对应硬件完成才能进入追问。过期或不匹配的回执不能改变运行时的播放状态。在健康的呈现端和队列持续获得处理机会时，每个未被打断的回答最终进入追问。
- 起点是已接纳的呈现。正式固件通过多条任务和驱动路径产生回执。模型将画面刷新合为一个事件，并从运行时已收到播放开始事件起检查收尾交错。故障恢复、断网、队列溢出、声学效果、实际耗时及硬件驱动需分别验证。

| 模型动作 | 对应实现与主机检查 |
| --- | --- |
| `Display`、`HardwareDone`、`ReportSegment` | [设备回执收集与最终封口](../../../../firmware/components/brain_link/brain_secure.c)。模型将外设报告当作输入。 |
| `TakeAudio`、`TakeEnd`、`Stop`、`NewTurn` | [设备状态机](../../../../firmware/components/bot_runtime/bot_runtime.c)。[C 主机测试](../../../../firmware/components/bot_runtime/tests/test_runtime_near_face.c) 用内存总线驱动真正的 `bot_runtime_step`，包括停止锁存、补话、旧片段和重复回执。 |
| `NextSegment` | [Mac 网关及回归用例](../../../../server/bot-server/src-tauri/src/brain/gateway/segments.rs) 调用 `Delivery::next/report`，覆盖同一计划重播时旧令牌、旧输出许可及上一段的重复回执。 |

## 从仓库根目录运行

```sh
./tools/check-turn-playback.sh
```

脚本依次检查 TLA+ 模型、真实固件 C 状态机和 Rust 网关。首次运行下载固定版本的 TLA+ Tools 1.7.4，每次使用前校验 SHA-256。JAR 默认缓存在 `~/Library/Caches/esp-bot/tlaplus/1.7.4/`，设置了 `XDG_CACHE_HOME` 时使用该缓存根目录。已准备好工具和 Cargo 依赖的 Mac 可以离线复跑。[运行脚本](../../../../tools/check-turn-playback.sh) · [官方发行包](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)

本机使用 OpenJDK 17、Apple Command Line Tools、仓库对应的 ESP-IDF 和已生成的 `firmware/build/config/sdkconfig.h`。Rust 测试使用锁定的依赖、临时数据库及本地测试数据。新 Mac 可按项目说明准备开发环境。[ESP-IDF 本机配置](../../../esp-idf-development.md) · [Mac 服务端开发环境](../../../mac-bot-server.md)

## 2026-11-20 本机结果

在源码基线 `4ae184b` 加本次模型与回归用例上，默认两轮、每轮最多两段共有 **2,660 个不同可达状态**。在临时配置将 `MaxTurns` 提到 3 后共有 **36,530 个**。两次搜索队列均清空，五条性质均通过。C 状态机测试通过，包含停止或补话与完成命令先到或后到的四组组合。Rust 网关 9 个用例通过。

另外在临时副本中给模型与 C 实现分别引入相同的三种回归，验证检查能发现错误。

| 临时改动 | TLC 反例 | C 主机断言 |
| --- | --- | --- |
| 收到最终呈现完成就直接准备追问 | 已收到呈现完成，运行时仍登记着播放，硬件完成事件还在队列 | `test_managed_playback_does_not_arm_followup` 检出追问过早 |
| 硬件完成只匹配播放编号，忽略轮次令牌 | 补话后新回答重用相同编号，旧完成事件清除了新播放状态 | `test_retired_presentation_receipts` 检出新播放被结束 |
| 硬件完成尚未处理时丢弃最终呈现完成命令 | 两队列排空后，未打断的回答一直留在等待状态 | `test_managed_playback_does_not_arm_followup` 检出追问未就绪 |

以上是有限协议模型、固件 C 主机执行和 Mac Rust 网关用例的结果。此次没有连接 S31，实际收音、扬声器输出和屏幕刷新仍以实板验收为准。
