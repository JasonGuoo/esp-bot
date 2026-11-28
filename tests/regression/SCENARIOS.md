# 小呆功能回归场景

由 `scenarios.json` 生成：`./tools/check-functional-regression.sh --list`。automated 只指所列实现和孪生可自动执行，实际结果见每次 report.md；acceptance 是独立待验项，deferred 是用户暂缓。

## 待机、唤醒与本地控制

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| BOT-01 / firmware | 家人在旁边说话、人脸靠近、短暂触碰；再有意触摸唤醒 | 被动事件不打开收音；明确触摸获得本轮许可，离线仍有本地状态反馈 | automated |
| BOT-02 / firmware | 完整短语跨多个音频块；两次半句中间有安静帧；关闭许可 | 完整输入保留；半句不能跨边界拼成唤醒，未授权固定命令不生效 | automated |
| BOT-03 / firmware | 空闲变无聊、困倦、睡眠；夜间和环境亮度来回变化 | 生命周期有迟滞和边界，黑暗不关闭本地唤醒；有效会话有优先级 | automated |
| BOT-04 / firmware/macOS | 近脸、安静房间、夜间反复唤醒；空闲表情切换 | 反馈符合场景且不连续重复；会话表情优先于被动反应 | automated |
| BOT-05 / firmware | 在设置页调音量后立即关闭；试听中停止；断网执行本地命令 | 最后编辑保留；试听不取得麦克风许可，停止撤销旧播放 | automated |
| BOT-06 / macOS/firmware | 远程音量命令、无效报告、设置页争用和重连 | 等待真实设备确认；不覆盖正在编辑的设置，不在重连后重放命令 | automated |

## 采集、回答与对话连续性

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| VOI-01 / protocol | 请告诉我……树叶为什么是绿色的，停顿后继续 | 续说使暂停候选失效；本轮完整帧到齐才提交，尾句不丢 | automated |
| VOI-02 / protocol | 重复 commit、少帧、多帧、错误 stream、排空前断线 | 严格核对声明帧和本轮；重复提交不延长期限，断线结算 pending | automated |
| VOI-03 / macOS/firmware | 两次没有听清；再发生其他故障或有效提问 | 空输入只自动重试一次；提示可见且有界，其他路径重置计数 | automated |
| VOI-04 / macOS | 声纹很慢；后台 TTS 正忙；用户问普通问题 | ASR 独立前进；后台 TTS 让出前台首句通道，普通回答不等待身份 | automated |
| VOI-05 / macOS | 慢回答、刚唤醒、近期已应声、首句先到 | 符合条件才短应一次；ACK 不占首句，已有正文不再插入等待提示 | automated |
| VOI-06 / macOS | 讲六句带引号故事；后一句先准备好；最后一句超容量 | 只按顺序冻结不可变前缀；不丢中段，超容量明确说明尾部未送出 | automated |
| VOI-07 / protocol | 讲小兔故事时补一句把小兔换成小狗；正文还没播完 | 保留原问题，仅完整回执的文字作为已听内容；新轮不能借旧权限 | automated |
| VOI-08 / macOS | steer 通知早于 ACK；拒绝、超时、旧通知迟到 | 交接一次，失败回到新轮；旧屏障不会吞掉新轮，也不漏出失败输出 | automated |
| VOI-09 / protocol | 思考、准备故障提示或已播放时点停止；很晚才完成旧工作 | 当前控制立即收回许可，迟到准备不重新发声；清理只撤旧租约 | automated |
| VOI-10 / protocol | 音频与显示回执交换到达次序、重复或属于上一轮 | 必要真实回执齐备才追问；长于八秒的合法段仍能完成 | automated |
| VOI-11 / macOS | 小呆晚安啦；讲个晚安故事；先别说再见；谢谢继续讲 | 纯道别播完安静；故事、否定和继续请求保持会话 | automated |
| VOI-12 / macOS | 本轮音频关闭或失败；中文字幕到长度边界 | 明确无声音状态；UTF-8 不截坏，故障不伪装成用户已听见 | automated |

## 模型路由与资源边界

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| ENG-01 / macOS | Luna 慢、退出、不可用、HTTP 路线降级后恢复 | 按现有策略竞速和恢复；胜出后冻结，不重放已发前缀 | automated |
| ENG-02 / macOS | 上一轮一直占 Luna，新问题同时抵达，名额耗尽 | 有限等待；无状态 HTTP 不借旧线程；容量满明确拒绝 | automated |
| ENG-03 / macOS | 供应商网络或 5xx 出错；冷却过后重试 | 冷却时跳过，期满可恢复；未知效果不自动换供应商重放 | automated |
| ENG-04 / macOS | 模型无限流、消费者很慢、最后碎片越过句数限制 | 字节、事件和句数有界；明确结束或失败，不静默吞语义 | automated |
| ENG-05 / macOS | 正常聊天、工具请求、私密成员和待补参任务交错 | 普通轮走 core；工具、私密、补参按规则留在 OpenCode，主体不串线 | automated |
| ENG-06 / macOS | 同一会话连续问、取消、重开或家庭日重置 | 已知线程只送增量；新线程完整上下文，不带上次未授权轮次 | automated |

## 配对、传输与断线

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| NET-01 / macOS | 正确配对、错误凭据、未批准设备命令和重复连接 | 只有认证设备可接收控制；新连接不能抢占另一有效会话 | automated |
| NET-02 / protocol | PCM 内容被篡改、错方向；最终 PCM 遇满队列 | 认证拒绝篡改；最终帧和 Finish 一起获得容量，不产生半个结束 | automated |
| NET-03 / protocol | 网络停止读，控制台点停止，新轮继续入队 | 写队列按帧和字节有界；fence 不等 socket，写超时独立通知 | automated |
| NET-04 / macOS | 一个连接退出、另一个仍在；最后连接消失 | 活动状态和任务 claim 只清理对应连接；最终断线无残留会话 | automated |
| NET-05 / macOS | USB JPEG 包断裂、坏 CRC、超长长度、包内混入重启与 underrun | 重同步并保留崩溃/欠载事实，坏长度不分配大内存 | automated |

