# Miyu 扩展接口契约

本目录记录**已经实现**的扩展接口。每份文档先写现状(as-built),再单列 planned;
没落地的能力不许写成"已支持"。改接口先改文档,再改代码,最后跑门禁。

## 词汇

| 词 | 指什么 | 代码落点 |
|---|---|---|
| core | 自己就能当 agent 用的那部分:回合引擎、核心工具、模型客户端、状态库、daemon 运行时 | `src/agent` `src/tools`(核心件)`src/llm` `src/state` `src/runtime` |
| 子系统 | 挂进回合流水线多个点的内置扩展:记忆、人格提醒、情绪、语音(技能 09-24 起是插件) | `PersonaManifest.subsystems` |
| 工具插件 | 只往工具面加东西:内置 Rust 工具、scripts、MCP 服务器、skills | `PersonaManifest.plugins`、`tools::compose_registry` |
| 平台生命周期插件 | 挂在场所流水线(入站/触发/投递)上的 `PlatformPlugin` | `src/platforms/plugins` |
| 场所 | 入口:CLI、WebUI、QQ、shellhook、语音。只声明信任位与能否弹问题 | `tools::Surface` |
| 宿主端口 | 下层用上层能力的窄 trait,由拥有能力的层装入 | `crates/miyu-base/src/host_ports/ports.rs` |

scripts 是"接脚本的机制",被执行的脚本是"被安装的资源";MCP client 是"连服务器的机制",
外部 server 是"资源"。机制随 Miyu 编译,资源可装可卸;装了资源不等于拿到宿主信息。

## 每份契约都要写的字段

id 与来源(core / builtin / 外装)、生命周期与挂接点、输入输出、鉴权来源(宿主判定,
不由扩展自报)、超时/取消/重试、错误映射、缓存与热更新语义、可观察字段、兼容窗口、
最小验收命令。密钥、完整 `AppConfig`、`DaemonState`、数据库句柄一律不进接口。

## 文档

- [供应商模型目录](provider-catalog.md) —— `src/provider_catalog`
- [scripts](scripts.md) —— `src/tools/scripts`
- [MCP client](mcp-client.md) —— `crates/miyu-engine/src/tools/mcp.rs`
- [Miyu MCP server](mcp-server.md) —— `src/cli/mcp_serve.rs`
- [内置工具](builtin-tools.md) —— `ToolSpec` + `src/tools/descriptions/`
- [平台 hook](platform-hooks.md) —— `PlatformPlugin`
- [宿主能力端口](host-capabilities.md) —— `crates/miyu-base/src/host_ports/ports.rs` 等
- [子系统挂接表](subsystems.md) —— `crates/miyu-base/src/config/subsystems.rs`
- [兼容规则](compatibility.md)

## 门禁

- 依赖方向:`python3 test_scripts/arch_dep_check.py`。白名单(`test_scripts/arch-dep-waivers.json`)
  自 2026-09-16 起为空——任何跨层引用都是新增,直接红。
- 工具面字节:`cargo test --lib shape_tests`(三个场所的 tools 数组逐字节)、
  `cargo test --lib token_diet_baseline -- --ignored --nocapture`(量尺)。
- 全套安全网:`bash test_scripts/refactor-check.sh`。
