# mailer-mcp

把 Mailer 的本地邮箱通过 **MCP（Model Context Protocol）** 暴露出去，让 Claude Desktop、IDE 或其他 Agent 直接检索、阅读和分析你的邮件。

传输是 stdio 上的换行分隔 JSON-RPC 2.0：stdout 只走协议帧，日志全部走 stderr。

工具与应用内助手完全同源（`mailer_core::tools`），共 8 个：

| 工具 | 作用 |
| --- | --- |
| `search_mail` | 按语义检索邮件，无嵌入索引时退化为关键词匹配 |
| `read_message` | 读取单封邮件的完整头部、附件列表与正文 |
| `list_accounts` | 列出账户（不含任何凭据） |
| `recent_mail` | 按时间倒序取最新邮件 |
| `analyze_mail` | 对一封邮件跑 AI 分类 |
| `remember` / `recall` | 写入 / 召回助手记忆 |
| `send_mail` | **只生成待确认草稿**，不碰 SMTP —— 发信必须由你在应用里确认 |

## 构建

```bash
cargo build -p mailer-mcp --release
# 产物：target/release/mailer-mcp
```

## 数据库

数据库路径取自 `argv[1]`，其次是环境变量 `MAILER_DB`。**文件必须已存在**：服务不会替你新建一个空库，否则客户端只会对着空邮箱一本正经地回答。

先运行一次 Mailer 应用完成初始化，然后使用它的 `mailer.db`：

| 平台 | 路径 |
| --- | --- |
| macOS | `~/Library/Application Support/com.mournink.mailer/mailer.db` |
| Windows | `%APPDATA%\com.mournink.mailer\mailer.db` |
| Linux | `~/.local/share/com.mournink.mailer/mailer.db` |

## 客户端配置

Claude Desktop 的 `claude_desktop_config.json`（其他 MCP 客户端字段名相同）：

```json
{
  "mcpServers": {
    "mailer": {
      "command": "/absolute/path/to/target/release/mailer-mcp",
      "args": ["/Users/you/Library/Application Support/com.mournink.mailer/mailer.db"],
      "env": {
        "RUST_LOG": "mailer_mcp=info"
      }
    }
  }
}
```

Windows 上 `command` 与 `args` 里的反斜杠需要转义（`"C:\\Users\\you\\..."`）。数据库也可以改用 `env` 里的 `MAILER_DB` 传入，效果相同。

## 排查

日志写在 stderr，MCP 客户端一般提供日志面板。默认级别 `mailer_mcp=info,mailer_core=warn`，用 `RUST_LOG` 覆盖。

手动跑一次握手：

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize"}' \
              '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | ./target/release/mailer-mcp ~/.local/share/com.mournink.mailer/mailer.db
```