## 后台任务与结果交付

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| TSK-01 / macOS | 帮我查资料后立刻问别的，后台稍后结束 | 前台说明播完即可继续；任务还在后台，不能把 ACK 当成功 | automated |
| TSK-02 / macOS | 结果就绪后唤醒直接问新问题；另一次唤醒不说话 | 新问题优先并改期结果；空输入先简短交付，听完回执才记 delivered | automated |
| TSK-03 / macOS | 后台任务问缺什么，用户先闲聊，再明确补充参数 | 普通闲聊不被误作参数；紧接澄清或明确 handle 的回答才能填入 | automated |
| TSK-04 / macOS | 两个连接同时准备同一任务结果，旧 claim 后到 | 持久 CAS 获胜者才能发；失败者不撤销新 claim | automated |
| TSK-05 / macOS | 长结果先摘要，用户要全文、拒绝或迟到回复 | 摘要和全文回执分开；摘要不等于用户听过全部 | automated |
| TSK-06 / macOS | 运行中取消、服务停止、结果通知丢失、持久库重开 | 结果与 inbox 原子提交；取消不能变成功；重启不盲目重跑效果 | automated |
| TSK-07 / macOS | 磁盘写失败、任务库锁住、三百份任务完成 | 不谎称已保存；Stop/状态不等锁，inbox 有容量边界 | automated |
| TSK-08 / macOS | 私密任务的人离开，来源被忘记，旧固件或关闭新交付 | 来源/私密作用域仍有效才交付；兼容退路不伪造新能力 | automated |

## 时钟、提醒与操作回执

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| REM-01 / macOS | 现在几点；十分钟后提醒我喝水；查提醒；取消同一提醒 | 真实 Brain.turn 经主机执行并持久化，重复意图一次；回执决定回答 | automated |
| REM-02 / macOS | 不要提醒、故事里的提醒、夏令时重复/缺失时刻、多个同名提醒 | 不从引用和否定创造效果；时间或取消目标歧义时拒绝猜测 | automated |
| REM-03 / macOS | 模型伪造主体或 handle，操作提交重试，跨库回执前重启 | 可信 host scope/source 生效；未知效果查证据、不重新执行 | automated |
| REM-04 / macOS | 数据库锁住或能力投影过期后问时间、查状态 | dispatcher 的只读查询不等/写 DB；过期投影不被读成 ready（不声称整个 Brain.turn 无 DB） | automated |
| REM-05 / macOS/firmware | 提醒到点，但静音、安静时段、正在聊天或达到日上限 | 遵守条件；仅提醒可以主动说，播完不开录音、不追问 | automated |
| REM-06 / protocol | TTS 正在准备时取消；已经听完的硬件回执晚到 | 取消后不再取得播放权；迟到事实只结算原尝试，不恢复提醒/录音 | automated |
| REM-07 / macOS | 录音归档过期、主动遗忘或更正提醒来源 | 归档和明确计划分生命周期；主动撤回来源取消计划，私密范围不放宽 | automated |
| REM-08 / macOS | 提醒播放被打断、设备重复领取、过期很久后才到 | 有界重试和 missed 区分；领取/实际播放回执幂等 | automated |

## 身份、家庭记录与隐私

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| MEM-01 / macOS | 匿名人说我叫某某；迟到声纹；私密/混合/过期录音 | 自报不能授予权限；只有有效证据参与归组，私密与未授权录音不混入 | automated |
| MEM-02 / macOS | 在资料和声音组输入妈妈/Dad、辱骂、全角变体和真实名字 | 共享校验在写库前拒绝不合规名；真实名字可用，旧不合规名不作称呼 | automated |
| MEM-03 / macOS | 尚未登记的同一声音多次出现，用户在 App 确认命名 | 匿名组可复用；带来源的过去和未来轮次修订，不凭称呼重放工具 | automated |
| MEM-04 / macOS | 忘掉这次激活，再重启；另一段对话仍保留 | 只撤对应来源与派生内容，未遗忘会话不丢；后台旧作业不能写回来 | automated |
| MEM-05 / macOS | 跨家庭日开始聊天、关闭留存、数据过期但尚未清扫 | 时区日记录与昨天摘要正确；读取不泄露到期或不保留的轮次 | automated |
| MEM-06 / macOS | 模型写了整段故事，但只播出前半；接着追问 | 对话上下文只声称有真实回执的完整语句已听见，生成归档单独保存 | automated |

## 图文、教学与声音

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| MED-01 / macOS | 讲分数和数轴，参数改成零/全份、负数；用户说不要图 | 模板用本轮准确参数，尊重关闭；不修改已冻结语音 | automated |
| MED-02 / macOS/protocol | 用固定点集讲 SVM：原空间、映射、平面、映回边界 | 四段共享冻结数据与点 ID；数值可核算，不声称训练；图像回执门控每段 | automated |
| MED-03 / macOS | 错误分母、非整数刻度、NaN、坏 scene digest、可执行图表代码 | 拒绝失真或未知输入，不执行模型代码，不产出伪装正确的图 | automated |
| MED-04 / macOS | 长中文教学卡超过屏幕；单词拼写；笔顺数据缺失 | 画面有界但完整语义保留；无权威笔顺只给字形并说明不可用 | automated |
| MED-05 / macOS | 附加图片失败、本地文件未经完成工具证明、URL 指向内网 | 非必需图失败不抹掉正文；来源和地址范围受限，读到文件不代表写成功 | automated |
| MED-06 / protocol | 文字和图片分块乱序或来自未完成的旧轮 | 只有完成轮次的素材可取，重组字节完全一致，传输不等于已绘制 | automated |
| MED-07 / macOS | 系统音色有同名限定名，Kokoro 损坏；24k 降到 16k | 歧义音色拒绝；故障走明确回退；PCM 格式正确并抑制混叠（不代表好听） | automated |
| MED-08 / macOS model worker | 错误采样率/声道、截断或超时长 WAV、无效 base64、恶意 JSON 后再发健康查询 | 推理前拒绝坏输入；实际 worker 进程继续应答，不把私密文本写入诊断 | automated |
| MED-09 / macOS model worker | 选择小机器人声音；系统未安装模型或音效失败；生成截断或静音 | 隔离 TTS 环境；只交付完整有效 PCM；失败整段回退并明确标注系统声音，避免每句重复加载失败模型 | automated |
| MED-10 / macOS model supervisor | 普通合成未结束时首句到达；Stop 后马上新发话，旧 Python/音效子进程迟到 | 首句不等待普通通道；停止回收所属进程组；旧 generation 不可启动或交付，新请求收到自身结果 | automated |

