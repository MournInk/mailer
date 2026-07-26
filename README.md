# Mailer

一站式邮箱管理 App —— **Rust + Tauri 2** 实现，多端原生运行（Windows / macOS / Linux / iOS / Android）。

外观是一个正常的三栏邮件客户端；内核带一个 **AI 过滤器**：接入你自己的 LLM API 后，每封新邮件都会被自动识别分类，并按类型执行动作 —— 验证码直接弹窗展示、垃圾邮件静默或删除、账单等重要邮件弹窗并推送到 Telegram / QQ 机器人等多个信息源。

## 功能

- **多来源邮箱**：任意数量账户，支持 IMAP / POP3 收信、SMTP 发信，TLS / STARTTLS，内置 Gmail、Outlook、QQ 邮箱、网易 163、iCloud 等服务商预设
- **正常邮箱外观**：侧栏（账户 + 智能分类）/ 邮件列表 / 阅读窗格三栏布局，未读、星标、搜索、回复、删除一应俱全
- **AI 过滤器**（自行配置 OpenAI 兼容 API，本地调用，不经过任何中间服务器）：
  - `验证码` → 系统通知直接展示验证码，应用内弹窗一键复制
  - `垃圾邮件` → 静默归类；模型判定“特别没用”的可自动删除（需在设置中显式开启）
  - `普通邮件` → 静默存档
  - `重要邮件`（账单 / 安全告警等）→ 系统弹窗提示 + 推送到所有已配置的通知渠道
- **多信息源通知**：Telegram Bot、QQ 机器人（OneBot v11：go-cqhttp / NapCat / Lagrange）、Bark（iOS）、通用 Webhook；每个渠道可独立选择要推送的邮件类型
- **本地优先**：邮件与配置存于本地 SQLite；LLM API 只在你配置后才会被调用

## 技术栈

| 层 | 技术 |
| --- | --- |
| 核心引擎 | Rust（`crates/mailer-core`）：IMAP（async-imap）、POP3（手写 RFC 1939 客户端）、SMTP（lettre）、MIME 解析（mail-parser）、SQLite（rusqlite）、全链路 rustls |
| 应用壳 | Tauri 2（`src-tauri`）：桌面 + 移动端入口、系统通知、IPC 命令 |
| 前端 | React + TypeScript + Vite，自研 “Paper & Ink” 设计系统（参考 Anthropic/Claude、Stripe、OpenAI、ElevenLabs、Grok 的设计语言），亮/暗双主题 |

## 开发

```bash
# 依赖：Rust 1.85+、Node 20+，Linux 桌面需 webkit2gtk-4.1 / gtk3 开发库
npm install

# 桌面端开发
npm run tauri dev

# 桌面端构建
npm run tauri build

# 核心逻辑测试
cargo test -p mailer-core
```

### 移动端

项目结构已按 Tauri 2 移动端要求组织（`src-tauri` 为 lib crate，含 `mobile_entry_point`）。在装有对应 SDK 的机器上：

```bash
# Android（需 Android SDK/NDK）
npm run tauri android init
npm run tauri android dev

# iOS（需 macOS + Xcode）
npm run tauri ios init
npm run tauri ios dev
```

## AI 过滤器配置

设置 → AI 过滤器：

| 字段 | 说明 |
| --- | --- |
| API Base | OpenAI 兼容端点，如 `https://api.openai.com/v1`、`https://api.deepseek.com/v1`，或本地 Ollama `http://127.0.0.1:11434/v1` |
| API Key | 仅存储在本地 SQLite |
| 模型 | 如 `gpt-4o-mini`、`deepseek-chat`、`qwen2.5:7b` |
| 自动删除垃圾邮件 | 默认关闭；开启后模型标记为 `deletable` 的垃圾邮件会被本地+服务器删除 |

分类结果为四类之一：`verification` / `spam` / `normal` / `important`，模型同时产出一行摘要、验证码（如有）与置信度。

## 通知渠道配置

| 渠道 | 配置 |
| --- | --- |
| Telegram | Bot Token（@BotFather 获取）+ Chat ID |
| QQ 机器人 | OneBot v11 HTTP 地址（如 go-cqhttp / NapCat 的 `http://127.0.0.1:3000`）+ 私聊/群 + 目标号码 |
| Bark | 设备 Key（iOS 推送） |
| Webhook | 任意 URL，POST JSON 载荷 |

## 目录结构

```
crates/mailer-core/   # 纯 Rust 核心：协议、AI、通知、存储、同步编排（可独立测试）
src-tauri/            # Tauri 2 壳：窗口、系统通知、IPC 命令
src/                  # React 前端：三栏邮件 UI + 设置
```

## License

MIT
