# Mac 安全设备网关：配置、凭据与固件接线

**2026-11-06 源码交付，未构建、未测试、未运行模型、未连接硬件。** Mac 的 WSS v3
服务端、证书准备脚本和本地凭据管理已写入；当前 ESP32-S31-Korvo-1 V1.1 固件尚未
有 WSS v3 客户端。下面是为后续 Mac 安装和固件接线准备的操作说明，不表示这些命令
本轮已经执行。字段、帧布局和错误规则详见 [mac-brain-v3.md](../shared/mac-brain-v3.md)。

## 1. 四条边界先分清

| 入口 | 用途 | 当前状态 |
| --- | --- | --- |
| 原有 LAN v1 / 软配对 | 发现、诊断、原有设备管理 | 独立保留；没有认证加密，不能承载私密媒体或授权行为 |
| `wss://Mac:7443/v3/brain/ws` | 明确激活后的设备文本、PCM、语义卡片和音频回复 | Mac 服务端源码已实现；默认关闭；板端客户端待接 |
| 本地 Tauri `companion_request` | 家长配置、档案、记忆、模型、权限和设备凭据 | 中文 Mac 控制台通过此入口调用 Rust；没有映射到设备 WSS |
| 本地模型进程 JSONL | ASR、声纹、人脸 embedding、macOS TTS | Rust 监督子进程，无局域网 HTTP 监听；模型需单独准备 |

LAN 的“看见设备”或“软配对通过”不等于 WSS token 和证书信任建立。声纹或人脸
候选同样不用于认证。设备继续决定何时唤醒、看向谁、显示什么和何时停止动作。

相关实现为 [gateway.rs](../server/bot-server/src-tauri/src/brain/gateway.rs)、
[credentials.rs](../server/bot-server/src-tauri/src/brain/credentials.rs) 和
[家长 RPC](../server/bot-server/src-tauri/src/brain/rpc.rs)。整体安装与模型说明分别见
[mac-bot-server.md](mac-bot-server.md)、[mac-local-models.md](mac-local-models.md)。

## 2. 准备 Mac 证书

为实际连接使用的 Mac 主机名或 IP 创建证书。下面从 `server/bot-server` 目录执行，
`xiaodai-mac.local` 和 `192.168.1.20` 是占位示例，必须替换成实际地址；只使用主机名时
可省略 `--ip`，每种参数可重复。目标目录尚未有证书/私钥时才会生成。

```bash
python3 scripts/create_gateway_certificate.py \
  --directory "$HOME/Library/Application Support/ai.reasonia.espbot.server/tls" \
  --dns xiaodai-mac.local \
  --ip 192.168.1.20
```

[create_gateway_certificate.py](../server/bot-server/scripts/create_gateway_certificate.py)
复用本机 OpenSSL，生成 P-256 密钥、自签名 SHA-256 证书、实际 DNS/IP 的 SAN、
365 天有效期，并打印证书 DER 的 SHA-256 指纹。它创建目录权限 `0700`，将
`gateway-key.pem` 和 `gateway-cert.pem` 设为 `0600`，保留已有文件并拒绝覆盖。
需要轮换时使用新目录，再显式更新配置和设备信任材料。

**公有证书给设备，私钥留在 Mac。** 初始化时应通过可信渠道核对证书指纹，并将
公有证书/信任材料写入未来的设备配置；连接 URL 的主机名或 IP 必须匹配 SAN。
这份自签名证书是供显式信任使用的材料，脚本不会把它安装到 macOS 或设备信任库。
Mac 的服务端 TLS 不要求客户端证书，设备身份另由专属 Bearer 凭据验证。

脚本不配置 Wi-Fi、BLE、NVS，不签发设备 token，也不启动服务。项目计划的 Mac BLE
配对和安全 Wi-Fi 配置仍需后续接线；目前没有自动把公有证书/token 送进板子的流程。
不能通过关闭设备 TLS 校验补齐这一步。若使用已有正式证书，也可配置 PEM 证书链和
对应私钥，无需运行生成脚本。

## 3. 配置 `brain.json`

Mac 配置位于：

```text
~/Library/Application Support/ai.reasonia.espbot.server/brain.json
```

首次默认 `gateway.enabled=false`，默认监听地址是 `0.0.0.0:7443`，最大连接数为 4。
启用后必须提供可读证书和私钥。把以下 gateway 节合并到现有配置，保留 harness、
models、memory 等其他配置；不要把这个片段当作替换全部配置的操作。

```json
{
  "gateway": {
    "enabled": true,
    "bind": "0.0.0.0:7443",
    "certificate_pem": "/Users/YOUR_ACCOUNT/Library/Application Support/ai.reasonia.espbot.server/tls/gateway-cert.pem",
    "private_key_pem": "/Users/YOUR_ACCOUNT/Library/Application Support/ai.reasonia.espbot.server/tls/gateway-key.pem",
    "max_connections": 4
  }
}
```