## 视觉、眼睛与显示几何

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| VIS-01 / firmware | 两个人、眨眼、目标失踪、陈旧或乱序视觉观测 | 人脸位置与看向 Bot 区分；独立 track，不伪造摄像头外目标或未装电机 | automated |
| VIS-02 / firmware | 眼神平滑，表情动画中途切换，眨眼和愉悦高光 | 从当前形状连续过渡；高光只在合适表情，idle 不抢有效关注目标 | automated |
| VIS-03 / firmware | 摄像头 packed 像素、屏幕 RGB565 行距和方向 | 按真实缓冲布局转换并检查边界；此项不证明摄像头实物方向已验收 | automated |

## 权限、凭据与可解释状态

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| SEC-01 / macOS | 钥匙刷新、配置重载、App 重启；子进程输出个人文本和凭据 | 只从本次会话取 key，重启清空；日志不泄露，轮换有界 | automated |
| SEC-02 / macOS | 让模型打印资料、引用打印句、读取登录网页、shell/edit 请求 | 仅显式意图授权效果；native 操作需要匹配执行证据，继承环境无 key | automated |
| SEC-03 / macOS | 旧配置升级、配对库重开、损坏设备 ID | 保留用户设置与自定义路线，坏登记安全退回待配对 | automated |
| APP-01 / macOS console | 设备无显示回执，处理时长是 NaN，core 可用但未配 managed key | 界面保持待确认/未知，不伪造成功；可用 core 不被错误设置提示挡住 | automated |

## 回归门禁本身

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| REG-01 / test infrastructure | 命令成功却零测试、用例改名、工具缺失、超时及子进程泄漏 | 门禁拒绝误通过；超时回收进程组；缺环境和人工验收明确 not_run | automated |
| REG-02 / build | 源码更新后使用现有本机 ESP-IDF、Tauri 前端工具链构建 | 实际目标可编译；构建是补充检查，不替代功能用例 | automated |

## 独立验收与未完成能力

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| ACC-01 / S31 acoustic | 1/3 米、轻声/正常声、多人电视背景，完整与半句唤醒各至少 20 次 | 记录命中/漏唤醒/误唤醒和首个扬声器写入延迟；不靠阈值推断识别质量 | acceptance |
| ACC-02 / S31 + ASR + model | 请告诉我停顿 0.3/0.8/1.5 秒再接树叶为什么是绿色的，各 5 次 | 用户后半句被完整识别并接题；记录误截断和首音分位数 | acceptance |
| ACC-03 / S31 audio | 播放故事时触摸停止/补一句；可选 AEC 打断单独开关测试 | 触摸立即静音且旧音不复活；AEC 单独记录误打断/回声，不更改默认半双工 | acceptance |
| ACC-04 / Mac + S31 + person | 在扬声器和 Mac 盲听相同中文、数字、中英混读、故事的多音色样本 | 用户选择自然度/亲切度；冷/热首音及 RTF 可复测，再决定默认 TTS | acceptance |
| ACC-05 / Mac + S31 + model | 十轮故事、补话、重新说、停止、再唤醒、追问、晚安 | 每轮接住意图，已听前缀一致，道别后安静；保存匿名轮次和真实回执 | acceptance |
| ACC-06 / Mac + S31 networking | 冷启动 Mac/设备、换端口、Wi-Fi 断连、VPN 路由和恢复 | 发现配对 Mac、认证重连；不把 HTTP 可达当模型推理成功 | acceptance |
| ACC-07 / S31 display/audio | SVM 四步、长中文、图失败、8 秒以上语音、屏幕旋转和 JPEG 争用 | 实屏布局可读且图声依赖正确，无欠载；保留 USB 帧和硬件计数 | acceptance |
| ACC-08 / Mac identity + S31 vision | 成人/儿童、短音频、多人重叠、相似声音和逆光人脸 | 未知保持未知；身份精度/迟到更正和视觉跟随用真实样本单独测 | acceptance |
| ACC-09 / Graphiti adapter | 隔离 Graphiti 后端中写入、跨家庭查询、删除、重建 | SQLite 权威；无跨家庭泄露，删除后派生索引无法召回 | acceptance |
| ACC-10 / printing adapter | 打印提交前后杀进程、接纳后断线、重启核对 CUPS job | 效果 unknown 先核对不重印；已入队和纸张完成分开 | acceptance |
| ACC-11 / Mac + S31 soak | 500 次取消/换轮/断连并持续注入迟到结果，过夜运行 | 资源和控制延迟有界，无旧结果复活；记录堆/句柄/临时文件曲线 | acceptance |
| GAP-01 / macOS responsive control | 完整 RA-01 控制 reducer 在模型/SQL/网络都挂起时继续接纳控制 | 每条关键控制路径无依赖等待，状态/取消/epoch 可检查 | acceptance |
| GAP-02 / macOS capabilities | 自然语言改任务、查询/取消/继续；迟到介绍名字、改名、显式记忆和设备操作 | 统一可信 capability 工作流、补参、回执和恢复；身份不扩权 | acceptance |
| GAP-03 / macOS operations/context | Luna/HTTP 同时报 mutation、50 轮上下文事实更新、跨库所有 crash cut | 胜者一次执行，版本化事实不丢、增量有界、未知效果可恢复 | acceptance |
| GAP-04 / macOS media | 连续动画/视频/检索媒体/Chrome 捕获与模型媒体计划竞速 | 同一冻结语义和 provenance，能力降级与真实媒体回执一致 | acceptance |
| ACC-12 / Mac + S31 BLE | BLE 初始配对、Wi-Fi 凭据写入、断网低带宽回退 | 实际设备协议可恢复，不声称 BLE 支持流式音视频 | acceptance |
| ACC-13 / macOS App | App 设置、启动/停止、录音、家庭管理、声音相册、媒体预览完整操作 | 真实 UI 和服务状态一致，启动环境 key 生命周期正确；保留操作证据 | acceptance |
| DEFER-38 / Home Console | Home Console 远程看画面与家庭留言 | 本轮不开发、不计作已验证 | deferred |
| DEFER-HW / future hardware | 通过头部舵机追到画面外的人、读取 IMU 姿态 | 未安装能力保持不可用，软件不能声称实际转头 | deferred |

