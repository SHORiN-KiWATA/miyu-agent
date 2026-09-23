---
name: bilibili-live
display_name: 哔哩哔哩直播控制
summary: 改直播标题分区、开播拿推流码、下播
description: Control a Bilibili live room. Use when the user says 开播、下播、关播、改直播标题、换分区、推流码、RTMP、OBS、看直播间状态、b站直播.
---

# 哔哩哔哩直播间控制

脚本不在常驻工具面上，经工具桥调：

```bash
miyu tool-call bilibili_live_stream --stdin <<'JSON'
{"command": "status"}
JSON
```

`command` 必填，其余可省。完整参数用 `miyu tool-call bilibili_live_stream --describe` 现取。

## 七个命令

| command | 作用 | 要登录 |
|---|---|---|
| `status` | 读直播间当前状态 | 是 |
| `areas` | 列分区；`search` 按关键词、`parent` 列某父分区的子分区；`refresh` 跳过一天的本地缓存 | 否 |
| `update` | 原子地改标题 / 分区（`title`、`area`） | 是 |
| `start` | 开播，返回 RTMP 地址与推流码 | 是 |
| `stop` | 下播 | 是 |
| `login` / `logout` | 二维码登录（PNG 路径在返回里）/ 退出 | — |

加 `{"json": true}` 拿完整 JSON，默认是省 token 的文字摘要。

## 三条硬规矩

1. **`start` 必须先拿到用户明确同意**。它会让直播间**立刻公开**并给所有粉丝推送开播通知，没法悄悄撤销。用户只说"准备一下"「改个标题」不等于要开播。
2. **`update` 在直播中是观众立刻可见的**。改标题分区前先确认是不是正在播。
3. 分区名可以写中文名、拼音或 `父分区/子分区`，脚本自己解析成 id。**有歧义时会失败**，候选在 `error.detail.candidates` 里——把候选摆给用户挑，别自己猜一个。

## 没登录怎么办

返回 `kind=auth` 就是没登录。跑 `{"command":"login"}`，它会把二维码写成 PNG 并返回路径，让用户用手机 b 站扫。扫完再重试原来那条命令。
