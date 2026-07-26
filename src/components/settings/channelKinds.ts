/**
 * Display metadata for the four notification kinds, shared by the channel list
 * and the channel editor. The hints say where the credential comes from —
 * that is the part people get stuck on, not the form itself.
 */

import type { ChannelKind } from "../../lib/types";

export const KINDS: ChannelKind[] = ["telegram", "qqbot", "bark", "webhook"];

export const KIND_META: Record<
  ChannelKind,
  { label: string; icon: string; hint: string }
> = {
  telegram: {
    label: "Telegram",
    icon: "send",
    hint: "在 Telegram 中与 @BotFather 对话创建机器人即可获得 Bot Token；先给机器人发一条消息，再打开 api.telegram.org/bot<token>/getUpdates 就能看到 Chat ID。",
  },
  qqbot: {
    label: "QQ 机器人",
    icon: "bot",
    hint: "需自行部署 go-cqhttp / NapCat 等 OneBot v11 服务并开启 HTTP 接口，这里填写它的 HTTP 地址（如 http://127.0.0.1:5700）。",
  },
  bark: {
    label: "Bark",
    icon: "bell",
    hint: "iOS 上安装 Bark App，在 App 首页复制设备 Key 即可；使用自建服务器时再填写服务器地址。",
  },
  webhook: {
    label: "自定义 Webhook",
    icon: "link",
    hint: "任何能接收 POST 请求的地址，可自定义请求头与请求体模板，方便接入企业微信、飞书、Slack 等。",
  },
};