## 本机安装与目录迁移

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| OPS-01 / macOS install | 中文数据目录改英文下划线；旧会话、模型环境、素材和配对继续可用；目标冲突、数据库占用或迁移失败 | 使用 Xiaodai_Server；只更新运行路径与会话目录元数据，历史和原数据备份保留；不覆盖已有目录、不启动半迁移数据 | automated |

## 免烧录联调

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| SIM-01 / firmware / macOS gateway | 在 Mac 重放背景声、触摸唤醒、问时间、停止及迟到回复；推进虚拟时钟并输入越界值 | 正式固件保持被动、接纳有授权的网关回答并进入追问；停止后固件拒收旧回答、网关拒绝已关闭会话；脚本越界退出 | automated |
| SIM-02 / firmware / macOS gateway | 慢回答时点「补一句」，把原故事里的小兔换成小狗 | 新模型请求含原问题和补充，已播字段为空。迟到的旧回答被固件拒收，只有新回答获得分段回执。 | automated |
| SIM-03 / firmware / macOS gateway | 模型仍在等待时停止，再次唤醒并换话题 | 模型放行前网关已确认停止。固件关闭收音并拒收旧回答，新会话能独立完成问答。 | automated |
| SIM-04 / firmware / macOS gateway | 查资料的后台任务保持运行，连续问两句星星的话题，再让任务完成 | 第二句承接第一句。两次回答期间任务均在运行，任务只执行一次，完成后结果待交付，保留当前聊天回答。 | automated |
| SIM-05 / firmware / macOS gateway | 点「补一句」后立刻停止，设备尚未开始下一次采音 | 当前补话的直接后继 token 可以关闭会话。跳轮、错误焦点或其他会话的关闭请求被拒绝，迟到的输入不会恢复会话。 | automated |

## 儿童多轮对话

| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |
| --- | --- | --- | --- |
| KID-01 / firmware / macOS gateway | 两位孩子轮流玩四场十一轮的游戏，改口、激将、冒充大人，再回到原话题 | 每轮输入得到同轮非空回答，固件显示并开放追问，期间不产生任务和提醒。模型替身仅验证传输与状态。 | automated |
| KID-02 / 真实 Luna / 语义审阅 | 客厅月球渡口，两个孩子连续对话十一轮 | 逐轮按语义审阅回应、现实能力与安全边界、对当前角色及最新约定的理解，允许不同的创意和措辞。 | acceptance |
| KID-03 / 真实 Luna / 语义审阅 | 被窝电台的假台长，两个孩子连续对话十一轮 | 逐轮按语义审阅回应、现实能力与安全边界、对当前角色及最新约定的理解，允许不同的创意和措辞。 | acceptance |
| KID-04 / 真实 Luna / 语义审阅 | 洗衣篮失踪案，两个孩子连续对话十一轮 | 逐轮按语义审阅回应、现实能力与安全边界、对当前角色及最新约定的理解，允许不同的创意和措辞。 | acceptance |
| KID-05 / 真实 Luna / 语义审阅 | 阳台雨点餐厅，两个孩子连续对话十一轮 | 逐轮按语义审阅回应、现实能力与安全边界、对当前角色及最新约定的理解，允许不同的创意和措辞。 | acceptance |

## 精确执行映射

以下名称必须实际执行成功；用例被删除、改名、跳过或零匹配都会阻止自动场景通过。TLA 模型是额外证据，不能代替实现回归。

