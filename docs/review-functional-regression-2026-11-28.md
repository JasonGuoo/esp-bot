# 2026-11-28 补充功能回归集验收

按 owner 要求，从人实际遇到的情境建立[87 个场景](../tests/regression/SCENARIOS.md)和[统一运行入口](../tests/regression/README.md)。68 个自动场景关联精确实现测试名；17 个真实设备/模型验收或未完成能力，以及 #38 和未安装执行器两项暂缓，独立保留 not_run。本清单不把未实现能力计为通过。

新增 9 个 Python 产品用例（实际 Pillow 媒体渲染、PCM 准入、实际 worker JSONL 进程）与 5 个控制台 presenter 产品用例；复用全部已有 Rust/C 测试。运行器自身 8 个测试覆盖非零退出、零用例、超时与子进程回收、缺工具/SDK、skip、用例改名、TLC 失败和人工验收不能继承 pass。

本次增加测试、编排、npm test 和开发约定，并修复回归发现的数轴边界缺陷：`ticks: 2.5` 原先被 `int()` 静默改成 2，现返回 `number_line_ticks_invalid`。先实际重现 `ValueError not raised`，再修复；触发输入保留在 `test_media_regression.py`。这是媒体参数准入变化，不改变 Bot 状态机/跨端播放时序，沿用原有 16 个孪生及其 linked regressions；没有为测试编排新造 Bot 状态模型。前序集成所改模型的范围仍以[11 月 27 日记录](review-local-integration-2026-11-27.md)为准，不能由此声明整个 RA-01～10 完成。

## 本机运行结果

命令：`./tools/check-functional-regression.sh --report-dir /Users/jason/Library/Caches/esp-bot/regression/20261128-full-03`。

| 检查 | 结果 |
| --- | --- |
| 运行器回归 | 8 passed |
| 固件主机测试 | 17 个 C 程序 passed；Mac Clang、ASan、UBSan、实际项目 IDF/ESP-SR 头文件 |
| bot-server | 213 passed，3 ignored（两个真实端点检查、一个性能基准，报告列名） |
| xiaodai-core | 90 passed，1 ignored 文档示例 |
| 本机 Python | 18 passed，包含真实系统 TTS 合成、Pillow 和 worker 子进程，无 ASR/身份/云模型推理 |
| TypeScript presenter | 5 passed，真实实现由已安装 TypeScript 编译器加载 |
| TLA+ | 16 个 TLC 配置全部完成、无反例；各 checks.sh 通过 |
| 前端构建 | tsc + Vite passed |
| 固件构建 | 本地 ESP-IDF v6.1、ESP32-S31 build passed，未烧录 |
| 场景汇总 | 68 passed、19 not_run；本机门禁退出码 0；九组总耗时约 125 秒 |

私有完整日志与机器报告：[report.md](/Users/jason/Library/Caches/esp-bot/regression/20261128-full-03/report.md)、[report.json](/Users/jason/Library/Caches/esp-bot/regression/20261128-full-03/report.json)。报告记录源 revision、工作区摘要和逐组实际命令。本次源修改未提交，报告摘要对应开始运行的工作区；本验收文档随后新增。未触发 GitHub Actions/CI，未调用真实供应商，未修改家庭配置和数据。

首轮 `20261128-full-01` 保留失败结果：新固件编排遗漏 `bot_voice.c` 链接及官方头文件路径，不能编译关键词适配用例；整套报告正确失败。补齐实际依赖后，17 个 C 程序定向通过，full-02 完整通过；随后增加非整数刻度反例、修复媒体准入，再执行上表 full-03 全套并通过。失败报告没有被覆盖或改成 pass。

## 外部验收边界

未连接 S31，只有系统调试/蓝牙串口；本次没有实板唤醒、AEC、扬声器、屏幕、BLE 或真人听感通过声明。默认半双工、已有音量、配对/provisioning 与模型分区保持原有约定。GUI 操作、真实 ASR/身份、Graphiti、供应商、打印和长时稳定性在 ACC-01～13 独立运行。TTS 候选试听是 ACC-04 的前置实验，格式合法或能生成声音不会自动使该验收通过。

#31 的完整媒体规划/动画/视频路径、#36 的控制 reducer、部分存储隔离、统一 task/identity/memory/device adapter、上下文与全恢复矩阵仍未完成，清单 GAP-01～04 明列，不计入 68 个自动通过。#38 按用户决定继续暂缓。

之后每个新功能/修复：新增或更新实现测试与场景映射，必要时更新孪生，先定向检查，再执行完整入口，并保留新报告。子集运行不会返回完整门禁通过。
