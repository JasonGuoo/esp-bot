# browser 与 deepseek-search

优先让 OpenCode 自己加载 Mac 上现有的 MCP 配置。小呆的 provider / permission
覆盖文件不覆盖 `mcp.servers`，所以模型切换不会另建一套工具。

没有现成配置时，仓库提供
[`../examples/opencode-tools.example.jsonc`](../examples/opencode-tools.example.jsonc)。
它使用 OpenCode **v2** 的 `mcp.servers`，不是旧版的平铺 `mcp` 结构。
只把缺少的条目合并到自己的 OpenCode 配置，或将其绝对路径设置到
`brain.json → harness.inherit_config`。把示例绝对路径替换为本机实际路径。
不要直接覆盖已有 browser / deepseek-search，也不要把 API key 写入 git。

## 官方 browser

复用 [Microsoft Playwright MCP](https://github.com/microsoft/playwright-mcp/tree/v0.0.84)，
示例固定版本 **0.0.84** 并使用 `--cdp-endpoint=chrome --no-webmcp`，连接用户选择的
当前 Chrome，会共用已登录会话。Chrome 需要用户先在
`chrome://inspect/#remote-debugging` 开启本实例的远程调试，并在连接请求时亲自批准。
小呆每次读取页面或执行浏览器操作均需在控制台逐次批准。

本机当前复用已缓存的 Microsoft Playwright MCP **0.0.83**，复制到项目的
`runtime/tooling/`；配置位于 `runtime/opencode-local-tools.json`，不含密钥。
它依赖本机 Node 和 Chrome。若项目运行目录被清理，需要重新准备依赖及配置。

当前 Chrome 中的页面和登录状态可能包含敏感信息。网站仍可能重定向、下载、提交表单；
这些能力由 OpenCode 小呆 agent 的动作权限与家长一次性批准管理。
不要添加 `--no-sandbox`，不要把文件系统 / shell 权限放给孩子侧 agent。
官方 profile 说明：[Playwright user profile](https://playwright.dev/mcp/configuration/user-profile)。

## DeepSeek 官方 Search / Fetch

`deepseek_search.py` 是一个小的 MCP 适配器：MCP 传输、工具声明和取消协议由
[官方 Python SDK 2.3.0](https://github.com/modelcontextprotocol/python-sdk/releases/tag/v2.3.0)
处理；项目代码只把 `search` / `fetch` 映射到官方 HTTP API。
SDK v2 使用 `MCPServer`，没有复制旧 FastMCP 的协议实现。

```bash
cd server/bot-server
python3.11 -m venv .venv-tools
.venv-tools/bin/python -m pip install -r tools/requirements.txt
```

托管模式启动时，服务会把控制台保存的 DeepSeek Key 作为 `DEEPSEEK_API_KEY`
传给 OpenCode 及其本地工具；DeepSeek 模型路线关闭时也可用于搜索。
适配器也接受显式的 `DEEPSEEK_SEARCH_API_KEY`。密钥不写入 MCP 配置。
适配器不接受模型指定 API 地址，不跟随 API 重定向，
不传 Cookie、不读取家庭数据库、不自动重试付费请求。查询和目标 URL 会发给
DeepSeek，因此系统提示要求排除孩子姓名、家庭记忆和身份信息。

| 工具 | 官方端点 | 应用限制 |
| --- | --- | --- |
| `deepseek-search_search` | `POST https://api.deepseek.com/v1/tools/search` | 查询 1–128 字符；最多 10 条结果；返回真实来源 URL |
| `deepseek-search_fetch` | `POST https://api.deepseek.com/v1/tools/fetch` | 公共 HTTPS URL；无用户名密码；正文最多返回 24,000 字符 |

官方搜索支持 `site:`、`lang:`、`after:` 等语法。适配器保留原查询，不把模型写出的
普通答案伪装成搜索结果。`402` 是余额不足，`429` 是速率限制，二者独立于 Qwen / GLM
套餐额度。搜索与网页获取按 DeepSeek 自己的规则计费；它们不消耗 Coding Plan 的免费额度。

来源：[Search & Fetch 官方指南](https://api-docs.deepseek.com/guides/search/)、
[Search 请求规范](https://api-docs.deepseek.com/api/search/)、
[Fetch 请求规范](https://api-docs.deepseek.com/api/fetch/)、
[MCP v2 官方最小示例](https://py.sdk.modelcontextprotocol.io/)。

本机检查和授权状态见 [Mac OpenCode 说明](../../../docs/mac-opencode.md)。
