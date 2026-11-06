# bot_vision：本地相机与官方人脸检测适配器

这个组件把 **ESP32-S31-Korvo-1 V1.1 的 OV3660 DVP** 接到现有
`vision_attention`，实现实际的相机取帧、官方 ESP-DL 人脸检测和临时多人追踪。
相机驱动、传感器寄存器配置、神经网络模型和推理后端均使用乐鑫组件；本项目只写
生命周期、缓冲区、坐标、质量过滤和事件适配。

**当前工作仅完成源码和文档，没有编译、测试、烧录或硬件验证。** 硬件尚未到货，
帧率、推理耗时、峰值内存、曝光恢复时间、相机安装方向和最终屏幕映射均未测量。
`bot_vision_start()` 返回成功只表示工作任务已创建，不表示相机已经工作。

## 实际实现范围

- 通过平台回调使用官方 BSP 的共享 I2C、DVP 和 XCLK 初始化。
- 通过 `esp_video` 的 `/dev/video2` 使用 V4L2 `MMAP` 队列抓取画面。
- 按真实 `G_FMT` 区分 RGB565 大端、RGB565 小端和 RGB888，按行跨度复制。
- 在独占 PSRAM 副本中完成 180° 旋转，然后调用官方
  `HumanFaceDetect(HumanFaceDetect::MSRMNP_S8_V1, false)`。
- 读取人脸框、置信度、两个眼睛的关键点，转换为直立、不镜像的归一化坐标，交给
  `vatt_tracker_update()`。最多保留四个人脸，输入按检测置信度排序。
- 对错误帧、短帧、异常尺寸、暗光和过期结果发布 unavailable；正常且无人脸的
  画面才进入正常的零人脸观测流程。
- 支持开始、暂停、夜间停流、恢复、流故障恢复和有限等待的停止接口。
- 回调只输出健康状态和可复制的 `vatt_snapshot_t`，不输出照片或 DMA 指针。

五点人脸关键点只支撑人脸位置与临时追踪。当前实现**不产生身份 embedding、家庭
成员身份、声纹关联、虹膜位置、头部姿态、视线方向或眼神接触证据**。
`track_id` 不是“认识了某个人”；机器人把眼睛移向人脸位置，也不能说明这个人
正在看机器人。`attention_manager` 决定关注对象，视觉组件本身不选行为、不唤醒
设备、不驱动屏幕或电机。

## 官方来源和固定版本

以下链接指向本次阅读的固定提交。它们是接口与适配依据，不表示已经构建验证。

