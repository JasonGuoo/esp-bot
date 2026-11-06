# attention_manager：唯一视觉关注目标

**状态：已实现，未编译、未运行测试、未实机验证。** 输入是既有 `vision_attention` 的多个人脸快照，输出是一个 `bot_focus_snapshot_t`。没有增加检测模型，也没有把临时跟踪升级为持久身份识别。

## 所有权与算法

`vision_attention` 继续拥有全部 track、眼部位置和人看向 Bot 的证据。manager 忽略其历史 `primary_track_id` 字段，自行选择唯一正式目标。后面的 `attention` 只消费这一目标做眼睛/头部空间规划，`face` 只绘制；manager 不调用驱动、LCD 或电机。

候选必须同时具备可见、没有关联歧义、有效的临时 track ID、新鲜的真实观测时间、有限的检测分数、有效边界框和 `[0,1]` 内的眼睛中点。分数由检测置信度、画面占比和靠近中心程度组成；相同分数使用 track ID 做稳定排序。看向 Bot 不作为同意对话或身份推断的依据。

| 参数 | 默认值 | 作用 |
|---|---:|---|
| `observation_max_age_ms` | 350ms | 超时立即撤销位置有效性 |
| `acquire_dwell_ms` | 160ms | 新目标需跨真实帧持续存在 |
| `switch_dwell_ms` | 900ms | 非会话模式换目标的持续优势要求 |
| `lost_hold_ms` | 1800ms | 短暂丢失保留原 track 的上限 |
| `min_face_confidence` | 0.55 | 有效候选的最小检测质量 |
| `switch_margin` | 0.18 | 挑战者相对当前目标的分数优势 |

dwell 使用相机快照的捕获时间，只在**新帧**推进。主循环以 50Hz 重读同一帧，不会被算成连续观察了 50 次。过长的帧间隔会重新计算候选等待时间。

进行中的会话优先保留仍可见的同一对象，不会因为另一个人突然更靠近画面而跳过去。目标暂时丢失时立即设置 `localization_valid=false`，保留临时 track 与 generation 到丢失保留期结束，随后进入 reacquiring。保留期内的 ID 不能当作仍然看到人；attention 也不能继续按过期坐标追踪。

## 状态与代次

状态为 `none/acquiring/tracking/temporarily_lost/reacquiring`。只有位置有效的 tracking 输出可用于当前相机坐标系的眼睛指向。无相机、太暗、姿态未校准或检测失败时，由视觉模块提供 unavailable/有效性标记，runtime 关停相应许可。

正式目标发生变化或释放时，focus generation 改变。绑定某一视觉目标的远程命令必须携带这一代次，runtime 会在不匹配或目标位置无效时拒绝。代次和 track ID 都只适用于当前进程里的短期跟踪，不能保存成孩子的姓名、家庭成员或跨会话识别结果。

眼睛在画面中的位置与“人是否看向 Bot”是独立信息。`gaze_evidence_valid` 还要求上游显式提供有效眼神接触证据以及有效 gaze 质量；仅有五点人脸 landmark 时通常为 false。当前 detector 不会凭眼睛位置虚构凝视方向。

## 调用

```c
attention_manager_t manager;
attention_manager_config_t config = attention_manager_default_config();
if (attention_manager_init(&manager, &config)) {
    attention_manager_set_policy(&manager, true, true);
    bot_focus_snapshot_t focus;
    attention_manager_step(&manager, &latest_snapshot, now_ms, &focus);
}
```

正常应用由 `bot_runtime` 内部调用，不需要 main 再创建第二个 manager。`set_policy(false, ...)` 立即清除目标定位；`step(NULL, now_ms, ...)` 仍会使旧位置过期。所有 API 属于同一个 owner 任务，不在相机 worker 中执行，也不存储输入指针。

视觉坐标以当前相机朝向为准。当前固定头没有已安装 IMU、关节反馈或电机；manager 不会生成虚构的静止零姿态，也不会声称能转动相机找回屏幕外的人。下游由平台向 `attention` 明确提供 stationary/校准能力。

来源：[既有 vision_attention](../vision_attention/include/vision_attention.h)、[ESP-DL Human Face Detect 示例](https://github.com/espressif/esp-dl/tree/master/examples/human_face_detect)。此组件只实现产品所需的选择与稳定策略，上游的检测、landmark、采集和模型均由视觉 owner 复用。

关闭视觉策略时也清除捕获排序历史；下次明确唤醒从新帧重新建立目标，不使用跨越长时间休眠的旧时间戳。
