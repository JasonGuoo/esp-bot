# ESP32-S31-Korvo-1 V1.1 原厂打印模型

来源：Espressif 官方 [3D Printed Case](https://github.com/espressif/esp-dev-kits/tree/bd4d4ac18657e73583af3d7c0b0a23c3cddf3735/examples/esp32-s31-korvo/3d-printed-case)，2026-06-03 版本；对应硬件 **ESP32-S31-Korvo-1 V1.1**，**不是 S3-Korvo**。

本目录收录乐鑫原文件，已成功复制的 6 个二进制 STL / 文本 STEP **Git blob SHA 与官方逐个一致**。因 GitHub 连接器对 `>1 MiB` 文件返回空内容，**TOP 顶壳的 STL (2.15 MB) 和 STEP (3.92 MB) 尚不能通过本次连接器完成入库**；绝不使用伪造、截断或无效模型替代。它们的原厂下载地址、完整文件大小、Git blob SHA 已保存在 [manifest.json](manifest.json)。

**一键补齐（在联网 Mac/Linux 上的仓库根目录执行）：**

```bash
python3 tools/fetch_s31_korvo_case.py
```

脚本使用锁定的官方 commit，下载缺失的 TOP 两个文件，按字节长度与 Git blob SHA-1 严格校验后才保存到 `original/`，并保留其它原始文件。也可直接点击：[TOP STL 原始下载](https://raw.githubusercontent.com/espressif/esp-dev-kits/bd4d4ac18657e73583af3d7c0b0a23c3cddf3735/examples/esp32-s31-korvo/3d-printed-case/ESP32-S31-Korvo-1_Case_top_20260603.STL) / [TOP STEP 原始下载](https://raw.githubusercontent.com/espressif/esp-dev-kits/bd4d4ac18657e73583af3d7c0b0a23c3cddf3735/examples/esp32-s31-korvo/3d-printed-case/ESP32-S31-Korvo-1_Case_top_20260603.STEP)。

可打印（STL）：top（顶盖，待补齐）、btm（底盖）、stand（静态支架）、light_guide（导光件）。STEP 用于后续修改转轴适配片与底座外罩，不应改变主板定位特征。

## 原始版权和安全说明

原模型版权及归属：Espressif Systems；原仓库包含 [LICENSE-APACHE](https://github.com/espressif/esp-dev-kits/blob/master/LICENSE-APACHE) 和 [LICENSE-CC-BY-SA](https://github.com/espressif/esp-dev-kits/blob/master/LICENSE-CC-BY-SA)。具体 CAD 内容的授权范围需在商业分发和改型之前确认；保留原作者署名与来源链接。此目录不声称新设计产权。

仅 3D 模型文件真实复制/校验；**没有实际打印、装配尺寸、主板卡扣、喇叭孔或云台承重验证**。现成官方静态支架不代表兼容舵机云台。采购和修改顺序见 [BOM](../../../docs/print-files-and-bom-v2.md)。