将 `/Users/YOUR_ACCOUNT` 替换为实际绝对路径。JSON 中的 `~` 或 `$HOME` 不会像 shell
一样自动展开。`bind` 需要 IP:port SocketAddr；可改成需要的本机网卡 IP，而非随意
填写 DNS 名称。TLS 主机名验证针对设备连接 URL 和证书 SAN，与监听通配地址不同。

Unix/macOS 上私钥只要存在任何组或其他用户权限位，启动就会报
`gateway_private_key_requires_mode_0600`；生成脚本已按 `0600` 写入。证书读取失败、
证书和密钥不匹配、地址已占用都会使网关启动失败，不会静默改用明文端口。

通过家长控制台保存完整配置对应 `config.save`，返回 `reload_required: true`。
随后使用 `service.reload` 重新载入，按控制台启动服务；`service.start` 启动 harness
及配置启用的网关，`service.status` 可查看状态，`service.stop` 取消并停止服务。
自动启动由顶层 `auto_start` 控制，默认关闭。配置定义见
[settings.rs](../server/bot-server/src-tauri/src/brain/settings.rs)。

网关不自动修改防火墙、不配置端口映射，也没有新增自动发现广播。实际设备地址、
局域网可达性与证书信任，需要后续安装和固件接线时配置。

## 4. 家长签发、轮换与撤销设备凭据

中文控制台的设备凭据区域可为设备 ID 签发 token；同一 ID 再签发即轮换，旧 token
立即失效。新 token 只在当前窗口显示一次，不写浏览器存储，也不作为遥测事件广播。
忘记 token 时签发新的，不从服务器取回旧明文。

对应本地 RPC 如下。这里的 method/payload 是 `companion_request` 参数，
**不是**发往 `/v3/brain/ws` 的方法：

| method | payload | 返回 / 效果 |
| --- | --- | --- |
| `device.credentials.list` | `{}` | `device_id` 与 `created_at_ms` 列表，无 token 或摘要 |
| `device.credentials.issue` | `{"device_id":"xiaodai-001"}` | 新凭据，或轮换该设备现有凭据 |
| `device.credentials.revoke` | `{"device_id":"xiaodai-001"}` | 删除凭据记录并通知在线连接；`{"revoked":true}` |

签发返回结构：

```json
{
  "device_id":"xiaodai-001",
  "token":"<仅本次返回的 64 字符 token>",
  "transport":"wss",
  "requires_certificate_trust":true
}
```

设备 ID 限制为 1–64 个 ASCII 字母、数字、`_`、`-`；最多保存 64 台设备凭据。
存储文件是应用配置目录下的 `runtime/devices/secure-devices.json`，持久化内容只有
token 的 SHA-256 摘要与创建时间，采用私有目录/文件权限。它与旧软配对记录独立。

未来的固件初始化流程应接收至少这四项：设备 ID、token、Mac WSS URL、可信证书。
公有证书可以公开核对，token 必须作为设备秘密保存；不要把 token 放进 URL、日志、
公开仓库或 LAN hello。握手应发送：

```http
Authorization: Bearer <新 token>
X-Espbot-Device: xiaodai-001
```

已有连接也会响应凭据撤销/轮换并断开。重新连接不会恢复旧会话或旧音频，设备需要
新的有效凭据和真实明确激活。旧 token 被撤销只影响设备访问，不删除对话或个人档案。

## 5. 设备接线采用哪条输入路径

设备当前只有本地唤醒/VAD 等能力，不应宣称已有完整 ASR。后续 WSS 适配可复用设备
现有音频单一所有者，在完整“小呆小呆”或明确触摸后接出 PCM consumer：

1. 带运行时 `epoch/session_id/turn_id/focus_generation` 发送 `session.open`。
2. 发送 `audio.begin`，随后按 XDAI 20 字节头发送 16 kHz 单声道 PCM16LE。
3. 一次明确输入结束时发送 `audio.commit`，最多 30 秒；Mac 在本地 ASR 后调用 harness。
4. 先处理最终回复的文字、卡片、表情语义，再按顶层 `audio` 描述接收二进制回复。
5. 按当前 runtime 和 stream 拒绝旧内容；本地播放缓冲耗尽后才视为真正播完。

如果未来设备已经有自己的 ASR，直接用 `assistant.text` 提交文本，跳过 Mac ASR。
两条路径共享相同会话、记忆过滤和模型路由。Mac 原始音频只在内存处理，ASR 文本
会进入配置的模型回合；是否保留本地对话文本由记忆策略决定。身份 embedding 不会
作为模型提示词发给 OpenCode，候选匹配也不自动选中某个孩子的个人档案。

模型缺失时返回实际错误/警告，不自动下载权重，也不改用在线 ASR。准备本地模型的
显式操作见 [mac-local-models.md](mac-local-models.md)。短于 3 秒的语音可以有可用
ASR 文本，但不满足当前声纹提取的最短输入要求；不要因此假装识别到了说话人。

当前 v3 没有图像输入。Mac 控制台可以明确点击拍照或选文件做本地人脸注册/匹配，
但设备相机帧不会走此协议，也没有后台上传接口。屏幕 `image`/`web` 卡片是受限
HTTPS URL 和解释文字，不是可执行 HTML；只有家长批准的 `conversation.media_hosts`
会保留远程媒体 URL。默认空名单会让这些卡片降级为文字。

