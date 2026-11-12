# Mac 安全设备网关：配置、凭据与固件接线

数据目录为 `~/小呆 Server`；Mac 和 ESP32-S31 固件均已接入 v3。设备联网、
无线握手及完整语音对话的实际验证状态以 [本次整合记录](server-integration-2026-11-08.md)
为准。字段、帧布局和错误规则详见 [mac-brain-v3.md](../shared/mac-brain-v3.md)。

## 1. 四条边界先分清

| 入口 | 用途 | 当前状态 |
| --- | --- | --- |
| 原有 LAN v1 / 软配对 | 发现、诊断、原有设备管理 | 独立保留；没有认证加密，不能承载私密媒体或授权行为 |
| Bonjour `_espbot-brain._tcp` 公布的 WSS 入口 | 设备认证、会话控制、文本和语义卡片 | 小呆查询当前地址及端口，主动发起 TCP/TLS 连接，Mac 在已建立的连接上回送结果 |
| 按回合临时开放的局域网 TCP 端口 | 明确激活后的 PCM 语音上行和下行 | 当前固件使用明文音频，帧带认证校验但不保密 |
| 本地 Tauri `companion_request` | 家长配置、档案、记忆、模型、权限和设备凭据 | 中文 Mac 控制台通过此入口调用 Rust；没有映射到设备 WSS |
| 本地模型进程 JSONL | ASR、声纹、人脸 embedding、macOS TTS | Rust 监督子进程，无局域网 HTTP 监听；模型需单独准备 |

LAN 的“看见设备”或“软配对通过”不等于 WSS token 和证书信任建立。声纹或人脸
候选同样不用于认证。设备继续决定何时唤醒、看向谁、显示什么和何时停止动作。

相关实现为 [gateway.rs](../server/bot-server/src-tauri/src/brain/gateway.rs)、
[credentials.rs](../server/bot-server/src-tauri/src/brain/credentials.rs) 和
[家长 RPC](../server/bot-server/src-tauri/src/brain/rpc.rs)。整体安装与模型说明分别见
[mac-bot-server.md](mac-bot-server.md)、[mac-local-models.md](mac-local-models.md)。

## 2. 准备 Mac 证书

首次配对可退出 Mac App，从 `server/bot-server` 目录运行以下命令。脚本用这台 Server
的持久 ID 生成稳定的证书名称。已有证书的安装沿用证书中的名称或 IP 作为 TLS 身份。
用实际串口和设备 ID 替换示例值。已有配对的设备升级固件后直接读取原有信任材料。

```bash
python3 scripts/provision_usb_gateway.py --port /dev/cu.usbserial-210 --device-id korvo-01
```

[create_gateway_certificate.py](../server/bot-server/scripts/create_gateway_certificate.py)
复用本机 OpenSSL，生成独立的 P-256 本地 CA 和由其签署的服务端证书。服务端证书
包含实际 DNS/IP 的 SAN，有效期 365 天；脚本打印公有 CA 证书的 SHA-256 指纹，
将目录设为 `0700`、文件设为 `0600`，保留已有文件并拒绝覆盖。需要轮换时使用
新目录，再显式更新配置和设备信任材料。

**公有 CA 证书给设备，两个私钥留在 Mac。** 初始化时应通过可信渠道核对 CA 指纹；
固件用配对时保存的 TLS 身份核对证书 SAN。
脚本不会把 CA 安装到 macOS 系统信任库。
Mac 的服务端 TLS 不要求客户端证书，设备身份另由专属 Bearer 凭据验证。

生成证书的脚本不配置设备。已有
[`provision_usb_gateway.py`](../server/bot-server/scripts/provision_usb_gateway.py)
可在退出 Server 和断开 USB 调试后，将 CA、设备专属 token 和证书身份通过本机 USB 写入
开发板；重新打开 App 才会载入新 token。它不配置 Wi-Fi 或 BLE。若使用其他证书，仍须
配置 PEM 证书链、对应私钥和设备可信 CA，不能关闭设备 TLS 校验。
Mac 监听成功后发布服务类型、实际端口和公有 CA 指纹。每次连接或重连前，小呆通过
Espressif mDNS 查询同一局域网中与配对 CA 匹配的记录，再把解析出的地址、端口交给
官方 WebSocket 客户端。只有 TLS 证书验证通过后才发送设备凭据。Mac 从通过认证的连接
取得设备 ID。任一方的 DHCP 地址变化后按同样流程重连。

## 3. 配置 `brain.json`

Mac 配置位于：