| 官方来源 | 本项目直接使用的版本 / 阅读提交 | 复用内容 |
| --- | --- | --- |
| [esp-bsp / esp32_s31_korvo_1](https://github.com/espressif/esp-bsp/tree/67f41a169401d1f23e827188dd08fb351cb0febe/bsp/esp32_s31_korvo_1) | BSP `1.1.0`；提交 `67f41a169401d1f23e827188dd08fb351cb0febe` | 板级 DVP、OV3660、共享 SCCB/I2C、XCLK；由 `bot_platform` 调用 |
| [esp-bsp / display_camera_video](https://github.com/espressif/esp-bsp/blob/67f41a169401d1f23e827188dd08fb351cb0febe/examples/display_camera_video/main/app_video.c) | 同上，例程标记 `CC0-1.0` | `open → G_FMT → REQBUFS → QUERYBUF → mmap → QBUF → STREAMON → DQBUF → QBUF`；S31 RGB565 冷启动格式重设 |
| [esp_video](https://github.com/espressif/esp-video-components/tree/ac08aff99e5193fa384452c02ccdb4db29060c1a/esp_video) | `2.4.1`；提交 `ac08aff99e5193fa384452c02ccdb4db29060c1a` | 官方 V4L2 驱动、DVP 数据路径、取帧超时 ioctl |
| [human_face_detect](https://github.com/espressif/esp-dl/tree/512ed0b0dc5f5320f3d9ca5a5aea189803f87f31/models/human_face_detect) | `0.5.2`；提交 `512ed0b0dc5f5320f3d9ca5a5aea189803f87f31` | MSR + MNP 检测模型与五点关键点 |
| [ESP-DL 人脸检测例程](https://github.com/espressif/esp-dl/blob/512ed0b0dc5f5320f3d9ca5a5aea189803f87f31/examples/human_face_detect/main/app_main.cpp) | 同上；`esp-dl 3.3.14` | 官方 `HumanFaceDetect::run()` 与模型输入结构 |
| [ESP-DL 图像类型](https://github.com/espressif/esp-dl/blob/512ed0b0dc5f5320f3d9ca5a5aea189803f87f31/esp-dl/vision/image/dl_image_define.hpp) | 同上 | `img_t`、`RGB565BE`、`RGB565LE`、`RGB888` |

`idf_component.yml` 固定本组件的直接依赖：`esp_video =2.4.1`、
`human_face_detect =0.5.2`、`esp-dl =3.3.14`，目标限定 `esp32s31`，IDF 要求
`>=6.1`。BSP 的官方依赖为 `esp_video ~2.4.1`，因此没有跟随新的 `esp_video`
主分支。间接依赖仍遵守官方 manifest，例如 `esp_cam_sensor 2.4.*`；本轮没有
运行依赖解析，也没有声称存在经过验证的完整依赖锁文件。

本组件不使用旧的 `esp32-camera`、`esp_camera_fb_get()` 或 ESP32-S3 的板级引脚。
源文件保留上游来源说明。实际驱动和模型从官方组件包取得，不在这里重新实现。

## 接入方式

```c
bot_vision_config_t cfg = bot_vision_default_config();
cfg.context = app;
cfg.prepare_camera = prepare_camera;  // bot_platform_prepare_camera(...)
cfg.on_status = vision_status;        // copy camera health into local bus
cfg.on_snapshot = vision_snapshot;    // bot_bus_publish_vision(...)
cfg.start_enabled = false;
cfg.calibration_valid = false;

esp_err_t err = bot_vision_start(&cfg, &app->vision);
// Runtime owns the requested state; health does not permanently disable retry.
if (err == ESP_OK) {
    bot_vision_set_enabled(app->vision, runtime_snapshot.camera_requested);
}
```

`prepare_camera()` 在视觉任务中调用；平台必须在整个启动周期内缓存首次结果，
包括失败结果。`on_status()` 总在对应 `on_snapshot()` 之前调用，二者均在视觉
工作任务执行。回调必须立即复制或入队并返回，不能保存传入指针、阻塞等待行为
线程、执行 UI 更新或在回调中调用 `bot_vision_stop()`。

`stream_active && observation_valid` 表示当前有可用观测；正常的“没检测到脸”
也是可用观测。`stream_active=true`、`observation_valid=false` 可以表示相机
正在预热、画面过暗或结果过旧。调用者须使用快照本身的时间戳处理新鲜度，不能把
持续为 true 的健康标志当作一张始终新鲜的画面。

`calibration_valid` 独立于健康状态。默认 false；它表示实际安装方向和机器人
屏幕/电机映射是否已经确认，不代表用户视线已被估计。项目级的
`CONFIG_ESPBOT_GAZE_CALIBRATED` 即便启用，也不会把本组件没有产生的视线、
瞳孔或眼神接触 validity 标记置为 true。

## 帧与模型结果的所有权

1. 单个工作任务独占 V4L2 fd、队列、检测器和 `vatt_tracker_t`。
2. `VIDIOC_REQBUFS` 的 `MMAP` 缓冲由 `esp_video` 分配；`mmap()` 给出的是
   驱动拥有的内存视图。此组件不对这些指针调用 `free()` 或 `heap_caps_free()`。
3. 每次成功 DQBUF 后，检查 buffer index、错误位、`bytesused` 和实际映射长度。
   有效画面按 `bytesperline` 拷入组件自己的紧凑 PSRAM 图像，必要时翻转 180°。
4. 在推理前 QBUF 归还相机缓冲。暗光、预热和被丢弃的有效索引帧同样归还。
   驱动若返回非法索引，组件不能安全归还未知缓冲，会进入流关闭与恢复。
5. 检测器只读取独占副本。`run()` 返回的结果属于检测器，立即转换为栈上的
   `vatt_observation_t`；其内部 `std::list` / `vector` 指针不跨回调传播。
6. 成功 STREAMOFF、关闭 fd 后，由驱动释放它拥有的 MMAP 缓冲，再释放本组件
   的图像副本。STREAMOFF 失败时保留 fd 和缓冲，不在潜在 DMA 写入时释放内存。

组件是单实例。`bot_vision_start()` 拷贝配置和设备路径，调用者仍须保证
`context` 在停止完成前有效。PSRAM 图像、模型和追踪器不会被两个任务并发使用。

## 格式、坐标与暗光

默认传感器配置是 **640 × 480，RGB565 大端，10 fps**。

| V4L2 格式 | ESP-DL 格式 | 每像素字节数 |
| --- | --- | --- |
| `V4L2_PIX_FMT_RGB565X` | `DL_IMAGE_PIX_TYPE_RGB565BE` | 2 |
| `V4L2_PIX_FMT_RGB565` | `DL_IMAGE_PIX_TYPE_RGB565LE` | 2 |
| `V4L2_PIX_FMT_RGB24` | `DL_IMAGE_PIX_TYPE_RGB888` | 3 |

组件以 `G_FMT` 的实际结果为准，YUV/JPEG/Bayer 等格式会明确报告不支持。它不把
YUV422 的两个字节误当成 RGB565，也不偷偷覆盖传感器配置。恢复时复用 BSP
例程中 S31 RGB565 冷启动的 200 ms 延迟和 G/S_SENSOR_FMT 步骤，再重新获取格式。

软件副本默认旋转 180°；传感器 HFLIP/VFLIP 显式设为 0，与该 BSP 宏一致。帧先
摆正，模型再运行，因而不需要对模型输出做第二次旋转。180° 是依据板级约定的
初始配置，仍须在实物安装后确认。

MNP 五点顺序为：左眼、左嘴角、鼻、右眼、右嘴角。两个眼睛使用索引
`[0,1]` 和 `[6,7]`，不会误取嘴角。人脸框、眼坐标经范围检查后，以 `width-1`
和 `height-1` 归一化到 0..1，原点在左上角。脸部置信度同时应用于官方 MSR/MNP
两阶段阈值和适配器输出过滤；如修改配置中的追踪器阈值，也应保持相应含义一致。

暗光判断对每隔 8 像素的 RGB 值采样，计算粗略亮度均值。它是计算成本较小的
画面可用性门槛，不是曝光测光、活体检测或全黑人脸判定。默认阈值 12/255。
强背光、局部阴影、曝光恢复及反光仍可能误判，需实物调参。

## 时间戳与过期结果

固定版本的
[`esp_video_ioctl_dqbuf()`](https://github.com/espressif/esp-video-components/blob/ac08aff99e5193fa384452c02ccdb4db29060c1a/esp_video/src/esp_video_ioctl.c)
填充 index、bytesused、flags 等字段，**没有填充 `v4l2_buffer.timestamp` 或
sequence**。因此此实现不会把未初始化值当作硬件时间，也不会宣称拿到了准确的
曝光时刻。

每轮先用官方 `VIDIOC_S_DQBUF_TIMEOUT` 设置零等待，以有限次数排空已经完成的
队列，再恢复有限等待去取下一帧。VFS 没有实现可依赖的 O_NONBLOCK 语义，因此
不用它假装无阻塞取帧。排空达到上限仍未追上生产者时，该轮放弃推理。

取到用于推理的帧后，立即记录 `esp_timer_get_time()/1000`，发生在拷贝和推理
之前。状态明确报告 `time_source=BOT_VISION_TIME_DEQUEUE_ESTIMATE`、
`capture_timestamp_exact=false`。200 ms 的配置预算会加到处理耗时中；合计
超过默认 750 ms 的结果被丢弃。**这个预算是初始策略参数，不是已经测得或有
硬保证的传感器曝光误差上限。** 同步推理慢、驱动积压或实际帧率不符都需要后续
实测判断。

可用快照始终保留该帧的推理前时间，不在发布时刷新时间。无效/暗光/暂停等状态
发布“当前不可用”的新健康观测，`camera_ok=false`，不附带旧脸作为新检测。
相同或倒序的快照不会为了满足总线要求而改时间。低 32 位单调毫秒与现有运行时
一致，按有符号差处理计数器跨零；同一采集期间相邻观测之间须小于约 24.8 天。
成功暂停后会销毁临时 tracker，并清除快照排序时间锚点，因此暂停时长不受这个
半周期比较窗口限制。恢复时创建新 tracker，从新观测重新获取临时轨迹；消费者
也应退休已结束会话的快照，当前 runtime 已在非活跃状态清空 bus 中的视觉快照。

需要精确的传感器曝光/帧结束时间时，应在经过确认的官方驱动版本或扩展中携带
帧完成时钟，再贯穿上层接口；当前代码保留独立的时间来源字段以便明确升级。

## 暂停、夜间、恢复与停止

`bot_vision_set_enabled(false)` 设置最新请求并唤醒工作任务。它在当前已经开始
的同步推理返回后丢弃结果，执行 STREAMOFF、关闭相机 fd、释放帧副本，再发布
paused/unavailable，随后销毁 tracker 并清掉发布时间锚点。临时轨迹不跨暂停保留，
也不能把恢复后的同号 `track_id` 当成原来的那个人。暂停不会销毁模型，恢复可复用
模型；恢复时先重新创建 tracker，再打开相机流、重新排队，丢弃两帧预热后再输出。
tracker 分配失败会报告不可用并按恢复间隔重试，不会对空 tracker 进行推理发布。
暂停只做逻辑停流，**没有声称关闭传感器电源或
XCLK**：官方 BSP 没有向调用方交出对应时钟句柄。

[`bsp_camera_start()`](https://github.com/espressif/esp-bsp/blob/67f41a169401d1f23e827188dd08fb351cb0febe/bsp/esp32_s31_korvo_1/src/bsp_camera.c)
的 XCLK 句柄是函数局部变量；在所阅读实现中，后续 `esp_video_init()` 失败时
没有释放这个句柄。所以视觉组件和平台均缓存第一次准备结果，平台每次开机
**最多尝试一次 BSP 相机初始化**。失败需要重启设备；不能以每两秒重建 BSP
的方式重复分配时钟。成功准备后的普通 open/STREAMON/取帧故障可以重开流恢复，
默认三个连续取帧超时触发重开，恢复间隔 2 秒。

`bot_vision_stop(handle, timeout_ms)` 发出停止请求并等待清理完成。超时会保留
句柄和资源，可以稍后重试；成功后句柄失效。工作任务清理后等待调用者回收，
因此重试 stop 不会对已经自删除的任务句柄发送通知。关闭流失败时不会强行释放
仍可能被驱动使用的内存。停止调用必须与其他生命周期和状态读取调用串行化，
不支持两个调用者同时停止同一实例。

屏幕夜间状态由平台负责。所阅读 S31 BSP 的亮度/背光接口返回不支持，项目的
黑屏路径是逻辑黑色覆盖；这里不把它描述为硬件背光已断电。

## SDK 配置与内存预算

主工程需要以下官方选项；本组件不拥有 BSP 初始化，也不复制板级引脚定义。

```ini
CONFIG_CAMERA_XCLK_USE_LEDC=y
CONFIG_ESP_VIDEO_ENABLE_DVP_VIDEO_DEVICE=y
CONFIG_CAMERA_OV3660=y
CONFIG_CAMERA_OV3660_AUTO_DETECT_DVP_INTERFACE_SENSOR=y
CONFIG_CAMERA_OV3660_DVP_RGB565_BE_640X480_10FPS=y
CONFIG_CAMERA_OV3660_DVP_DEFAULT_FMT_RGB565_BE_640X480_10FPS=y
CONFIG_FLASH_HUMAN_FACE_DETECT_MSRMNP_S8_V1=y
CONFIG_HUMAN_FACE_DETECT_MSRMNP_S8_V1=y
CONFIG_HUMAN_FACE_DETECT_MODEL_IN_FLASH_RODATA=y
```

PSRAM 必须可用。默认三张 RGB565 相机缓冲和一张紧凑副本，按 640 × 480 × 2
计算，仅这四张图就是 **2,457,600 字节，约 2.34 MiB**。驱动可能还有备用帧、
DMA 描述符等分配；官方模型还有权重、张量和临时内存，同时屏幕、音频也占用
PSRAM。这个数字是图像部分的算术预算，不是实测峰值。图像副本分配失败会报告
`ESP_ERR_NO_MEM`。官方模型内部仍可能存在断言或无法恢复的分配失败；外层
`new (std::nothrow)` 不构成对模型内部所有分配的恢复保证。

模型使用官方 flash-rodata 方式，权重随应用镜像，不需要额外 `human_face_det`
分区。需要应用分区能容纳实际链接后的镜像；本轮没有生成镜像或测量占用。
`Kconfig` 只提供任务栈 8192 字节、优先级 3 的初始设置，不把这些初值描述为已
调优的容量。默认推理间隔 200 ms 是最大调用频率目标，不保证能达到 5 fps。

## 文件说明与后续硬件确认

- `include/bot_vision.h`：稳定的 C 接口、回调和状态。
- `bot_vision.cpp`：实际相机/模型适配和唯一资源所有者任务。
- `idf_component.yml`：官方依赖版本；`CMakeLists.txt`：真实实现参与固件构建。
- `Kconfig`：工作任务资源设置；`INTEGRATION.md`：主程序接入摘要。

硬件到货后的工作包括确认 180° 安装和 H/V flip、RGB 字节序、相机帧率、首次
曝光恢复、亮度门槛、队列延迟、模型耗时、PSRAM 峰值、任务栈余量和共享 I2C
行为，再决定是否启用屏幕/电机朝向校准标志。这些项目尚未执行；本轮也没有
改动 `vision_attention` 的既有测试或添加以通过测试为名的替代检测器。
