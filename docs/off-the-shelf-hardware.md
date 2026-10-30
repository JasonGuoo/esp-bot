# 小呆：现成外壳、双轴运动和外放音频方案（2026-10-30）

**目标：** 保留已有 **ESP32-S31-Korvo-1 V1.1（4.3 英寸 800×480 屏幕）**，尽量使用官方 STEP/STL、现成云台与电机、内置功放，做出好看的双轴桌面机器人。**状态：采购与装配方案研究，并非实物已安装、CAD 已校核或电机已验证。**

关联：[本地 Mac Brain 架构](local-first-hybrid-brain.md) · [上游复用清单](upstream-reuse-catalog.md) · [硬件/供电限制](hardware-power.md)。

## 1. 最大的发现：乐鑫有精确匹配 S31 Korvo-1 V1.1 的外壳 CAD

- [官方 Korvo-1 V1.1 用户手册（含 3D 外壳链接）](https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32s31/esp32-s31-korvo-1/user_guide.html)
- **[官方 STL + 可编辑 STEP 文件目录](https://github.com/espressif/esp-dev-kits/tree/master/examples/esp32-s31-korvo/3d-printed-case)**
- 文件：ESP32-S31-Korvo-1_Case_top_20260603.STEP / STL、Case_btm、Case_stand、Case_light_guide（每种都有 STEP / STL），均已在 GitHub 目录核对。

这是目前找到的、按**这块板的版本**发布的直接参考外壳。建议头部直接以它为基础打印，优先浅色磨砂外壳与深色屏幕边框；STEP 文件仅用于必要的底座转接片/外观微调。需确认摄像头、双麦克风、Wi-Fi 天线、触控边缘、USB/开关/散热都不被遮挡。

**不能假设官方原装底座能直接装在市售双轴云台上**，也不能假设它预留了两只扬声器的腔体或具备承受频繁转头负载的刚度。后续只能承诺尽量减少二次设计，不能声称零适配装配成功。

## 2. 现成云台和机械组件：实际可买的候选

| 方案 | 已核对的产品事实 | 对小呆的价值与限制 |
| --- | --- | --- |
| **Hiwonder 双自由度全金属 Pan-Tilt** | [厂家 SKU 21070016](https://www.hiwonder.com/collections/all-product/products/pan-tilt)：两只 LD-1501MG 数字金属齿轮舵机、金属 U 型架、滚珠轴承、转盘与安装螺丝；厂家标称整台约 **550 g**、转盘直径 **120 mm**、台高 **120 mm**、轴转角约 180 度、舵机 6–8.4 V、7.4 V 时 **17 kg·cm** 标称扭矩；当时标价 **US$83.99**。 | **首选强壮、完整的运动模块**。适合未来支撑较重的 4.3 英寸头，但必须实测头重量、重心、安装孔距和运行电流。裸露金属不美观：外面包圆润底座罩，不重新开发传动齿轮。 |
| **DFRobot FIT0004 黑色金属双轴支架** | [官方](https://www.dfrobot.com/product-340.html)：支架和螺丝，**不含舵机**，兼容部分标准尺寸舵机（如 Hitec HS422、DFRobot 15kg 标准舵机）。当时厂家标价 **US$6.50**。 | **低价紧凑的替代方案**。须另买两只合适的舵机、舵盘和电源，真实负载与安装刚度尚未确定。 |
| **M5Stack StackChan Body** | [官方](https://docs.m5stack.com/en/base/StackChan_Body)：完整可爱的外壳、双轴结构、内置电池；原装机器人头部为 2.0 英寸 CoreS3。 | **外观/机械风格参考，不是可直接安装 S31 Korvo 4.3 英寸主板的外壳**。不要误购作兼容机壳。 |
| **Adafruit 微型 9g 双轴云台** | [官方装配](https://learn.adafruit.com/mini-pan-tilt-kit-assembly)：小相机/微型板用 SG90 类机构。 | 对带 4.3 英寸屏幕、双板和外壳的整个头部**不建议作为首选**，应先测量负载。 |

**有真实成品装配实例的参考项目：** [cgro00/desk-robot](https://github.com/cgro00/desk-robot)——V1 使用现成双轴支架，V2 使用可打印颈部和头壳，带摄像头、外放扬声器和电脑上的语音 Brain。其硬件是 XIAO ESP32-S3、2.42 英寸 OLED，**不是 Korvo 尺寸兼容部件**；只借鉴装配、电源和走线逻辑。另有 [stack-chan/stack-chan 开源外壳与装配](https://github.com/stack-chan/stack-chan)，同样只作为工业设计和机构参考，所有 CAD 必须逐一核对许可证。

## 3. 外放声音：S31 板上已有两套功放，直接接两只喇叭

[乐鑫官方 Korvo-1 V1.1 说明](https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32s31/esp32-s31-korvo-1/user_guide.html)：音频芯片为 **ES8389**，有**两个 NS4150B 3W Class-D 功放**，左右各有一个 **4Ω / 3W、2.00 mm 针距**输出接口。所以只需要适当的**无源 4Ω / 3W 喇叭**，**不要再在功放输出端串接 MAX98357A 或 PAM8403 功放**。

可采购例子：[Little Bird 2 × 45mm 4Ω 3W、PH2.0 线头扬声器](https://littlebirdelectronics.com.au/products/2-x-45mm-4ohm-4-3w-stereo-speaker-with-ph2-0-plug)，查阅时 AU$6.20。**2.00 mm 针距不意味着连接器系列、卡扣和正负极绝对匹配**：购买前用实物/照片核对，尤其不能把差分 Class-D 喇叭输出的负极随意接到公共地。

为了减小转头负载和提高低频音质，建议左右扬声器放在**固定底座两侧**，用对称声孔和小声腔；头部移动的两对喇叭线需要经过转轴的柔性线缆环并防缠绕。也可以把扬声器放在头壳，线更短但转动负载更大。保证麦克风声学隔离、音频 AEC 与散热。

## 4. 电机控制与供电：复用现成部件，避免烧板

- 舵机 PWM 优先评估 ESP-IDF 官方外设或现成 **PCA9685 I2C 舵机控制板**。详见 [Adafruit PCA9685 官方接线](https://learn.adafruit.com/16-channel-pwm-servo-driver/pinouts)：逻辑 VCC、舵机 V+ 与信号地职责不同。S31 的 I2C 可能与 codec、触摸等共享；先查真实自由 GPIO/接口/I2C 地址，不能凭 SDK 宏断定可用。
- **舵机电源必须独立按型号供电**。Hiwonder LD-1501MG 需 6–8.4 V（常用 7.4 V），绝不直接从 GPIO/3.3V/板载功放取电；低压标准舵机则依厂家规格。信号地共参考、合理电流裕量、浪涌保护、急停与限位必要。
- S31 开发板单独保持合适 5V USB 供电。乐鑫指出高功率音频 + USB Host 负载时应考虑最多 **3A** 电源裕量；不要未经设计把 7.4V 舵机电源直接注入 5V 输入。
- 先称量 **屏幕 + 两层 PCB + 壳 + 摄像头 + 线束**，测水平重心到俯仰轴的偏移。静态重力扭矩近似 = 头部重量(kg) × 水平力臂(cm) kg·cm，须另计启动加速度、安全系数和**厂家峰值/堵转扭矩并不代表连续可用扭矩**。
- 旋转角度、线束、底座稳定性、双麦克风/屏幕/摄像头孔、外壳承力、抗夹手和抬起时禁动都应在实物上验证；当前仓库并没有真实电机或反馈的已运行驱动。

## 5. 推荐组合与外观目标

    [官方 S31 Korvo-1 顶壳 + 主板、LCD、摄像头与麦克风] ← 可动「头」
                              |
                    [现成 2-DoF 俯仰架]
                    [现成 2-DoF 水平底座]               ← 「颈」
                              |
              [外层圆润的稳重底座/隐藏舵机与走线]
                [左 3W 扬声器]   [右 3W 扬声器]

**外观方向：** 类似 StackChan 的圆角桌面机器人，但保持我们原定独立眼睛/主题 UI、加大的 4.3 英寸全面屏，米白/玉白外壳、深色前脸、侧面细孔扬声器、螺丝藏于后盖或底面。该组合是有来源的模块拼装方案，**不是已经存在的 Korvo-1 免改装成品**；差的应该只是一块转接片和底座外观罩，而不是新的主板、音频功放或自行造舵机机构。

**采购前最小验证顺序：** 先下载官方 CAD 打印壳，测合壳效果 → 购买/试听 2×4Ω/3W 无源扬声器 → 实测活动头质量/重心 → 再决定 Hiwonder 强壮套件或 DFRobot 紧凑套件 → 采购合适的舵机电源和控制器 → 做转接片/底座外罩。不要提前承诺装配尺寸与承重。

## 6. 新的蓝牙音频源码案例

[han0519/esp32-xiaofeng](https://github.com/han0519/esp32-xiaofeng)：项目公开了 S31 蓝牙 **A2DP Sink**、AVRCP、HFP 类型 UI 和 ESP-GMF/LVGL 媒体功能。乐鑫社区展示了该演示 ([社区原文](https://esp32-s31.espressif.com/zh-hans/community/14))。它提供 Mac→小呆**蓝牙外放**的可参考实现；但**不是 Bot→Mac 麦克风/RPC 自动实现**，后者需独立 BLE GATT 或兼容 HFP 方案与 CoreBluetooth/音频 profile 测试。

**关键兼容风险：** 这个开源项目 README 标的 ES8311/ES8388/ES7210、8MB PSRAM 与官方 **Korvo-1 V1.1 的 ES8389/16MB PSRAM** 不一致，可能是复用模板或另一个硬件配置。**请先实际检查板级初始化与接线，严禁直接声称现成固件可以刷到我们的板子。**其 Unlicense 主仓库不自动覆盖 vendored ESP-GMF/LVGL/字体资源许可证。

## 7. 必须标注的未知项

**未验证：** 无需改孔就能直接装双轴的 Korvo-1 成品壳（未找到）；官方 STL 与云台的直接安装配合；扬声器插头实际匹配；整套头部动载/舵机供电/散热；蓝牙全双工语音；外壳 STL 商用许可；是否需要更大更重的底座。当前不应发出未经测试的“一键购物/直接兼容”保证。
