# OpenCode 2 在小呆中的能力清单

本表针对当前选择的 OpenCode 2.2.5，以及 Bot Server 的实际配置和桥接代码。
OpenCode 提供某个工具，不代表小呆已经开放、用户账号已配置或设备已经支持。

| 能力 | OpenCode 原生能力 | 当前 Bot Server 状态 |
| --- | --- | --- |
| 文件查看和查找 | read、glob、grep | 可在控制台选择启用、逐次确认或自主执行；目录范围由设置决定 |
| 新建和修改文件 | write、edit、patch | 通过 `edit` 权限接入；手动确认时显示 OpenCode 的实际差异预览 |
| 终端命令 | shell，前台和后台 | 可配置；Shell 的实际权限属于 Mac 用户账户，工作目录本身不是沙箱 |
| 搜索和网页 | websearch、webfetch、MCP | DeepSeek Search / Fetch 和 browser MCP 已配置；可启用原生 webfetch；工具连接不等于真实查询已验收 |
| 提出问题 | question 交互表单 | 已接入控制台问题 / 回答 / 跳过；本机模拟模型已验证 OpenCode 问答协议，尚未通过真实模型调用验收 |
| Skills | skill 按需加载本机已安装的技能说明 | 可配置；可用技能取决于 OpenCode 实际发现的技能 |
| 子代理 | subagent，可启动子会话 | 当前禁止，子会话的权限请求与取消尚未桥接 |
| Code Mode | execute，在 JavaScript 中组合工具 | 当前禁止；它可以直接发起网络请求，不能视为只会调用已批准工具 |
| 会话 | OpenCode 会话、历史、取消和删除 | 宿主已管理会话、取消和遗忘；未提供完整历史浏览 |
| Git 工作树 | 创建、列出、移除和会话迁移 | 未接入；移除工作树可能影响已有更改 |
| 图片生成 | 实验性图像工具 | 未配置图像模型，不能因为工具存在就显示为可用 |
| MCP 扩展 | 使用已安装的本地或远程 MCP 服务器 | 控制台可配置继承文件和服务名；默认 browser / deepseek-search，新服务调用逐次确认 |
| 定时 / 后台 | OpenCode 部分能力支持后台任务 | 小呆当前不自动启动对话或后台任务；不能将尚未配置的定时能力当成可用 |

## 设计边界

- 用户选择按 Mac 当前账户可访问的文件范围自主执行原生工作工具；页面操作仍逐次确认。系统隐私权限仍由 macOS 决定。
- 独立工作目录只定义默认路径。OpenCode 官方说明 shell 按本机用户权限执行，
  对命令中目录的推断是尽力而为，不能宣称工作目录阻止访问其他文件。
- 已登录的 Chrome 通过现有 browser MCP 接入，页面读取和操作需要控制台逐次确认。
- 机器人唤醒和身体行为仍由设备端本地 runtime 控制；OpenCode 不直接控制逐帧眼睛或电机。
- 需要系统授权的模型、云端接口或外部服务应如实显示配置 / 连接状态，不声称已安装。

2026-11-07 本机验证：Mac release App 已编译、签名并启动；browser 和 deepseek-search
显示已连接。隔离的 OpenCode 2.2.5 使用本机模拟模型完成了文件写入、修改预览 / 单次
批准、问题表单及回复。随后经用户授权进行的一次真实 Qwen 测试，在等待服务响应头时
超时，未创建测试文件；因此控制台与真实云模型的完整交互尚未通过验收，也没有发起真实搜索。

依据：[OpenCode V2 tools](https://opencode.ai/docs/tools/)、
[OpenCode V2 permissions](https://dev.opencode.ai/v2/docs/permissions/)、
[项目的 OpenCode 集成](mac-opencode.md)。
