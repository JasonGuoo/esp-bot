# 小呆 / ESP32-S31-Korvo-1 V1.1 — 3D 打印与采购清单 V2

更新：2026-10-30。**这是供应商资料核对后的采购候选清单，不是已装配验证的 BOM。** 目标是漂亮的 4.3 英寸可转头桌面机器人，保留现有 Korvo-1 硬件，不重造音频功放、电机机构或基础外壳。产品链接可能变价/缺货；优先厂家渠道，澳洲备选渠道仅为方便本阶段试验。

## 一、3D 打印：哪些文件真的适配现有板子？

**唯一明确匹配 ESP32-S31-Korvo-1 V1.1 的现成打印文件：乐鑫官方四部件外壳（2026-06-03）**。

- [全部官方 STL/STEP 文件目录](https://github.com/espressif/esp-dev-kits/tree/master/examples/esp32-s31-korvo/3d-printed-case)；[官方 V1.1 用户手册](https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32s31/esp32-s31-korvo-1/user_guide.html)
- **TOP**（顶壳/面部）：[STL](https://github.com/espressif/esp-dev-kits/blob/master/examples/esp32-s31-korvo/3d-printed-case/ESP32-S31-Korvo-1_Case_top_20260603.STL) · [STEP](https://github.com/espressif/esp-dev-kits/blob/master/examples/esp32-s31-korvo/3d-printed-case/ESP32-S31-Korvo-1_Case_top_20260603.STEP)
- **BTM**（底壳/后部）：[STL](https://github.com/espressif/esp-dev-kits/blob/master/examples/esp32-s31-korvo/3d-printed-case/ESP32-S31-Korvo-1_Case_btm_20260603.STL) · [STEP](https://github.com/espressif/esp-dev-kits/blob/master/examples/esp32-s31-korvo/3d-printed-case/ESP32-S31-Korvo-1_Case_btm_20260603.STEP)
- **STAND**（原厂静态支架）：[STL](https://github.com/espressif/esp-dev-kits/blob/master/examples/esp32-s31-korvo/3d-printed-case/ESP32-S31-Korvo-1_Case_stand_20260603.STL) · [STEP](https://github.com/espressif/esp-dev-kits/blob/master/examples/esp32-s31-korvo/3d-printed-case/ESP32-S31-Korvo-1_Case_stand_20260603.STEP)
- **LIGHT GUIDE**（导光件）：[STL](https://github.com/espressif/esp-dev-kits/blob/master/examples/esp32-s31-korvo/3d-printed-case/ESP32-S31-Korvo-1_Case_light_guide_20260603.STL) · [STEP](https://github.com/espressif/esp-dev-kits/blob/master/examples/esp32-s31-korvo/3d-printed-case/ESP32-S31-Korvo-1_Case_light_guide_20260603.STEP)

**先打印 TOP+BTM 做真机试装**；STAND 可作为固定开发模式对照，不能当作已经验证兼容的云台连接板。LIGHT GUIDE 根据实际材料/装配要求追加。为运动底座制作的**俯仰轴适配板、底座外观罩、喇叭座**可能需要在 STEP 上做最少的新设计；本轮没有找到已证实免改装的“Korvo-1 + 双轴 + 扬声器”现成一体 STL。

**其他结构示范，不是兼容打印件**：[cgro00/desk-robot](https://github.com/cgro00/desk-robot)（实做转头、音箱、电脑 Brain，屏幕 2.42"、XIAO 板）；[stack-chan/stack-chan](https://github.com/stack-chan/stack-chan)（可爱两轴小机器人，较小屏幕）；[m-esm/Parviz 的 STL](https://github.com/m-esm/Parviz/tree/main/stl)（7" 触屏及参数化机构，作者尚未实装）。这些只能借鉴造型、喇叭固定、走线或转轴布局，不能直接打印后安装我们的 4.3" Korvo。

## 二、具体采购型号（按「轻巧好看」优先）

| 组件 | 采购型号、数量 | 优先渠道 | 替代/核对 |
| --- | --- | --- | --- |
| 头部原装外壳 | **Espressif 20260603 TOP + BTM** ×1 组，按需要加 STAND/LIGHT GUIDE | 上述官方 GitHub STL（3D 打印服务商） | STEP 可用于适配孔位，打印前确认面板、镜头、麦克风、触控、USB 和天线净空 |
| 两轴支架 | **DFRobot FIT0004** 黑色金属 Pan/Tilt，**1 套，不含舵机** | [DFRobot 官网](https://www.dfrobot.com/product-340.html)；[Little Bird AU: DF-FIT0004](https://littlebirdelectronics.com.au/products/pan-and-tilt-kit-black-anodized-no-servos) | 厂家页面列明与标准尺寸 15kg 金属舵机兼容；需要头部连接板 |
| 伺服电机 | **DFRobot SER0019 / DSS-M15** 标准尺寸金属齿轮 **180° ×2** | [DFRobot 厂家](https://www.dfrobot.com/product-120.html) | 与 FIT0004 配套，约 6V 运行；**不是连续旋转舵机**，厂家峰值/堵转扭矩不等于连续静载 |
| 优选外放 | **Adafruit 3351 / ADA3351** 密闭音箱 **4Ω、3W，×2** | [Adafruit 厂家](https://www.adafruit.com/product/3351)；[Core Electronics AU](https://core-electronics.com.au/mono-enclosed-speaker-3w-4-ohm.html) | 70×30×17mm、26.4g/只，带 JST-PH 2pin；适合固定底座两侧，不用再设计完整喇叭声腔 |
| 低价外放替代 | **Little Bird 2×45mm 4Ω/3W PH2.0**，**1 对** | [Little Bird AU](https://littlebirdelectronics.com.au/products/2-x-45mm-4ohm-4-3w-stereo-speaker-with-ph2-0-plug) | 更便宜，**必须设计声腔/固定结构**；不和 ADA3351 重复买 |
| 舵机控制（可选） | **Adafruit PCA9685 PWM Servo Driver / #815，×1** | [Adafruit 原厂](https://www.adafruit.com/product/815)；[Core Electronics](https://core-electronics.com.au/adafruit-16-channel-12-bit-pwm-servo-driver-i2c-interface-pca9685.html) | 如果 Korvo 有可用 PWM GPIO，可先用 ESP-IDF MCPWM/LEDC；买 PCA9685 前确认 I2C 总线、地址 0x40 和针脚；不是电源板 |
| 舵机电源（需选型确认） | **稳压 6V DC，按照两只 SER0019 的实际峰值/堵转电流选规格**，工程初选 ≥5A 且留裕量 | 合规电源供应商/可调实验电源；完成电流量测后再选最终型号 | Korvo 自身需独立 5V USB 供电；不能将 6V 直接灌进 Korvo 5V/3.3V，伺服与控制板需规范共信号地 |
| 机械转接/收纳 | **STEP-derived adapter + rounded base fairing**（打印件，尚无适配当前头壳的现成文件） | 原厂 STEP 修改后交 3D 打印服务商 | 必须真实称重、校核重心与孔距后绘制；不要假装成已下载、即插即用的文件 |

**价格快照（非结算价格，2026-10-30）：** DFRobot FIT0004 厂家 US$6.50，澳洲 Little Bird AU$18.85；SER0019 厂家约 US$14/只；Adafruit 3351 厂家 US$3.95/只，澳洲 Core AU$8.95/只。采购不同地区有税费运费差异。

### 结构更结实但体积更大的备选

**Hiwonder SKU 21070016** 全金属双轴总成，含 **LD-1501MG ×2**、轴承、转盘、支架和紧固件。**[厂家页面](https://www.hiwonder.com/collections/all-product/products/pan-tilt)**。参数：转盘外径约 120mm、机构高 120mm、**本体重约 550g**，舵机使用 **6–8.4V**（推荐对应规格供电），标称 7.4V 17kg·cm；调查时 US$83.99。**优点是机械结构完整可靠性潜力高，缺点是头/颈比例可能显得巨大**。不能拿 6V 轻巧版的电源配置直接给 Hiwonder 7.4V 方案。只有称量整机头、验证轻巧支架容量不足后才推荐升级。

**不要下单：** DFRobot 已停产的旧全套 FIT0046；把为小摄像头设计的 SG90 9g 薄弱支架当 4.3 英寸整头承重方案；任何宣称 StackChan 2.0 英寸壳兼容 Korvo 4.3 英寸的卖家套件。

## 三、扬声器电气兼容与安装布局

ESP32-S31-Korvo-1 V1.1 使用 ES8389 与**两路 NS4150B Class-D 3W 功率放大器**，有两组 **4Ω/3W、针距 2.00mm 的喇叭接口**，参见[官方用户手册](https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32s31/esp32-s31-korvo-1/user_guide.html)。直接用两只**无源**喇叭；**不能再将另一个功放模块串接在功放输出上**。

**关键风险：** “JST-PH 2.0 / 针距 2mm”不保证具体接头塑壳、扣位、极性与 Korvo 板上插座一致。必须先核对 V1.1 原理图/实物插座，再插线。扬声器输出可能是桥接差分 BTL，任何一边的负线不得想当然接系统地。若喇叭在固定底座，而主板在旋转头，需为喇叭线/供电/控制信号做**转轴穿线、应变释放、线缆限转与防缠绕**；如果将喇叭装头部，减少跨轴线但会增加俯仰负载。先用官方板载功放播放和测电机噪声/AEC，再选择最终机腔。

## 四、装配外观构想（不是可直接打印的完整配件包）

正面：**浅色圆角矩形头部 + 深色全屏脸（保留小呆原设计的眼睛）**；右上相机窗口无遮挡；头壳与俯仰轴之间是一块**小型打印转接片**；标准两轴伺服装进**圆润圆柱形底座**；左/右底座侧面各一只带声腔扬声器；底座后侧 USB 电源与可维修线缆。不要展示外露舵机和乱线，外观圆润类似 StackChan 的体块比例、但**4.3 英寸头部更大**。该渲染/示意仅用于审美与物料标注，**未验证装配可行性**。

**推荐采购顺序：** 先下载并打印 TOP+BTM → 试装主板/称重/量测重心/测螺孔 → 同时试听 ADA3351 两只 → 再下单 FIT0004+SER0019 ×2 与独立供电 → CAD 修改最少的适配片/罩壳 → 验证线缆、电机限位、抗倾覆、噪声和麦克风/回声后再固定整体设计。

## 五、验证状态 / 没有做过的工作

只有在线供应商资料与 GitHub 文件名/仓库资源核对；**尚未**实际下载整套二进制 STL 并切片、打印、组装或实际称量 Korvo 头部、核对扬声器插头极性、确认自由 GPIO / 电机供电、电流及扭矩、校准眼睛/摄像头、验证双轴运行，也**没有**证明 DFRobot 支架与官方壳直接对孔。单个零件的兼容并不意味着整机免机械适配。无 CI，遵守项目 [AGENTS.md](../AGENTS.md)。