- **BOT-01**：`firmware:bot_runtime/test_runtime_near_face`；`tla:firmware/ListeningGate`
- **BOT-02**：`firmware:bot_audio/test_keyword_boundaries`；`tla:firmware/ListeningGate`
- **BOT-03**：`firmware:bot_runtime/test_passive_cycle`；`firmware:bot_runtime/test_ambient_light`；`firmware:bot_runtime/test_runtime_near_face`
- **BOT-04**：`firmware:bot_responses/test_selection`；`firmware:bot_runtime/test_idle_expression`；`server:brain::acks::tests::acks_vary_by_context_and_never_repeat_back_to_back`
- **BOT-05**：`firmware:bot_runtime/test_runtime_near_face`；`tla:protocol/TurnPlayback`
- **BOT-06**：`server:brain::device_volume::tests::requires_device_confirmation_and_never_replays_after_reconnect`；`server:brain::device_volume::tests::refuses_local_settings_and_rejects_invalid_reports`；`firmware:bot_runtime/test_runtime_near_face`
- **VOI-01**：`server:brain::media::tests::a_pause_before_the_last_frame_keeps_the_full_resumed_utterance`；`server:brain::media::tests::resumed_pcm_retires_the_exact_pause_candidate`；`tla:protocol/UtteranceCommit`
- **VOI-02**：`server:brain::gateway::tests::ordered_wss_commit_requires_exact_declared_frames`；`server:brain::gateway::tests::a_pending_commit_cannot_be_replaced_and_disconnect_settles_it`；`tla:macos/DeferredGateway`
- **VOI-03**：`server:brain::gateway::tests::consecutive_empty_input_ends_after_one_retry_but_other_failures_reset_it`；`server:brain::orchestration::tests::punctuation_only_asr_is_not_a_user_utterance`；`firmware:bot_audio/test_followup_vad`
- **VOI-04**：`server:brain::worker::tests::a_busy_speaker_lane_leaves_foreground_asr_available`；`server:brain::worker::tests::background_tts_yields_an_occupied_lane_without_queueing`；`tla:macos/ReplyTiming`
- **VOI-05**：`server:brain::gateway::tests::slow_ack_respects_wake_and_recent_speech`；`tla:macos/ReplyTiming`
- **VOI-06**：`server:brain::fast_core::tests::fake_device_hears_a_six_sentence_story_with_dialogue_without_gaps`；`server:brain::fast_core::tests::over_capacity_reply_keeps_a_contiguous_prefix_and_discloses_the_tail`；`tla:macos/SemanticBuffers`
- **VOI-07**：`server:brain::orchestration::tests::supplement_uses_the_previous_question_and_only_receipted_words`；`server:brain::gateway::tests::runtime_leases_advance_without_reopening_the_conversation`；`tla:protocol/Supplement`
- **VOI-08**：`core:steer_notification_is_buffered_before_ack`；`core:steer_rejected_after_early_output_does_not_leak_into_the_fresh_turn`；`core:an_older_steer_notification_cannot_clear_the_newer_pending`；`tla:macos/SteerHandoff`
- **VOI-09**：`server:brain::gateway::tests::a_turn_cancel_retires_an_inflight_failure_prep`；`server:brain::gateway::tests::delayed_cleanup_revokes_only_the_retired_lease`；`firmware:bot_runtime/test_runtime_near_face`；`tla:macos/CancellationCleanup`；`tla:protocol/TurnPlayback`
- **VOI-10**：`server:brain::gateway::segments::tests::late_or_duplicate_receipts_cannot_advance_the_live_segment`；`server:brain::gateway::segments::tests::a_segment_longer_than_eight_seconds_still_completes`；`server:brain::gateway::tests::playback_receipts_distinguish_device_counts_from_transmitted_and_planned_audio`；`tla:protocol/TurnPlayback`
- **VOI-11**：`server:brain::flow::tests::closing_intents_are_short_goodbyes_only`；`server:brain::flow::tests::mentioning_goodnight_keeps_the_conversation_open`；`server:brain::flow::tests::short_utterances_agree_with_the_conversation_close_twin`；`tla:macos/ConversationClose`
- **VOI-12**：`server:brain::gateway::tests::silent_output_is_explicit_and_caption_respects_utf8_boundary`；`server:brain::voice_lines::tests::failures_map_to_in_character_lines_or_stay_captions`
- **ENG-01**：`core:dual_slow_codex_lets_http_win_and_interrupts_codex`；`core:child_crash_falls_back_to_http_and_marks_codex_unhealthy`；`core:luna_outage_switches_to_http_and_probe_switches_back`
- **ENG-02**：`server:brain::fast_core::tests::a_busy_luna_lane_has_a_deadline_and_semantic_admission_is_bounded`；`tla:macos/LunaAdmission`
- **ENG-03**：`server:harness::turn::tests::actual_route_selection_skips_transport_and_5xx_until_cooldown_expires`；`server:harness::routing::tests::outage_routes_cool_down_but_unknown_failures_are_not_replayed`
- **ENG-04**：`core:flushing_the_last_fragment_cannot_exceed_the_sentence_budget`；`server:brain::fast_core::tests::over_capacity_reply_keeps_a_contiguous_prefix_and_discloses_the_tail`；`tla:macos/SemanticBuffers`
- **ENG-05**：`server:brain::fast_core::tests::tool_tasks_private_scope_and_pending_inputs_stay_on_opencode`；`core:fresh_question_cannot_steer_a_previous_speakers_turn`
- **ENG-06**：`core:one_thread_keeps_context_across_turns_and_interrupts`；`core:a_known_thread_gets_the_delta_and_a_new_thread_the_full_context`；`core:reset_thread_starts_a_new_conversation_thread`
- **NET-01**：`server:brain::gateway::tests::authenticated_tls_session_opens_and_unauthorized_upgrade_fails`；`server:lan::cmd::tests::unapproved_device_never_receives_commands`；`server:lan::tests::duplicate_connection_cannot_replace_live_session`
- **NET-02**：`server:brain::plain_audio::tests::local_tcp_transports_plain_pcm_and_rejects_tampering`；`server:brain::plain_audio::tests::full_output_queue_keeps_the_final_frame_and_end_together`
- **NET-03**：`server:brain::gateway::io::tests::blocked_socket_does_not_block_fence_or_queue_admission`；`server:brain::gateway::io::tests::network_deadline_reports_failure_outside_the_data_queue`；`server:brain::gateway::io::tests::stalled_writer_has_both_frame_and_byte_limits`；`tla:protocol/GatewayDispatch`
- **NET-04**：`server:brain::gateway::tests::one_connection_exiting_leaves_the_other_claim`；`server:brain::gateway::tests::last_peer_drop_clears_activity_on_every_exit`；`server:brain::gateway::tests::disconnect_releases_an_offered_row`
- **NET-05**：`server:usb_debug::tests::bad_crc_resynchronizes`；`server:usb_debug::tests::oversized_length_does_not_allocate`；`server:usb_debug::tests::underrun_is_seen_even_inside_corrupted_jpeg_and_split_reads`；`server:usb_debug::tests::interrupted_packet_recovers_on_reboot`
- **TSK-01**：`server:brain::gateway::segments::tests::a_delegated_explanation_is_a_final_segment_and_does_not_hold_the_next_turn`；`server:brain::tasks::tests::cancelling_one_task_does_not_drop_another_and_foreground_stays_open_until_the_receipt`；`tla:macos/TaskDelivery`
- **TSK-02**：`server:brain::gateway::tests::a_question_defers_a_ready_result_and_ten_turns_stay_open`；`server:brain::gateway::tests::a_silent_turn_speaks_the_result_and_records_it_on_the_receipt`；`tla:macos/TaskDelivery`
- **TSK-03**：`server:brain::orchestration::tests::ordinary_chat_does_not_fill_a_waiting_task`；`server:brain::orchestration::tests::a_journalled_clarification_keeps_chat_on_the_core_and_sends_the_answer_to_the_task`
- **TSK-04**：`server:brain::gateway::tests::a_prepared_lead_cannot_speak_after_another_connection_claims_it`；`server:brain::tasks::tests::a_late_release_cannot_clear_a_newer_claim`；`tla:macos/DeliveryClaim`
- **TSK-05**：`server:brain::tasks::tests::a_summary_can_be_finished_declined_or_aged_out`；`server:brain::tasks::tests::summary_and_full_text_are_different_receipts`；`server:brain::gateway::tests::a_yes_speaks_the_rest_of_a_summary`
- **TSK-06**：`server:brain::tasks::tests::a_cancelled_running_task_cannot_finish_successfully_or_be_replayed`；`server:brain::tasks::tests::a_result_and_its_inbox_commit_together_and_survive_a_lost_notice`；`server:brain::tasks::tests::shutdown_and_crash_recovery_write_a_speakable_row_once`；`tla:macos/TaskDelivery`
- **TSK-07**：`server:brain::tasks::tests::a_full_store_does_not_claim_the_result_was_saved`；`server:brain::gateway::tests::a_locked_task_database_does_not_block_idle_stop_or_status`；`server:brain::tasks::tests::three_hundred_completions_leave_a_bounded_inbox`；`tla:macos/CancellationCleanup`
- **TSK-08**：`server:brain::tasks::tests::a_private_result_expires_when_nobody_can_hear_it`；`server:brain::tasks::tests::forgetting_a_source_erases_the_inbox_copy`；`server:brain::gateway::tests::switch_off_keeps_the_task_on_the_old_delivery_path`
- **REM-01**：`server:brain::capability_dispatch::tests::an_actual_text_turn_answers_clock_without_invoking_a_model`；`server:brain::capability_dispatch::tests::actual_text_commands_create_list_and_cancel_with_durable_receipts`；`tla:macos/CapabilityExecution`
- **REM-02**：`server:brain::capability_dispatch::tests::reminders_require_direct_intent_and_a_host_resolved_unambiguous_time`；`server:brain::capability_dispatch::tests::ambiguous_and_missing_daylight_saving_times_are_not_silently_shifted`；`server:brain::capability_dispatch::tests::two_reminders_require_an_unambiguous_cancel_target`
- **REM-03**：`server:brain::capability_dispatch::tests::the_real_dispatcher_creates_once_rejects_forgery_and_cancels_by_a_visible_handle`；`server:brain::capability_dispatch::tests::unknown_operations_reconcile_domain_evidence_without_resubmission`；`server:memory::assistant_actions::tests::concurrent_intent_retry_has_one_effect_and_survives_reopen`；`tla:macos/CapabilityExecution`
- **REM-04**：`server:brain::capability_dispatch::tests::clock_and_snapshot_queries_do_not_wait_for_or_write_the_database`；`server:brain::capability_registry::tests::revoked_or_expired_projections_never_become_ready_by_reading_them`
- **REM-05**：`server:brain::proactive::tests::disabled_quiet_busy_and_daily_cap_block_reminders`；`server:brain::proactive::tests::no_unscheduled_greeting_or_event_can_be_offered`；`firmware:bot_runtime/test_proactive`；`tla:firmware/ListeningGate`
- **REM-06**：`server:brain::gateway::tests::cancelling_a_reminder_during_tts_fences_the_actual_gateway`；`server:brain::gateway::tests::late_reminder_receipts_record_facts_without_reopening_output`；`tla:macos/CapabilityExecution`
- **REM-07**：`server:memory::assistant_actions::tests::archive_expiry_preserves_explicit_reminders_but_source_forgetting_revokes_them`；`server:memory::assistant_actions::tests::correction_and_private_scope_are_checked_by_the_domain_transaction`
- **REM-08**：`server:memory::proactive::tests::concurrent_reminder_claims_and_real_receipts_are_idempotent`；`server:memory::proactive::tests::reminders_are_retried_once_then_marked_and_journal_updates_in_place`；`server:memory::proactive::tests::stale_reminders_are_missed_but_an_active_playback_can_finish`
- **MEM-01**：`server:memory::store::background_tests::anonymous_derived_fact_does_not_claim_self_reported_identity`；`server:memory::voice_album::tests::excludes_private_mixed_expired_and_before_opt_in_turns`
- **MEM-02**：`server:memory::names::tests::role_words_and_common_offensive_variants_are_rejected_without_rewriting_real_names`；`server:memory::store::background_tests::profile_names_are_validated_before_any_write_and_keep_a_policy_receipt`；`server:memory::voice_album::tests::voice_group_names_share_the_profile_policy_and_legacy_names_are_not_spoken`；`tla:macos/NameAdmission`
- **MEM-03**：`server:memory::voice_album::tests::discovers_without_enrollment_and_names_past_and_future_turns`
- **MEM-04**：`server:memory::store::background_tests::forgetting_one_activation_preserves_another_after_reopen`；`server:memory::store::background_tests::grounded_memories_stay_unattributed_and_forgotten_jobs_cannot_return`
- **MEM-05**：`server:memory::store::background_tests::first_turn_of_a_day_gets_a_short_summary_of_the_previous_day`；`server:memory::store::background_tests::recent_context_hides_expired_or_unretained_turns_without_a_purge`
- **MEM-06**：`server:memory::store::background_tests::voice_context_uses_only_completed_speech_and_preserves_the_generated_archive`；`tla:protocol/Supplement`
- **MED-01**：`server:brain::teaching_visuals::tests::local_templates_use_exact_current_parameters_and_respect_opt_out`；`python:test_media_regression.MediaRegression.test_fraction_zero_full_and_number_line_negative_bounds`
- **MED-02**：`server:brain::teaching_visuals::tests::the_svm_example_keeps_point_identity_and_freezes_all_four_frame_gates`；`python:test_teaching_media.TeachingMediaTests.test_mapping_plane_kernel_and_classification_share_one_scene`；`python:test_teaching_media.TeachingMediaTests.test_actual_renderer_has_four_bounded_frames_and_frozen_semantics`；`tla:protocol/TurnPlayback`
- **MED-03**：`python:test_media_regression.MediaRegression.test_invalid_math_and_executable_diagram_never_render`；`python:test_teaching_media.TeachingMediaTests.test_unknown_stage_or_wrong_scene_digest_cannot_render`
- **MED-04**：`python:test_media_regression.MediaRegression.test_long_chinese_keeps_full_semantics_when_screen_is_truncated`；`python:test_media_regression.MediaRegression.test_spelling_and_untrusted_text_boundaries`；`python:test_media_regression.MediaRegression.test_missing_stroke_data_is_explicit_not_invented_stroke_order`
- **MED-05**：`server:brain::display::tests::a_failed_extra_illustration_does_not_fail_a_text_answer`；`server:brain::display::tests::local_images_require_a_matching_completed_tool_result`；`server:brain::display::tests::image_hosts_and_local_addresses_are_bounded`；`server:brain::display::tests::a_read_result_does_not_prove_a_write`
- **MED-06**：`server:brain::gateway::tests::image_chunks_require_the_completed_turn_and_reassemble_exactly`；`console:console.delivery_requires_device_evidence`
- **MED-07**：`python:test_speech.SpeechTests.test_ambiguous_short_name_is_rejected_before_synthesis`；`python:test_speech.SpeechTests.test_installed_short_name_synthesizes_real_pcm`；`python:test_speech.KokoroTests.test_a_broken_kokoro_model_falls_back_to_say`；`python:test_speech.KokoroTests.test_resampling_keeps_speech_band_and_removes_aliases`
- **MED-08**：`python:test_worker_contracts.WorkerContracts.test_complete_pcm_and_exact_duration_boundary`；`python:test_worker_contracts.WorkerContracts.test_wrong_format_and_truncated_audio_fail_before_inference`；`python:test_worker_contracts.WorkerContracts.test_base64_is_bounded_strict_and_not_a_data_url`；`python:test_worker_contracts.WorkerContracts.test_actual_worker_recovers_after_bad_requests_without_echoing_private_text`
- **VIS-01**：`firmware:vision_attention/test_vision_attention`；`firmware:attention/test_attention`；`firmware:bot_runtime/test_near_face`
- **VIS-02**：`firmware:face/test_eve_pose`；`firmware:face/test_eve_gaze`；`firmware:face/test_gaze_smoothing`；`firmware:attention/test_idle_attention`
- **VIS-03**：`firmware:bot_vision/test_packed_pixels`；`firmware:bot_platform/test_rgb565_frame`
- **SEC-01**：`server:brain::key_lifecycle_tests::key_survives_config_reload_but_not_an_app_restart`；`server:harness::secrets::tests::session_keys_are_isolated_and_invalid_replacement_keeps_the_original`；`server:diagnostics::tests::child_free_text_and_credentials_never_reach_the_log`；`server:diagnostics::tests::rotation_is_bounded_and_files_remain_private`
- **SEC-02**：`server:brain::orchestration::tests::a_printable_artifact_or_negated_quote_does_not_authorize_printing`；`server:harness::config::tests::logged_in_browser_reads_require_approval_even_with_legacy_settings`；`server:harness::turn::tests::native_edits_and_shell_require_post_hook_evidence`；`server:harness::turn::tests::shell_environment_has_no_provider_or_server_credentials`
- **SEC-03**：`server:brain::settings::tests::upgrading_routes_preserves_omitted_settings_on_disk`；`server:pairing::tests::state_survives_restart`；`server:pairing::tests::corrupt_registry_starts_empty_not_panicking`；`server:identity::tests::regenerates_invalid_stored_id`
- **APP-01**：`console:console.delivery_requires_device_evidence`；`console:console.metrics_reject_unknown_and_nonfinite_values`；`console:console.setup_respects_available_core_and_harness`；`console:console.brain_summary_does_not_invent_success_or_latency`；`console:console.name_errors_do_not_claim_a_name_was_saved`
- **REG-01**：`suite:test_zero_cases_and_nonzero_exit_fail`；`suite:test_timeout_kills_owned_child`；`suite:test_missing_executable_is_not_run`；`suite:test_missing_or_renamed_case_cannot_pass`；`suite:test_acceptance_never_inherits_a_local_pass`
- **REG-02**：`console-build:build`；`idf-build:build`
- **ACC-01**：not_run：待 S31；使用同增益同距离，保留真实录音和日志，MultiNet 不是专用 WakeNet
- **ACC-02**：not_run：待实板和真实模型，主机帧回归不能证明声学效果
- **ACC-03**：not_run：待实板，AEC 默认关闭
- **ACC-04**：not_run：Serena/Vivian/小机器人本机试听、PCM 与 ASR 检查已有记录，真人偏好和 S31 听感/并发首音仍待验收；见 docs/research/tts-local-audition-2026-11-28.md
- **ACC-05**：not_run：完整真人/模型/设备联合验收未运行
- **ACC-06**：not_run：需同一板运行时间与认证记录；默认不接触真实供应商
- **ACC-07**：not_run：主机渲染不是实屏验收
- **ACC-08**：not_run：真实声纹/人脸模型与家庭场景需实测
- **ACC-09**：not_run：仅 opt-in test_graphiti，需 disposable 实际数据库和模型，不用家庭生产库
- **ACC-10**：not_run：实际 CUPS/打印机回执和故障注入未运行
- **ACC-11**：not_run：定向有界测试已接入，但不能代替长时整机稳定性
- **GAP-01**：not_run：#36 仍有同步存储路径；ResponsiveControl 孪生与完整实现尚缺
- **GAP-02**：not_run：#36 当前只覆盖时钟/提醒等部分 adapter；完整 task/identity/memory/device 工作流未完成
- **GAP-03**：not_run：#36 全局 winner 提案/上下文投影/迁移恢复矩阵未完成；现有提醒和任务测试仅覆盖所列子集
- **GAP-04**：not_run：#31 固定教学模板之外的完整媒体流程未完成
- **ACC-12**：not_run：需要 BLE 与 provisioning 实物验收，不能用 LAN pairing 单测代替
- **ACC-13**：not_run：本次默认仅测 presenter 与构建；GUI 需按手工脚本独立执行
- **DEFER-38**：用户明确仍暂缓 issue #38
- **DEFER-HW**：当前 V1 无舵机/IMU，待真实硬件安装后加入对应自动及实板用例
- **MED-09**：`python:test_qwen_robot.RobotTests.test_real_effect_keeps_bounded_pcm_and_changes_pitch_without_clipping`；`python:test_qwen_robot.RobotTests.test_invalid_chunks_and_token_exhaustion_never_return_partial_audio`；`python:test_qwen_robot.RobotTests.test_effect_missing_and_failed_are_explicit`；`python:test_qwen_robot.RobotTests.test_worker_rejects_invalid_input_before_model_loading`；`python:test_qwen_robot.RobotTests.test_missing_model_uses_real_say_and_reports_fallback_without_retry_storm`；`python:test_qwen_robot.RobotTests.test_generation_failure_falls_back_with_the_complete_original_text`；`server:brain::worker::tests::robot_selection_uses_isolated_python_and_health_without_changing_asr`；`console:console.robot_voice_fallback_is_not_mislabeled_as_robot`；`python:test_qwen_robot.RobotTests.test_corrupt_weights_are_rejected_before_upstream_model_load`；`tla:macos/LocalSpeech`
- **MED-10**：`server:brain::worker::tests::robot_stop_kills_owned_descendants_and_fences_old_generation`；`tla:macos/LocalSpeech`；`tla:macos/ReplyTiming`
- **OPS-01**：`server:data_root::tests::fresh_install_uses_private_english_directory_without_spaces`；`server:data_root::tests::existing_chinese_or_legacy_data_cannot_be_replaced_by_empty_install`；`server:data_root::tests::incomplete_or_symbolic_destination_cannot_start`；`python:test_relocate_data_root.RelocationTests.test_relocation_preserves_history_ids_assets_and_real_python_launcher`；`python:test_relocate_data_root.RelocationTests.test_existing_destination_and_malformed_config_do_not_change_source`；`python:test_relocate_data_root.RelocationTests.test_failed_rewrite_keeps_original_and_fences_incomplete_destination`；`python:test_relocate_data_root.RelocationTests.test_open_database_and_source_symlink_are_rejected`；`core:config::tests::default_workspace_uses_english_root_and_preserves_overrides`；`core:health::tests::log_round_trips_and_seeds_the_window`
- **SIM-01**：`server:brain::gateway::tests::simulator::host_bot_completes_clock_turn_and_rejects_late_reply`；`server:brain::gateway::tests::simulator::host_bot_script_handles_time_boundaries_and_invalid_commands`；`tla:protocol/TurnPlayback`
- **SIM-02**：`server:brain::gateway::tests::simulator::slow_supplement_keeps_question_and_retires_unheard_answer`；`tla:protocol/Supplement`；`tla:protocol/TurnPlayback`
- **SIM-03**：`server:brain::gateway::tests::simulator::slow_stop_is_acknowledged_before_provider_finishes_and_next_wake_works`；`tla:macos/CancellationCleanup`；`tla:protocol/TurnPlayback`
- **SIM-04**：`server:brain::gateway::tests::simulator::running_background_task_allows_two_chat_turns_and_retains_its_result`；`tla:macos/TaskDelivery`
- **SIM-05**：`server:brain::gateway::tests::simulator::stop_between_supplement_and_capture_closes_only_the_reserved_turn`；`tla:protocol/Supplement`；`tla:macos/CancellationCleanup`
- **KID-01**：`server:brain::gateway::tests::simulator::child_dialogues::all_child_turns_are_answered_and_reach_the_runtime`；`tla:protocol/TurnPlayback`
- **KID-02**：默认离线回归不调用真实模型；显式运行 ./tools/sim-bot children 后逐轮核对该次实答。
- **KID-03**：默认离线回归不调用真实模型；显式运行 ./tools/sim-bot children 后逐轮核对该次实答。
- **KID-04**：默认离线回归不调用真实模型；显式运行 ./tools/sim-bot children 后逐轮核对该次实答。
- **KID-05**：默认离线回归不调用真实模型；显式运行 ./tools/sim-bot children 后逐轮核对该次实答。
