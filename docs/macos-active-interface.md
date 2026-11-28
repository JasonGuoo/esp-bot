# macOS 备忘录：找到活动网卡，验证模型直连

在小呆 **模型与密钥 → 模型设置** 选择「通过活动 Wi-Fi 或有线网卡直连」，保存并重载服务。点击对应服务商的 **测试网卡直连**，可以看到网卡名、实际使用的地址族和 HTTP 状态。例如 `en0 · IPv6 · HTTP 401` 表示请求通过 Wi-Fi 的 IPv6 到达服务商，服务商要求鉴权。探测无需 API Key。

## 1. 找到活动物理网卡

小呆按 macOS 的网络服务优先级逐项检查，同时接入 Wi-Fi 和有线网时选择首个符合条件的服务。

1. 从 SystemConfiguration 的 `Setup:/Network/Global/IPv4` 读取 `ServiceOrder`。VPN 开着时，普通默认路由可能指向 `utun*`。
2. 读取每个服务的 `Setup:/Network/Service/<服务 ID>/Interface`，取硬件类型为 `AirPort`（Wi-Fi）或 `Ethernet` 的 `DeviceName`。检查 `State:/Network/Interface/<设备名>/Link`，跳过明确报告断开的链路。
3. 检查同一服务的 `State:/Network/Service/<服务 ID>/IPv4` 和 `IPv6`。任一地址族具备匹配的接口名、地址和路由器，且 `if_nametoindex` 能取得接口索引，即可选用。DNS 从该服务的 `State:/Network/Service/<服务 ID>/DNS` 读取。

在项目根目录运行下面的命令，会直接调用小呆使用的选择器。需要 macOS Command Line Tools。输出的第一列是本次选中的网卡，第二列是该服务配置的 DNS 列表。换 Wi-Fi、插拔网线后重新运行。[网卡选择源码](../server/bot-server/src-tauri/src/harness/physical_network.m)

```sh
net_tmp=$(mktemp -d)
DEVELOPER_DIR=/Library/Developer/CommandLineTools /usr/bin/clang -fobjc-arc \
  -framework Foundation -framework SystemConfiguration \
  server/bot-server/src-tauri/src/harness/physical_network.m \
  -x objective-c - -o "$net_tmp/active-interface" <<'OBJC'
#include <net/if.h>
#include <stdio.h>
extern unsigned int espbot_active_interface(char *, size_t, char *, size_t);
int main(void) {
    char name[IF_NAMESIZE] = {0}, dns[512] = {0};
    if (!espbot_active_interface(name, sizeof name, dns, sizeof dns)) {
        fputs("未找到活动的 Wi-Fi 或以太网卡\n", stderr);
        return 1;
    }
    printf("%s %s\n", name, dns);
    return 0;
}
OBJC
"$net_tmp/active-interface"
```

## 2. 解析端点，绑定 HTTPS 连接

智谱国内 Coding Plan 的地址是 `https://open.bigmodel.cn/api/coding/paas/v4`。小呆合并系统解析结果与活动网卡 DNS、默认 DNS 返回的 `A`（IPv4）和 `AAAA`（IPv6）。如果网关提供 DNS 服务，可以使用它。此 Mac 实测网关未应答 DNS，Wi-Fi 配发的 DNS 返回了可达的智谱 IPv6 地址。DNS 查询本身仍遵循系统路由。[智谱官方端点](https://docs.bigmodel.cn/cn/coding-plan/tool/opencode)

在同一终端对照默认 DNS 和第一台网卡 DNS 的回答。若网卡列出了多台，可以逐台检查。CDN 会轮换地址，接入配置应始终保留服务商域名。

```sh
read -r iface network_dns <<EOF
$("$net_tmp/active-interface")
EOF
resolver=${network_dns%%,*}
host=open.bigmodel.cn
for family in AAAA A; do
  printf '\n默认 DNS，%s\n' "$family"
  /usr/bin/dig +time=1 +tries=2 +short "$host" "$family" || echo "默认 DNS 未应答"
  if [ -n "$resolver" ]; then
    printf '网卡 DNS %s，%s\n' "$resolver" "$family"
    /usr/bin/dig @"$resolver" +time=1 +tries=2 +short "$host" "$family" || echo "该网卡 DNS 未应答"
  fi
done
```

建立连接时，先取选中网卡的接口索引，在 TCP `connect` **之前**设置 socket 选项。IPv4 使用 `IPPROTO_IP / IP_BOUND_IF`，IPv6 使用 `IPPROTO_IPV6 / IPV6_BOUND_IF`。选择 IPv6 只决定地址族，绑定网卡才约束出口。TLS 的 SNI、证书校验和 HTTP 主机名都使用原始域名。[Apple IPv4 定义](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/netinet/in.h) · [Apple IPv6 定义](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/netinet6/in6.h)

小呆让托管 OpenCode 把模型请求交给本机转发器。转发器使用 `reqwest` 的 `.no_proxy().https_only(true).interface(&interface)`，直连失败时向调用方报错。智谱有多个候选地址时，新连接先在同一网卡上并发验证 TLS 握手，再向通过验证的地址发送一次模型请求。探测握手只交换 TLS 数据。服务会复用已建立的 HTTPS 连接，并在每次请求前重新检查活动网卡。[转发器实现](../server/bot-server/src-tauri/src/harness/direct.rs) · [reqwest 接口绑定](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html#method.interface)

## 3. 验证真正的 OpenCode 请求

在项目根目录运行两项本机联网测试。第一项检查端点并向聊天接口发请求，第二项启动隔离的官方 OpenCode，逐个验证三家模型。测试使用合成内容和故意无效的密钥。下面的程序路径适用于本项目这台 Mac。

```sh
DEVELOPER_DIR=/Library/Developer/CommandLineTools \
XIAODAI_TEST_OPENCODE="$HOME/Xiaodai_Server/engines/opencode-bin/opencode" \
cargo test --locked --offline --manifest-path server/bot-server/src-tauri/Cargo.toml \
  --lib harness::direct::tests::live_ -- --ignored --nocapture --test-threads=1
```

查看 `OpenCode →` 行里的 `interface`、`peer_ip` 和 `status`。当 TLS 校验通过并收到服务商的 401 或 403，说明聊天请求已直达上游。若 `peer_ip` 与 `status` 为空，则未取得上游响应，本机转发器的 502 也按失败记录。验证账号能否生成回答，需要有效密钥和实际模型输出。

2026-11-18 在这台 Mac 开启 VPN 时实测，智谱国内使用 `en0` 的 IPv6，DeepSeek 和千问使用 `en0` 的 IPv4。千问仅在隔离测试中启用。换网络后以新测试的对端和状态为准。[本机验收记录](review-network-speech-2026-11-18.md)