```text
~/小呆 Server/brain.json
```

首次默认 `gateway.enabled=false`，最大连接数为 4。USB 配对后监听 `0.0.0.0:7443`，
允许私有、链路本地及回环 IPv4 对端。也可使用端口 0 让系统分配，实际端口通过 Bonjour 公布。
启用后必须提供可读证书和私钥。把以下 gateway 节合并到现有配置，保留 harness、
models、memory 等其他配置；不要把这个片段当作替换全部配置的操作。

```json
{
  "gateway": {
    "enabled": true,
    "bind": "0.0.0.0:7443",
    "certificate_pem": "/Users/YOUR_ACCOUNT/小呆 Server/tls/gateway-cert.pem",
    "private_key_pem": "/Users/YOUR_ACCOUNT/小呆 Server/tls/gateway-key.pem",
    "max_connections": 4
  }
}
```

将示例账户替换为实际路径。JSON 中的 `~` 或 `$HOME` 不会像 shell 一样自动展开。
Bonjour 默认读取证书同目录的 `gateway-ca.pem`，其他位置可在 `pairing_ca_pem` 指定。

Unix/macOS 上私钥只要存在任何组或其他用户权限位，启动就会报
`gateway_private_key_requires_mode_0600`；生成脚本已按 `0600` 写入。证书读取失败、
证书和密钥不匹配、地址已占用都会使网关启动失败，不会静默改用明文端口。

通过家长控制台保存完整配置对应 `config.save`，返回 `reload_required: true`。
随后使用 `service.reload` 重新载入，按控制台启动服务；`service.start` 启动 harness
及配置启用的网关，`service.status` 可查看状态，`service.stop` 取消并停止服务。
自动启动由顶层 `auto_start` 控制，默认关闭。配置定义见
[settings.rs](../server/bot-server/src-tauri/src/brain/settings.rs)。

网关开始监听后在局域网上公布实际地址和端口。设备与 Mac 只需在局域网允许 mDNS
和相应 TCP 连接时完成一次可信配对，之后即可按服务自动发现。网关无需改动系统路由。

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

固件的 USB 配对接收 token、Mac WSS URL 和可信 CA；设备 ID 来自固件配置。
公有证书可以公开核对，token 必须作为设备秘密保存；不要把 token 放进 URL、日志、
公开仓库或 LAN hello。握手应发送：

```http
Authorization: Bearer <新 token>
X-Espbot-Device: xiaodai-001
```

已有连接也会响应凭据撤销/轮换并断开。重新连接不会恢复旧会话或旧音频，设备需要
新的有效凭据和真实明确激活。旧 token 被撤销只影响设备访问，不删除对话或个人档案。

## 5. 设备接线采用哪条输入路径

设备复用现有音频单一所有者，仅在明确触摸或完整“小呆小呆”唤醒后接出 PCM；
专用唤醒词声学模型尚未配置，以下设备路径尚待无线实机验证：

1. 带运行时 `epoch/session_id/turn_id/focus_generation` 发送 `session.open`。
2. 发送 `audio.begin` 并协商临时 TCP 连接，随后按 XDAI 20 字节头发送明文 16 kHz 单声道 PCM16LE。
3. 一次明确输入结束时在 WSS 发送带帧数的 `audio.commit`，最多 30 秒；Mac 在本地 ASR 后调用 harness。
4. 先处理最终回复的文字、卡片、表情语义，再按顶层 `audio` 描述接收二进制回复。
5. 按当前 runtime 和 stream 拒绝旧内容；本地播放缓冲耗尽后才视为真正播完。

本次实板网络探测发现无线链路在负载下丢包；固件按乐鑫排障建议关闭 Wi-Fi 聚合与省电，
并优先使用 PSRAM 分配 Wi-Fi/lwIP 可迁移的内存。本次实板语音和网络测试结果见
[整合记录](server-integration-2026-11-08.md)，长期稳定性仍需观察。明确交互期间诊断连接
暂时暂停，控制连接恢复且交互结束后自动恢复。

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

现有 [brain_link README](../firmware/components/brain_link/README.md) 说明：
LAN 诊断入口未认证且未加密，收到远程请求返回 `AUTH_REQUIRED`；设备只在认证 WSS
会话明确激活后，经独立 TCP 连接发送明文 PCM，局域网抓包可以读取。还需要完成语音
无线实机验收、语义卡片显示、真实
扬声器、专用唤醒词模型和 BLE 配网。设备相机目前不上传；增加相机输入前应扩展
版本化契约与明确采集授权。