## 6. 打断、停止、失效与删除是不同操作

| 操作 | 实际影响 | 不代表什么 |
| --- | --- | --- |
| 设备 `assistant.cancel` | 取消本连接采集、当前回合及音频下行，保留会话 | 不删除保存的文字/记忆 |
| 设备 `session.close` | 同时结束激活会话，下次需明确唤醒/触摸 | 不删除 harness 会话或长期记忆 |
| 家长 `assistant.cancel` | 可按字符串 turn_id 取消；未指定时全局取消并清除设备激活会话 | 不自动撤销设备凭据 |
| `service.stop` | 停止接收新回合、取消活动流程并关闭服务 | 不把配置恢复为默认值 |
| `device.credentials.revoke` | 阻止设备继续访问，并使现有连接断开 | 不删除档案、声纹、人脸或对话 |
| `conversation.forget` | 按 conversation_id 删除本地 harness 会话及记忆库的对应对话 | 不等于供应商侧所有数据的删除接口 |
| `conversation.forget_all` | 清理服务维护的全部会话记录与对应 harness 会话 | 不等同于撤销所有身份模板 |

`conversation.forget` 使用 `{"conversation_id":"<已有会话 ID>"}`，其他全量删除
使用对应控制台操作。删除时先取消执行并取得回合门，避免旧会话继续使用刚删除的
上下文；上游删除失败会明确报错，不会假装已全部遗忘。档案、身份、记忆的删除规则
及云可见标志见 [mac-memory.md](mac-memory.md) 和
[rpc.rs](../server/bot-server/src-tauri/src/brain/rpc.rs)。这些能力都没有暴露给设备 WSS。

孩子打断时，设备应先停自己的扬声器、清空旧缓冲，再发 cancel；不能等待一个网络
往返才停止。`audio.complete` 只表示服务器发送完成，不能用来宣称“孩子已经听完”。
掉线、撤销、session 失效和全局取消也应使设备清空待播放内容，不在重连后继续旧句子。

## 7. 资源限制与排错

默认最多 4 条 WSS 连接，但 Brain 只执行一个回合，其他回合可能收到 `brain_busy`。
每条连接的文本/音频采集/回复音频也按半双工安排；需要新输入时应先取消已有播放。
当前不是全双工声学打断系统，没有声学回声消除或连续流式识别承诺。

| 症状 / 错误 | 对应检查 |
| --- | --- |
| `gateway_requires_tls_certificate_and_key` | enabled 时同时提供证书、私钥路径 |
| `gateway_private_key_requires_mode_0600` | 私钥不可有组/其他用户权限；按脚本权限准备 |
| `gateway_bind_failed` | 配置的本机地址/端口是否可绑定，是否被占用 |
| TLS 握手失败 | 设备信任、公有证书、SAN 主机名/IP、证书有效期与设备时钟 |
| `401 device_credential_required` | 精确设备 ID、`Bearer ` 前缀、当前有效 token |
| `403 device_api_only` | 设备接口拒绝带 Origin 的浏览器请求；家长功能应走本地 Tauri |
| `explicit_session_required` | 先依据真实唤醒/触摸打开会话，LAN 连接不算激活 |
| `runtime_turn_stale` | 已提交的回合号被消耗，失败/取消后也需递增 |
| `audio_sequence_mismatch` | 丢弃当前采集，按新 begin 从 sequence 0 开始；不补传旧流 |
| 有文本没有语音 | 看顶层 audio 和 warnings，确认本机 TTS 声音及模型进程配置 |

TLS 和 WS 升级各限 10 秒；WS Ping 周期 15 秒，无上行消息超过 50 秒后在心跳检查
断开；所有 socket 写入限 3 秒，二进制音频另限 2 秒。上行最多 30 秒，采集墙钟超过
35 秒后清理；下行 PCM 最多 120 秒。会话默认空闲 300 秒失效。完整字节边界、检查
时机与恢复规则以 [v3 协议](../shared/mac-brain-v3.md) 为准，均为源码设定，尚无设备
性能实测数据。

## 8. 固件下一步的具体边界

现有 [brain_link README](../firmware/components/brain_link/README.md) 明确：旧 LAN
未认证且未加密，收到远程请求返回 `AUTH_REQUIRED`。主程序没有安装已认证的媒体
consumer，所以没有实际上传原始麦克风、相机、面部轨迹或对话。启动 Mac WSS 不会
改变这个状态，也不能把旧 LAN 的 authenticated 标志直接改成 true 来接通。

后续工作是给现有 ESP32-S31 运行时接入可信配置存储、原生 WSS、runtime token、
唤醒后的有界 PCM 发送、音频播放与取消，以及语义卡片渲染。设备屏幕/声音能力需要
按真实固件逐项接线，模型/摄像头/扬声器的效果需要硬件到货后再确认。Mac 层的人脸
识别、本地语音和设备协议已经分离；今后增添相机输入时，应先扩展版本化契约与显式
采集授权，再建立对应媒体通道。
