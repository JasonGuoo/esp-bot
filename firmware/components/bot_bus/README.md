# bot_bus：有界事件、命令与最新视觉快照

**状态：已写源码，未编译、未测试、未在 ESP32-S31-Korvo-1 V1.1 运行。** 组件直接使用 ESP-IDF / FreeRTOS 的 `xQueueCreate/xQueueSend/xQueueReceive`、mutex 和 portMUX，不创建专用任务，也不包含行为决策。

## 三类数据各自的传递方式

| 数据 | 存放位置 | 默认容量 | 消费约定 |
|---|---|---:|---|
| 关键离散事件 | 独立 FreeRTOS FIFO | 24 | 先于普通事件；溢出锁存故障 |
| 可替代的成功健康状态 | 普通 FIFO | 16 | 满时明确返回 timeout 并计数 |
| 远程控制命令 | 独立 FIFO | 8 | runtime 仲裁并对每条取出的命令回执 |
| 最新视觉状态 | mutex 保护的单个 `vatt_snapshot_t` | 1 | 旧帧被新帧取代，不积压画面 |
| 本地停止 | portMUX 保护的锁存位 | 不消耗 FIFO | runtime 在处理消息前先取出 |

关键事件包含完整关键词唤醒、有意触摸、会话/VAD/播放完成、设置、链路变化、音频与摄像头健康、夜间策略、故障，以及显示失败。只有成功的显示健康状态使用普通队列。`bot_bus_receive()` 总是优先取关键队列。不同队列之间不承诺全局顺序，所以 runtime 会按消息种类与原始单调时间拒绝倒退状态。

没有通用订阅列表或隐式广播；唯一消费者是 runtime。也没有把 PCM、相机帧、图像或网页塞进这些结构体。媒体由自己的 owner 持有，使用独立的有界缓冲协议。

## 失败必须可见

`bot_bus_post()` 对队列满返回 `ESP_ERR_TIMEOUT`。关键队列满还设置 `signals.critical_overflow`，它与队列存储无关，runtime 随后撤销会话、上行和播放权限。不能只记录日志然后继续执行可能漏了一半的会话。

`BOT_EVENT_LOCAL_STOP` 直接使用与 `bot_bus_request_stop()` 相同的锁存路径，不需要队列空间。stop 与 overflow 可以同时存在，`bot_bus_take_signals()` 原子地读取并清除两项；每次都必须同时处理两项。stop 本身不是故障；overflow 需要显式本地恢复。

命令满不会挤掉已有命令：`bot_bus_submit_command()` 返回失败，网络适配器应立即回覆拒绝。已有命令仍由 runtime 消费，保证其 accepted/rejected 回执有明确归属。统计项区分关键事件拒绝、普通事件丢失、命令拒绝和视觉旧帧拒绝。

视觉发布若 mutex 被占用，立即返回 timeout，不阻塞相机任务。相同或倒退的捕获时间返回 `ESP_ERR_INVALID_STATE`，不能通过重新填写当前时间使旧推理结果变新。runtime 可以保留上次成功读取的副本，但始终按副本原时间判断过期。视觉结果新鲜度没有依靠“队列刚取出来”这种错误推断。

## 接入约定

```c
bot_bus_t *bus = NULL;
bot_bus_config_t config = bot_bus_default_config();
esp_err_t result = bot_bus_create(&config, &bus);
/* 应用处理 result；所有生产者仅在 bus 成功创建之后启动。 */
```

生产者在任务上下文中填写零初始化的 `bot_event_t`，设置种类、来源、原始 `observed_ms` 与必要的会话 token，然后 `bot_bus_post(bus, &event, 0)`。回调中使用零等待并检查结果。没有 FromISR API；GPIO ISR 应先通过板级事件任务转交。

来源枚举是应用内部所有权标记，不是网络身份认证。网络输入只能转成受限的 `bot_command_t`；不能允许 Mac/网页任意指定 `BOT_SOURCE_LOCAL_UI`、触摸、唤醒、停止恢复或健康状态。`BOT_BUS_SCHEMA_VERSION=1` 是本地 C 合约版本，不是可直接把结构体内存发送到线上协议的 ABI。协议层应显式序列化字段并使用已有安全通道。

所有时间是本地单调毫秒低 32 位。所有比较窗口小于 `2^31ms`。队列容量可在创建时调整，关键/普通队列分别为 1–256，命令队列为 1–64。创建失败会释放已经分配的队列；运行期不会为单条消息做堆分配。销毁之前必须停止全部生产者和消费者。

参考：[ESP-IDF FreeRTOS](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-reference/system/freertos_idf.html)、[FreeRTOS Queue API](https://www.freertos.org/Documentation/02-Kernel/04-API-references/06-Queues/00-QueueManagement)。


摄像头可用/不可用状态均走关键队列，因为生产者可能只在变化时上报，不能把唯一恢复通知当成可覆盖的采样帧。显示成功状态留在普通队列，满时显式返回失败并计数；真正的本地显示请求执行不依赖这份回执。`bot_bus_clear_vision()` 在被动期清除 latest snapshot 和排序时间；若互斥量暂时繁忙，返回 false，由 owner 下个 step 重试。播放事件的 `data.playback.id` 必须复制音频适配器的真实操作 id。
